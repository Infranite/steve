//! Local send permission recovery through the existing reviewable, undoable changeset path.
//! External service policies have no management contract here; this never clears their refusal
//! cache, authorizes a remote credential, or sends a message.

use crate::flow_actions::LateChannels;
use crate::{Channels, bindings, spaces};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::sync::Arc;
use steve_modules::accounts::connections::repo::{self, ConnRow};
use steve_modules::changes::{self, ChangeOp, DraftTarget, OpCx, Plan, Prepared};
use steve_modules::{ApiError, ApiResult, Caller, Registry};
use steve_store::Tx;
use steve_store::base::{self, Change, Viewer};

const KIND: &str = "channel_send_permission";
const OP: &str = "chan.send_permission.enable";

pub fn register(registry: &mut Registry, late: Arc<LateChannels>) {
    registry
        .target(KIND, Box::new(Permission(late.clone())))
        .op(OP, Box::new(Permission(late)));
}

struct Permission(Arc<LateChannels>);

fn own(c: &Connection, viewer: &Viewer, id: &str) -> ApiResult<ConnRow> {
    crate::channel_accounts::can_manage(c, viewer, id)?;
    let row = repo::get(c, id)?.ok_or_else(|| ApiError::not_found("连接"))?;
    if bindings::connection_account(c, &viewer.ws, id)?.1 != spaces::Mode::Own
        || row.status == "revoked"
    {
        return Err(ApiError::rule("这不是可恢复本地代发能力的个人连接"));
    }
    Ok(row)
}

impl Permission {
    fn available(&self, row: &ConnRow) -> ApiResult<()> {
        let ch = self.0.get().ok_or_else(|| ApiError::rule("渠道尚未就绪"))?;
        let provider = ch
            .connections()
            .provider(&row.adapter)
            .ok_or_else(|| ApiError::rule("渠道服务不可用"))?;
        if !provider
            .available_caps(&row.granted_scopes)
            .iter()
            .any(|c| c == spaces::SEND_AS_USER_CAP)
        {
            return Err(ApiError::rule(
                "平台尚未授予代发权限，请在接入里按服务要求补齐权限并重新授权",
            ));
        }
        Ok(())
    }
}

impl DraftTarget for Permission {
    fn prepare(
        &self,
        store: &steve_store::Store,
        _r: &mut steve_store::R<'_>,
        viewer: &Viewer,
        id: Option<&str>,
    ) -> ApiResult<Prepared> {
        store.reader(|c| {
            let id = id.ok_or_else(|| ApiError::invalid("target.id"))?;
            let row = own(c, viewer, id)?;
            self.available(&row)?;
            Ok(Prepared {
                target_id: Some(id.into()),
                revision: Some(row.revision),
                title: "允许这个连接以你本人发送（不会发送任何消息）".into(),
                href: format!("#/channels/acc/{id}"),
                subject: "local send-as-user capability".into(),
                fields: vec![],
                ops: vec![],
                free_text: None,
                context: Value::Null,
            })
        })
    }
    fn plan(
        &self,
        _store: &steve_store::Store,
        _r: &mut steve_store::R<'_>,
        _: &Viewer,
        _: &Prepared,
        _: Vec<Value>,
    ) -> ApiResult<Plan> {
        // Only the deterministic recovery endpoint creates this narrowly scoped proposal.
        Err(ApiError::rule("请从失败消息创建授权改动"))
    }
    fn revision(
        &self,
        store: &steve_store::Store,
        _r: &mut steve_store::W<'_>,
        ws: &str,
        id: &str,
    ) -> ApiResult<Option<i64>> {
        store.reader(|c| {
            Ok(repo::get(c, id)?
                .filter(|r| r.workspace_id == ws)
                .map(|r| r.revision))
        })
    }
}

