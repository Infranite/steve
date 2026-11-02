//! What the executor knows about a decision before [`super::judge`] sees it: the evidence each row of
//! the table needs, read from the database (check results, disclosure verdicts, release state, who
//! the message speaks as), the full tool input the runner handed over, and whether the user is around.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult};
use steve_store::yolo as repo;

use super::answer;
use super::engine::Engine;
use super::judge::{
    self, Accept, CheckRun, Disclosure, Facts, Identity, Outbound, OutboundStage, Policy, Release,
    Stall, StallCause, Subject, Tool,
};
use super::ledger::{self, Streak};
use crate::git::git;
use crate::repo::{Decision, Item};

/// The facts, and the progress snapshot automatic continues are compared by.
pub struct Gathered {
    pub facts: Facts,
    pub progress: Value,
}

/// `会话出错：…` and the restart texts of the orchestrator (`on_turn_end`, `recover`).
const SESSION_ERROR: &str = "会话出错：";
const INTERRUPTED: &str = "Hub 重启时这个会话正在运行";
const STOPPED: &str = "Agent 停下来了";

fn ce(e: crate::CoreError) -> ApiError {
    match e {
        crate::CoreError::Module(e) => e,
        e => ApiError::Internal(e.to_string()),
    }
}

/// `检查：✓ a；✗ b` at the end of a delivery's draft (`tool_deliver`).
pub fn parse_checks(draft: &str) -> Vec<CheckRun> {
    let Some(i) = draft.rfind("检查：") else {
        return Vec::new();
    };
    draft[i + "检查：".len()..]
        .split('；')
        .filter_map(|c| {
            let c = c.trim();
            if let Some(n) = c.strip_prefix('✓') {
                Some(CheckRun {
                    name: n.trim().into(),
                    passed: true,
                })
            } else {
                c.strip_prefix('✗').map(|n| CheckRun {
                    name: n.trim().into(),
                    passed: false,
                })
            }
        })
        .collect()
}

/// Why a `fail` card about an item came up, from its question.
pub fn stall_cause(d: &Decision) -> StallCause {
    let q = d.question.as_str();
    if d.subject_type == crate::stall::STALL || q.starts_with(STOPPED) {
        return StallCause::Stopped;
    }
    if let Some(e) = q.strip_prefix(SESSION_ERROR) {
        return StallCause::Error {
            message: e.into(),
            restart: false,
        };
    }
    if q.starts_with(INTERRUPTED) {
        return StallCause::Error {
            message: q.into(),
            restart: true,
        };
    }
    StallCause::Other
}

fn git_dir(item: &Item) -> Option<PathBuf> {
    let c = PathBuf::from(item.cwd.as_deref()?);
    c.is_dir().then_some(c)
}

/// `(clean, ahead of main)` of the item's branch.
fn branch_state(dir: &Path) -> (bool, bool) {
    let clean = git(dir, &["status", "--porcelain"]).is_ok_and(|s| s.is_empty());
    let ahead = ["main", "master"].iter().any(|b| {
        git(dir, &["rev-list", "--count", &format!("{b}..HEAD")])
            .ok()
            .and_then(|n| n.parse::<u64>().ok())
            .is_some_and(|n| n > 0)
    });
    (clean, ahead)
}

impl Engine {
    /// The processes the agent of `d`'s session started, looked up only for a command that kills.
    fn own_pids(&self, d: &Decision, raw: &Value) -> Vec<u32> {
        let mentions_kill = raw
            .get("command")
            .is_some_and(|c| c.to_string().contains("kill"));
        let pid = d
            .session_id
            .as_deref()
            .filter(|_| mentions_kill)
            .and_then(|s| self.core().0.runtime.agent_pid(s));
        pid.map(super::procs::live_started_by).unwrap_or_default()
    }

