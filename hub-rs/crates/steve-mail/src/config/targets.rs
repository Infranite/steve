//! Conversational configuration of mailboxes: the draft target `mail_account`
//! (`mail_account:<connection_id>`) with the fields `signature` (text), `duty` (text, appended to),
//! `send_delay_s` (0/10/30/60 秒), `archive_on_done` (是/否) and `sync_days` (7/30/90 天), and one
//! operation that is `PATCH /mail/accounts/:connection_id` inside the changeset's transaction and
//! undoes to the settings the mailbox had.
//!
//! The operation is called `mail.account.patch` (§10 writes `mail_account.patch`): every op of the
//! mail module starts with `mail.`, so it cannot collide in `Registry::op`.
//! Flow-like requests (“GitHub 的通知邮件以后都忽略”) stay with the `flow` target of steve-flow.

use rusqlite::Connection;
use serde_json::{Map, Value, json};
use steve_modules::accounts::connections::repo::{self as conn_repo, ConnRow};
use steve_modules::accounts::service::{current_party, own_account};
use steve_modules::changes::{Candidate, ChangeOp, DraftTarget, FieldSpec, OpCx, Plan, Prepared};
use steve_modules::{ApiError, ApiResult, Registry};
use steve_store::base::{Change, Viewer, show};
use steve_store::{Tx, now};

use crate::repo::{self, AccountSettings};
use crate::settings::{self, SettingsPatch};

/// `target.kind` of `POST /api/config/draft`.
pub const KIND: &str = "mail_account";
/// The one operation of the target.
pub const OP_PATCH: &str = "mail.account.patch";

const UNRECOGNIZED: &str = "（没认出）";
const DELAYS: [i64; 4] = [0, 10, 30, 60];
const DAYS: [i64; 3] = [7, 30, 90];
const NOUN: &str = "连接";

/// Registers the target and its operation (integrator, next to the other module targets).
pub fn register(r: &mut Registry) {
    r.target(KIND, Box::new(MailAccountTarget))
        .op(OP_PATCH, Box::new(PatchOp));
}

fn yes_no(b: bool) -> &'static str {
    if b { "是" } else { "否" }
}

fn seconds(n: i64) -> String {
    format!("{n} 秒")
}

fn days(n: i64) -> String {
    format!("{n} 天")
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "是" | "开" | "启用" | "开启" => Some(true),
        "false" | "否" | "关" | "停用" | "关闭" => Some(false),
        _ => None,
    }
}

/// `"30"`, `"30 秒"`, `"30秒"`, `"30 天"` → 30, if it is one of `allowed`.
fn parse_choice(s: &str, unit: &str, allowed: &[i64]) -> Option<i64> {
    let n: i64 = s.trim().trim_end_matches(unit).trim().parse().ok()?;
    allowed.contains(&n).then_some(n)
}

/// The text after appending `new` to `old` (a reply that repeats the old text replaces it).
fn appended(old: &str, new: &str) -> String {
    let new = new.trim();
    if old.is_empty() || new.starts_with(old) {
        new.to_string()
    } else {
        format!("{old}\n{new}")
    }
}

/// A mailbox `viewer` may configure: a connection of the workspace with `mail.read` whose account
/// they see and whose owner (if a person) is them — the same rule as `Connections::get`.
pub fn own_mailbox(c: &Connection, viewer: &Viewer, id: &str) -> ApiResult<ConnRow> {
    let r = conn_repo::get(c, id)?
        .filter(|r| r.workspace_id == viewer.ws && r.caps.iter().any(|x| x == "mail.read"))
        .ok_or_else(|| ApiError::not_found(NOUN))?;
    let a = own_account(c, viewer, &r.account_id).map_err(|_| ApiError::not_found(NOUN))?;
    let mine = current_party(c, &a.party_id)?
        .and_then(|p| p.user_id)
        .is_none_or(|u| u == viewer.user);
    if mine {
        Ok(r)
    } else {
        Err(ApiError::not_found(NOUN))
    }
}