impl ChangeOp for Permission {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let viewer = Viewer::bare(cx.ws, cx.actor);
        let row = own(c, &viewer, op["connection_id"].as_str().unwrap_or(""))?;
        self.available(&row)
    }
    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        self.validate(tx, cx, op)?;
        let mut row = own(
            tx,
            &Viewer::bare(cx.ws, cx.actor),
            op["connection_id"].as_str().unwrap_or(""),
        )?;
        let enabled = row.caps.iter().any(|c| c == spaces::SEND_AS_USER_CAP);
        if !enabled {
            row.caps.push(spaces::SEND_AS_USER_CAP.into());
            repo::update(tx, &row, steve_store::now())?;
            repo::set_enabled_at(tx, &row.id, steve_store::now())?;
            tx.publish(steve_types::DomainEvent::from(
                steve_types::ChannelsEvent::Connection {
                    id: row.id.clone().into(),
                    ws: cx.ws.into(),
                },
            ));
            base::audit(
                tx,
                cx.ws,
                cx.actor,
                "connection.send_permission",
                &row.id,
                &json!({"changeset":cx.changeset_id,"capability":spaces::SEND_AS_USER_CAP,"enabled":true}),
            )?;
        }
        Ok(json!({"enabled": enabled, "written_revision": row.revision + i64::from(!enabled)}))
    }
    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let mut row = own(
            tx,
            &Viewer::bare(cx.ws, cx.actor),
            op["connection_id"].as_str().unwrap_or(""),
        )?;
        if before["enabled"] == false {
            if before["written_revision"].as_i64() != Some(row.revision) {
                cx.keep("连接已再次修改，保留当前代发能力".into());
                return Ok(());
            }
            row.caps.retain(|c| c != spaces::SEND_AS_USER_CAP);
            repo::update(tx, &row, steve_store::now())?;
            tx.publish(steve_types::DomainEvent::from(
                steve_types::ChannelsEvent::Connection {
                    id: row.id.clone().into(),
                    ws: cx.ws.into(),
                },
            ));
            base::audit(
                tx,
                cx.ws,
                cx.actor,
                "connection.send_permission.undo",
                &row.id,
                &json!({"changeset":cx.changeset_id,"enabled":false}),
            )?;
        }
        Ok(())
    }
}

/// `POST /chan-messages/:id/authorization`: creates a proposal, never applies it or sends.
/// A service refusal is deliberately rejected here even if a local permission is also disabled.
pub fn propose(ch: &Channels, who: &Caller, message: &str) -> ApiResult<changes::Proposal> {
    let viewer = ch.modules().viewer(who)?;
    let (prepared, plan) = ch.store().reader(|c| -> ApiResult<_> {
        let (space, status, decision): (String, String, Option<String>) = c.query_row(
            "SELECT space_id,status,decision_id FROM chan_messages WHERE id=? AND workspace_id=? AND direction='out'",
            params![message, who.ws], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).map_err(|_| ApiError::not_found("消息"))?;
        spaces::own(c, &viewer, &space)?;
        let d = decision.and_then(|id| base::decisions::get(c, &id).ok().flatten())
            .filter(|d| status == "failed" && d.status == "open" && d.result["recovery"]["kind"] == "missing_permission" && d.result["recovery"]["authorization_kind"] == "managed")
            .ok_or_else(|| ApiError::rule("这条拒绝需在渠道服务侧处理；Steve 没有它的授权管理 API"))?;
        let conn = d.result["recovery"]["connection_id"].as_str().unwrap_or("");
        let row = own(c, &viewer, conn)?;
        let provider = ch.connections().provider(&row.adapter).ok_or_else(|| ApiError::rule("渠道服务不可用"))?;
        if row.caps.iter().any(|c| c == spaces::SEND_AS_USER_CAP)
            || !provider.available_caps(&row.granted_scopes).iter().any(|c| c == spaces::SEND_AS_USER_CAP) {
            return Err(ApiError::rule("本地代发能力无需恢复或平台尚未授权；请到接入核对平台权限"));
        }
        let prepared = Prepared {
            target_id: Some(conn.into()), revision: Some(row.revision),
            title: "允许这个连接以你本人发送（不会发送任何消息）".into(), href: format!("#/channels/acc/{conn}"),
            subject: String::new(), fields: vec![], ops: vec![], free_text: None, context: Value::Null,
        };
        Ok((prepared, Plan {
            ops: vec![json!({"op": OP,"connection_id":conn})],
            changes: vec![Change::new("本地代发能力 msg.send_as_user（整个连接）", "关闭", "开启；平台授权及每条消息的审批仍分别校验；可撤销")],
        }))
    })?;
    changes::store_plan(
        ch.modules(),
        who,
        KIND,
        &prepared,
        plan,
        changes::Origin {
            origin: "steve".into(),
            basis: format!("渠道失败消息 {message}；仅恢复本地能力，不发送消息"),
        },
    )
}