    /// Everything [`judge::judge`] needs about `d` (open, in scope).
    pub fn gather(&self, d: &Decision, policy: &Policy) -> ApiResult<Gathered> {
        let db = self.core().store();
        let item = match d.item_id.as_deref() {
            Some(i) => db.item(i).map_err(ce)?,
            None => None,
        };
        let project = match item.as_ref().and_then(|i| i.project_id.as_deref()) {
            Some(p) => db.project(Some(p)).map_err(ce)?,
            None => None,
        };
        let progress = match &item {
            Some(i) => json!({
                "done": i.plan.iter().filter(|p| p.done).count(),
                "head": git_dir(i).and_then(|d| git(&d, &["rev-parse", "HEAD"]).ok()),
            }),
            None => Value::Null,
        };
        let streak = match &item {
            Some(i) => {
                let acts = self
                    .yolo()
                    .store()
                    .reader(|c| repo::actions_of_item(c, &i.id, 50))?;
                ledger::streak(&acts, &progress)
            }
            None => Streak::default(),
        };
        let own = self
            .yolo()
            .own_dir(item.as_ref().and_then(|i| i.cwd.as_deref()));
        let project_dir = project.as_ref().map(|p| PathBuf::from(&p.path));
        let branch = item.as_ref().and_then(|i| i.branch.clone());
        // the user being around makes YOLO wait anyway: no model call for nothing
        let present = match &item {
            Some(i) => self.present(&i.id, policy.presence_s)?,
            None => false,
        };
        let subject = match (d.kind.as_str(), d.subject_type.as_str()) {
            ("approve", "tool_call") => match self.tool_input(&d.id) {
                Some(t) => Subject::Tool(Tool {
                    own_pids: self.own_pids(d, &t.raw),
                    kind: t.kind,
                    title: d.question.trim_start_matches("允许执行：").to_string(),
                    raw: t.raw,
                    truncated: false,
                    locations: t.locations,
                    options: judge::tool_options(&d.options),
                    cwd: own,
                    project_dir,
                    branch,
                }),
                None => judge::subject_from_row(d, own, project_dir, branch),
            },
            ("accept", "item") => match &item {
                Some(i) => Subject::Accept(Accept {
                    checks: parse_checks(&d.draft),
                    unfinished: i
                        .plan
                        .iter()
                        .filter(|p| !p.done)
                        .map(|p| p.step.clone())
                        .collect(),
                    branch: match (&i.branch, git_dir(i)) {
                        (Some(_), Some(dir)) => Some(branch_state(&dir)),
                        (Some(_), None) => Some((false, false)),
                        _ => None,
                    },
                    explicit_manual: i.accept_mode.as_deref() == Some("manual"),
                    no_progress: streak.no_progress,
                }),
                None => Subject::Unverified("事项不在了".into()),
            },
            ("fail", "item") | ("fail", crate::stall::STALL) => {
                let cause = self.stall_cause(d, item.as_ref())?;
                // it stopped on a question in plain text: answer it the way `steve_ask` is answered
                let sid = match d.session_id.clone() {
                    Some(s) => Some(s),
                    None => match &item {
                        Some(i) => self.item_session(&i.id)?,
                        None => None,
                    },
                };
                let asked = match (&cause, sid.as_deref()) {
                    (StallCause::Stopped, Some(sid)) if !present => {
                        match answer::looks_like_question(&self.session_tail(sid)?) {
                            Some(q) => Some(self.ask_facts(
                                d,
                                item.as_ref(),
                                &q,
                                &[],
                                (None, None),
                                &d.id,
                                policy.answer_asks && policy.continue_stalls && !present,
                            )?),
                            None => None,
                        }
                    }
                    _ => None,
                };
                Subject::Stall(Stall {
                    cause,
                    no_progress: streak.no_progress,
                    attempts: streak.errors,
                    asked,
                })
            }
            ("approve", "release") => {
                match (&item, project.as_ref().and_then(|p| p.release.clone())) {
                    (Some(i), Some(r)) => Subject::Release(Release {
                        batch: false,
                        release_kind: r.kind,
                        cmd: r.cmd,
                        branch_ok: i.branch.as_deref() == Some(d.subject_id.as_str()),
                        accepted: self.accepted(&i.id)?,
                        // one at a time: a release running, the Hub holding for one, or another
                        // card YOLO already scheduled
                        releasing: self.core().release_busy() || self.release_queued(d)?,
                        spacing_ok: self.release_spacing_ok(&d.workspace_id)?,
                        rebase_returns: self.rebase_returns(&d.workspace_id, &i.id)?,
                    }),
                    _ => Subject::Unverified("项目没有配置发布".into()),
                }
            }
            ("ask", "question") => Subject::Ask(self.ask_facts(
                d,
                item.as_ref(),
                &d.question,
                &d.options,
                answer::parse_hint(&d.draft),
                &d.id,
                policy.answer_asks && !present,
            )?),
            ("propose", "item") if !d.question.starts_with("前置事项都已完成") => {
                Subject::ProposeCreate {
                    from_chat: item.as_ref().is_some_and(|i| i.source.1 == "chat"),
                }
            }
            ("approve", "secret_use") => self.secret_use(d)?,
            (_, "chan_message") => self.chan(d)?,
            (_, "mail_draft") => self.mail(d)?,
            _ => judge::subject_from_row(d, own, project_dir, branch),
        };
        let mut facts = judge::facts_of(d, subject);
        facts.present = present;
        Ok(Gathered { facts, progress })
    }