fn address(r: &ConnRow) -> String {
    r.config
        .get("address")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn current(c: &Connection, id: &str) -> ApiResult<AccountSettings> {
    repo::settings(c, id)
        .map_err(|e| ApiError::Internal(e.to_string()))
        .map(Option::unwrap_or_default)
}

struct MailAccountTarget;

impl DraftTarget for MailAccountTarget {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let id = id.ok_or_else(|| ApiError::not_found(NOUN))?;
        let r = own_mailbox(c, viewer, id)?;
        let s = current(c, id)?;
        let addr = address(&r);
        let delays: Vec<String> = DELAYS.iter().map(|d| seconds(*d)).collect();
        let day_values: Vec<String> = DAYS.iter().map(|d| days(*d)).collect();
        Ok(Prepared {
            target_id: Some(r.id.clone()),
            revision: Some(s.revision),
            title: format!("邮箱 {addr} 的设置"),
            href: format!("#/accounts/{}", r.account_id),
            subject: format!("the mailbox \"{addr}\""),
            fields: vec![
                FieldSpec::new("signature", "签名", "text", json!(s.signature)),
                FieldSpec::new(
                    "duty",
                    "职责（这个邮箱的写信习惯，追加）",
                    "text",
                    json!(s.duty),
                ),
                FieldSpec::new(
                    "send_delay_s",
                    "发送延迟（批准后等多久再发，期间可以撤销）",
                    "enum",
                    json!(seconds(s.send_delay_s)),
                )
                .values(&delays.iter().map(String::as_str).collect::<Vec<_>>()),
                FieldSpec::new(
                    "archive_on_done",
                    "完成后归档",
                    "enum",
                    json!(yes_no(s.archive_on_done)),
                )
                .values(&["是", "否"]),
                FieldSpec::new("sync_days", "回溯天数", "enum", json!(days(s.sync_days)))
                    .values(&day_values.iter().map(String::as_str).collect::<Vec<_>>()),
            ],
            ops: vec![],
            free_text: Some("duty".into()),
            context: Value::Null,
        })
    }

    fn plan(
        &self,
        c: &Connection,
        viewer: &Viewer,
        p: &Prepared,
        raw: Vec<Value>,
    ) -> ApiResult<Plan> {
        let id = p.target_id.as_deref().unwrap_or("");
        own_mailbox(c, viewer, id)?;
        let cur = current(c, id)?;
        let mut next = cur.clone();
        let mut plan = Plan::default();
        let mut unknown = Vec::new();
        for op in raw {
            if op["op"] != "set" {
                unknown.push(Change::new(UNRECOGNIZED, "—", show(&op.to_string())));
                continue;
            }
            let key = op["key"].as_str().unwrap_or("");
            let said = text_of(&op["value"]).unwrap_or_default();
            let bad = |label: &str| Change::new(label, "—", format!("（没认出：{}）", show(&said)));
            match key {
                "signature" if said.chars().count() <= 1000 => next.signature = said,
                "duty" if !said.is_empty() => {
                    let d = appended(&next.duty, &said);
                    if d.chars().count() <= 4000 {
                        next.duty = d;
                    } else {
                        unknown.push(bad("职责"));
                    }
                }
                "send_delay_s" => match parse_choice(&said, "秒", &DELAYS) {
                    Some(n) => next.send_delay_s = n,
                    None => unknown.push(bad("发送延迟")),
                },
                "archive_on_done" => match parse_bool(&said) {
                    Some(b) => next.archive_on_done = b,
                    None => unknown.push(bad("完成后归档")),
                },
                "sync_days" => match parse_choice(&said, "天", &DAYS) {
                    Some(n) => next.sync_days = n,
                    None => unknown.push(bad("回溯天数")),
                },
                "signature" => unknown.push(bad("签名")),
                "duty" => {}
                _ => unknown.push(Change::new(
                    UNRECOGNIZED,
                    "—",
                    show(&format!("{key}: {said}")),
                )),
            }
        }
        // one line and one patch field per changed setting, in the order of the form
        let mut patch = Map::new();
        if next.signature != cur.signature {
            plan.changes.push(Change::new(
                "签名",
                show(&cur.signature),
                show(&next.signature),
            ));
            patch.insert("signature".into(), json!(next.signature));
        }
        if next.duty != cur.duty {
            let added = next
                .duty
                .strip_prefix(cur.duty.as_str())
                .unwrap_or(&next.duty)
                .trim();
            plan.changes
                .push(Change::new("职责（追加）", show(&cur.duty), show(added)));
            patch.insert("duty".into(), json!(next.duty));
        }
        if next.send_delay_s != cur.send_delay_s {
            plan.changes.push(Change::new(
                "发送延迟",
                seconds(cur.send_delay_s),
                seconds(next.send_delay_s),
            ));
            patch.insert("send_delay_s".into(), json!(next.send_delay_s));
        }
        if next.archive_on_done != cur.archive_on_done {
            plan.changes.push(Change::new(
                "完成后归档",
                yes_no(cur.archive_on_done),
                yes_no(next.archive_on_done),
            ));
            patch.insert("archive_on_done".into(), json!(next.archive_on_done));
        }
        if next.sync_days != cur.sync_days {
            plan.changes.push(Change::new(
                "回溯天数",
                days(cur.sync_days),
                days(next.sync_days),
            ));
            patch.insert("sync_days".into(), json!(next.sync_days));
        }
        plan.changes.extend(unknown);
        if !patch.is_empty() {
            plan.ops
                .push(json!({ "op": OP_PATCH, "connection_id": id, "patch": patch }));
        }
        Ok(plan)
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        let Some(r) = conn_repo::get(c, id)?.filter(|r| r.workspace_id == ws) else {
            return Ok(None);
        };
        Ok(Some(current(c, &r.id)?.revision))
    }

    fn candidates(&self, c: &Connection, viewer: &Viewer) -> ApiResult<Vec<Candidate>> {
        let mut out = Vec::new();
        for r in conn_repo::list(c, &viewer.ws)? {
            if r.status == "revoked" || own_mailbox(c, viewer, &r.id).is_err() {
                continue;
            }
            out.push(Candidate {
                code: format!("{KIND}:{}", r.id),
                label: format!("邮箱 {}", address(&r)),
            });
        }
        Ok(out)
    }
}

