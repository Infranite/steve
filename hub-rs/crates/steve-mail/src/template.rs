//! The flow template “邮件分诊” (email.md §4, prototype r1), installed enabled when the first mailbox
//! of a workspace connects (`origin=template:mail-triage`).
//!
//! ```text
//! t trigger(mail, types=[mail.new])
//! └ f 排除通知类 ── 通知类 → d digest                        (before any model call: no cost)
//!   └ 其余 → l item.link(by=thread, fallback=none)
//!            └ k 这串往来已有事项 ── 已补充 → (end, outcome linked)
//!              └ 新的往来 → b bot(源的 triage_bot)
//!                           ├ 需要回复       → r mail.draft_reply      (its send approval is the only card)
//!                           ├ 有截止或涉及钱 → i item.create {mode:"start"}
//!                           └ 其他           → o digest
//! ```
//! The `item.link` step is how later mail of the same thread reaches the item made from the first
//! one (§4 “同一往来后续的邮件由 item.link(by=thread) 补充到事项”) without asking the model again.
//!
//! Integrator: call [`install`] after a mailbox connection is created (with the connection's id and
//! creator). It is a no-op once the workspace has (or had, deleted) a flow of this origin.

use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use steve_flow::flows::{self, NewFlow};
use steve_flow::{FlowCx, FlowResult, graph, sources};
use steve_modules::ApiError;

use crate::nodes::DRAFT_REPLY;

pub const ORIGIN: &str = "template:mail-triage";
pub const NAME: &str = "邮件分诊";
pub const DESCRIPTION: &str = "需要回复的起草等你确认，涉及钱和截止时间的建事项，通知类进汇报";
/// The bot the bot node defaults to (the source's “先交给谁判断” is set to it when unset).
pub const MAIL_BOT: &str = "mail";

pub const BRANCH_REPLY: &str = "需要回复";
pub const BRANCH_MONEY: &str = "有截止或涉及钱";
pub const BRANCH_OTHER: &str = "其他";

/// Senders treated as notifications even without bulk headers.
pub const NOREPLY: &str = "(?i)^(no-?reply|do-?not-?reply|notifications?|notify|alerts?|mailer-daemon|postmaster|bounces?)([+._-][^@]*)?@";

/// The template's graph.
pub fn graph() -> Value {
    let notice =
        format!("event.payload.bulk == true or event.payload.from.addr matches '{NOREPLY}'");
    json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "title": "邮件 · 收到新邮件", "note": "所有邮箱 · 收件箱",
             "config": {"source": "mail", "types": ["mail.new"]}},
            {"id": "f", "kind": "filter", "title": "排除通知类", "note": "群发、订阅、noreply 发件人",
             "config": {"rules": [{"label": "通知类", "when": notice}], "else_label": "其余"}},
            {"id": "d", "kind": "action", "title": "进汇报", "note": "通知类不调用模型",
             "config": {"action": "digest"}},
            {"id": "l", "kind": "action", "title": "补充到已有事项", "note": "同一往来已经建过事项",
             "config": {"action": "item.link", "by": "thread", "fallback": "none"}},
            {"id": "k", "kind": "filter", "title": "这串往来已有事项",
             "config": {"rules": [{"label": "已补充", "when": "steps.l.found == true"}], "else_label": "新的往来"}},
            {"id": "b", "kind": "bot", "title": "邮件 Bot 判断", "note": "需要回复 / 有截止或涉及钱 / 其他",
             "config": {
                "instruction": "这封邮件要怎么处理？需要我亲自回信的选「需要回复」；有截止时间或涉及钱（付款、报价、发票、合同、报销），需要专门去办的选「有截止或涉及钱」；其余选「其他」。",
                "branches": [BRANCH_REPLY, BRANCH_MONEY, BRANCH_OTHER]}},
            {"id": "r", "kind": "action", "title": "起草回复，等你确认", "note": "发送需你批准",
             "config": {"action": DRAFT_REPLY}},
            {"id": "i", "kind": "action", "title": "建事项", "note": "建事项并开始",
             "config": {"action": "item.create", "mode": "start"}},
            {"id": "o", "kind": "action", "title": "进汇报", "note": "离开期间汇报",
             "config": {"action": "digest"}}
        ],
        "edges": [
            {"from": "t", "to": "f", "label": ""},
            {"from": "f", "to": "d", "label": "通知类"},
            {"from": "f", "to": "l", "label": "其余"},
            {"from": "l", "to": "k", "label": ""},
            {"from": "k", "to": "b", "label": "新的往来"},
            {"from": "b", "to": "r", "label": BRANCH_REPLY},
            {"from": "b", "to": "i", "label": BRANCH_MONEY},
            {"from": "b", "to": "o", "label": BRANCH_OTHER}
        ]
    })
}

/// Installs the template in `ws` unless a flow of [`ORIGIN`] exists there (also a deleted one: you
/// removed it). Also makes the mail bot the triage bot of `connection_id` when none is set and the
/// workspace has an enabled `mail` bot. Returns the new flow's id.
pub fn install(
    cx: &FlowCx,
    ws: &str,
    connection_id: &str,
    actor: &str,
) -> FlowResult<Option<String>> {
    let g = graph::validate(&graph(), &cx.nodes).map_err(ApiError::rule)?;
    let now = cx.now();
    cx.store().transaction(|tx| -> FlowResult<Option<String>> {
        let mut st = sources::settings(tx, ws, connection_id)?;
        if st.triage_bot.is_none() {
            let has_mail_bot: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id AND w.workspace_id = ?
                   WHERE b.id = ? AND b.enabled = 1 AND b.deleted = 0)",
                params![ws, MAIL_BOT],
                |r| r.get(0),
            )?;
            if has_mail_bot {
                st.triage_bot = Some(MAIL_BOT.into());
                sources::save(tx, &st, now)?;
            }
        }
        let had: Option<String> = tx
            .query_row(
                "SELECT id FROM flows WHERE workspace_id = ? AND origin = ? LIMIT 1",
                params![ws, ORIGIN],
                |r| r.get(0),
            )
            .optional()?;
        if had.is_some() {
            return Ok(None);
        }
        let id = flows::create(
            tx,
            ws,
            actor,
            &NewFlow {
                kind: "flow",
                name: NAME,
                description: DESCRIPTION,
                graph: &g,
                origin: ORIGIN,
                origin_label: flows::origin_label(ORIGIN),
                enabled: true,
            },
            now,
        )?;
        Ok(Some(id))
    })
}