    /// The item's session that is not ended, newest.
    fn item_session(&self, item: &str) -> ApiResult<Option<String>> {
        Ok(self
            .core()
            .store()
            .sessions_of_item(item)
            .map_err(ce)?
            .into_iter()
            .rfind(|s| s.status != "ended")
            .map(|s| s.id))
    }

    fn accepted(&self, item: &str) -> ApiResult<bool> {
        Ok(self
            .core()
            .store()
            .decisions_of_item(item)
            .map_err(ce)?
            .iter()
            .any(|x| x.kind == "accept" && x.status == "resolved" && x.result["choice"] == "pass"))
    }

    /// The user acted on the item lately: a message or an answer of theirs on its timeline, or a
    /// message of theirs waiting in its session's queue.
    fn present(&self, item: &str, presence_s: u64) -> ApiResult<bool> {
        let since = self.yolo().now() - presence_s as i64 * 1000;
        self.yolo().store().reader(|c| {
            let n: i64 = c
                .prepare_cached(
                    "SELECT (SELECT count(*) FROM timeline WHERE item_id = ?1 AND kind = 'me' AND ts >= ?2)
                          + (SELECT count(*) FROM queued q JOIN sessions s ON s.id = q.session_id
                              WHERE s.item_id = ?1 AND q.via = 'me')",
                )?
                .query_row(params![item, since], |r| r.get(0))?;
            Ok(n > 0)
        })
    }

    fn secret_use(&self, d: &Decision) -> ApiResult<Subject> {
        let row: Option<(String, String)> = self.yolo().store().reader(|c| {
            Ok::<_, ApiError>(
                c.prepare_cached(
                    "SELECT coalesce(i.template, ''), coalesce(i.sensitivity, '') FROM secret_uses u
                       JOIN vault_items i ON i.id = u.item_id WHERE u.id = ?",
                )
                .and_then(|mut st| {
                    st.query_row([&d.subject_id], |r| Ok((r.get(0)?, r.get(1)?)))
                        .optional()
                })
                .unwrap_or(None),
            )
        })?;
        let (template, sensitivity) = row.unwrap_or_default();
        Ok(Subject::SecretUse {
            template,
            sensitivity,
        })
    }