struct PatchOp;

fn parse_patch(op: &Value) -> ApiResult<SettingsPatch> {
    serde_json::from_value(op["patch"].clone()).map_err(|_| ApiError::invalid("patch"))
}

fn conn_of(op: &Value) -> &str {
    op["connection_id"].as_str().unwrap_or("")
}

fn check(c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
    let viewer = Viewer::load(c, cx.ws, cx.actor)?;
    own_mailbox(c, &viewer, conn_of(op))?;
    Ok(())
}

impl ChangeOp for PatchOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        check(c, cx, op)?;
        parse_patch(op).map(|_| ())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let id = conn_of(op);
        check(tx, cx, op)?;
        let before = current(tx, id)?;
        settings::patch_tx(tx, cx.ws, cx.actor, id, &parse_patch(op)?, now()).map_err(api)?;
        Ok(settings::fields(&before))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let id = conn_of(op);
        let cur = current(tx, id)?;
        let mut back = cur.clone();
        if let Some(v) = before["signature"].as_str() {
            back.signature = v.into();
        }
        if let Some(v) = before["duty"].as_str() {
            back.duty = v.into();
        }
        if let Some(v) = before["send_delay_s"].as_i64() {
            back.send_delay_s = v;
        }
        if let Some(v) = before["archive_on_done"].as_bool() {
            back.archive_on_done = v;
        }
        if let Some(v) = before["sync_days"].as_i64() {
            back.sync_days = v;
        }
        if let Some(v) = before["append_sent"].as_str() {
            back.append_sent = v.into();
        }
        if back == cur {
            return Ok(());
        }
        settings::write_tx(tx, cx.ws, cx.actor, id, &cur, &back, now()).map_err(api)
    }
}

fn api(e: crate::MailError) -> ApiError {
    match e {
        crate::MailError::Api(a) => a,
        e => ApiError::Internal(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_choices_and_appends() {
        assert_eq!(parse_choice("30 秒", "秒", &DELAYS), Some(30));
        assert_eq!(parse_choice("0", "秒", &DELAYS), Some(0));
        assert_eq!(parse_choice("45 秒", "秒", &DELAYS), None);
        assert_eq!(parse_choice("90天", "天", &DAYS), Some(90));
        assert_eq!(appended("", " 先给结论 "), "先给结论");
        assert_eq!(appended("用中文", "先给结论"), "用中文\n先给结论");
        assert_eq!(appended("用中文", "用中文\n先给结论"), "用中文\n先给结论");
    }
}