    /// A channel message card: who it speaks as, the space's audience, the disclosure verdict.
    fn chan(&self, d: &Decision) -> ApiResult<Subject> {
        type Row = (String, String, i64, Option<String>, Option<String>);
        let row: Option<Row> = self.yolo().store().reader(|c| {
            Ok::<_, ApiError>(
                c.prepare_cached(
                    "SELECT m.on_behalf, m.status, s.has_external, cs.mode,
                            (SELECT b.verdict FROM reply_basis b WHERE b.message_id = m.id ORDER BY b.created DESC LIMIT 1)
                       FROM chan_messages m JOIN spaces s ON s.id = m.space_id
                       LEFT JOIN channel_settings cs ON cs.connection_id = s.connection_id
                      WHERE m.id = ? AND m.workspace_id = ?",
                )?
                .query_row(params![d.subject_id, d.workspace_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .optional()?,
            )
        })?;
        let Some((on_behalf, status, external, mode, verdict)) = row else {
            return Ok(Subject::Unverified("消息不在了".into()));
        };
        let identity = if on_behalf == "proxy" {
            Identity::Proxy
        } else if mode.as_deref() == Some("self") {
            Identity::SelfAccount
        } else {
            Identity::Bot
        };
        let disclosure = match (status.as_str(), verdict.as_deref()) {
            ("blocked", _) => Disclosure::Blocked,
            (_, Some("pass")) => Disclosure::Pass,
            (_, Some("rewritten")) => Disclosure::Rewritten,
            (_, Some("drafted")) => Disclosure::Leak,
            _ => Disclosure::Unchecked,
        };
        let has = |o: &str| d.options.iter().any(|x| x == o);
        let (stage, send) = match d.kind.as_str() {
            "approve" if disclosure == Disclosure::Leak => (OutboundStage::LeakDraft, None),
            "approve" => (OutboundStage::Draft, has("yes").then(|| "yes".to_string())),
            "fail" if status == "failed" && has("retry") => {
                (OutboundStage::SendFailed, Some("retry".to_string()))
            }
            "fail" => (OutboundStage::Blocked, None),
            _ => (OutboundStage::Handoff, None),
        };
        Ok(Subject::Chan(Outbound {
            identity,
            disclosure,
            external: external != 0,
            customer: false,
            // the outbound path (steve-channels `Outbox::send`) marks what YOLO approved in your name
            ai_prefix: matches!(identity, Identity::SelfAccount | Identity::Proxy),
            send_option: send,
            stage,
        }))
    }

    /// A mail draft card: always in your name; external unless every address is in your organisation.
    fn mail(&self, d: &Decision) -> ApiResult<Subject> {
        type Row = (String, Option<String>, String, String, String);
        let row: Option<Row> = self.yolo().store().reader(|c| {
            Ok::<_, ApiError>(
                c.prepare_cached(
                    "SELECT status, basis, to_addrs, cc_addrs, bcc_addrs FROM mail_drafts
                      WHERE id = ? AND workspace_id = ?",
                )?
                .query_row(params![d.subject_id, d.workspace_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .optional()?,
            )
        })?;
        let Some((status, basis, to, cc, bcc)) = row else {
            return Ok(Subject::Unverified("邮件草稿不在了".into()));
        };
        let verdict = basis
            .and_then(|b| serde_json::from_str::<Value>(&b).ok())
            .and_then(|b| b["verdict"].as_str().map(String::from));
        let disclosure = match verdict.as_deref() {
            Some("pass") => Disclosure::Pass,
            Some("leak") => Disclosure::Leak,
            Some("blocked") => Disclosure::Blocked,
            _ => Disclosure::Unchecked,
        };
        let mut addrs = Vec::new();
        for l in [&to, &cc, &bcc] {
            if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(l) {
                addrs.extend(
                    a.iter()
                        .filter_map(|r| r["addr"].as_str().map(|s| s.trim().to_lowercase())),
                );
            }
        }
        let mut external = addrs.is_empty();
        for a in &addrs {
            let domain = a.rsplit_once('@').map(|x| x.1).unwrap_or_default();
            let internal: bool = self.yolo().store().reader(|c| {
                Ok::<_, ApiError>(
                    c.prepare_cached(
                        "SELECT EXISTS (SELECT 1 FROM org_domains o JOIN parties p ON p.id = o.org_id
                           WHERE o.workspace_id = ? AND o.domain = ? AND p.org_kind = 'self')",
                    )
                    .and_then(|mut st| st.query_row(params![d.workspace_id, domain], |r| r.get(0)))
                    .unwrap_or(false),
                )
            })?;
            external |= !internal;
        }
        let has = |o: &str| d.options.iter().any(|x| x == o);
        let (stage, send) = match d.kind.as_str() {
            "approve" => (
                OutboundStage::Draft,
                has("send").then(|| "send".to_string()),
            ),
            "fail" if status == "failed" && has("retry") => {
                (OutboundStage::SendFailed, Some("retry".to_string()))
            }
            _ => (OutboundStage::Blocked, None),
        };
        Ok(Subject::Mail(Outbound {
            identity: Identity::SelfAccount,
            disclosure,
            external,
            customer: false,
            // steve-mail `Sender::execute` puts `[AI] ` on the body's first line for YOLO's approvals
            ai_prefix: true,
            send_option: send,
            stage,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_come_from_the_delivery_draft() {
        let c = parse_checks("做完了\n\n检查：✓ cargo test；✗ clippy");
        assert_eq!(
            c,
            vec![
                CheckRun {
                    name: "cargo test".into(),
                    passed: true
                },
                CheckRun {
                    name: "clippy".into(),
                    passed: false
                }
            ]
        );
        assert!(parse_checks("没有检查").is_empty());
    }
}
