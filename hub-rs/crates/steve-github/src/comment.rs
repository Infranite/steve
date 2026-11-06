//! Comments on PRs and issues.
//!
//! - Yours (`POST /github/comments`): posted directly, audited `github.comment`.
//! - An agent's (`steve_github_comment`): posted directly only while an open
//!   `grants(capability="github.comment", subject=bot, resource_type="repo", resource_id=<ghr>)`
//!   covers it; otherwise a `gh_comment` decision (`yes` / `no` / `grant_8h`) and END. The answer
//!   is handled like G3's: the handler records it (and the grant) in the answer's transaction and
//!   queues an effect; the effect posts after the commit and tells the session how it went.
//!
//! Every body that leaves passes the session redactor (values injected into this session) and the
//! paste guard first, then B1's outbound check (v1: every repo's readers count as external).

use axum::routing::post;
use axum::{Extension, Json, Router};
use serde_json::{Value, json};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::decisions::{DecisionCx, DecisionHandler, choice_result};
use steve_modules::outbound::{self, Audience, CheckCx, Verdict};
use steve_modules::vault::mcp::ToolOut;
use steve_modules::vault::r#use::END;
use steve_modules::vault::{VaultResult, guard, redact};
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};
use steve_store::base::{self, DecisionKind, decisions as base_decisions};
use steve_store::{Decision, NewDecision, Tx};

use crate::GitHub;
use crate::accept::Slot;
use crate::effects::{self, Fail, Ran};
use crate::repo::{self, RepoRow};

/// `subject_type` of “may this bot post this comment” (subject = the `ghp_…` or `ghi_…`).
pub const COMMENT: &str = "gh_comment";
pub const OPTIONS: [&str; 3] = ["yes", "no", "grant_8h"];
/// Hours of the grant `grant_8h` writes.
pub const GRANT_HOURS: i64 = 8;
/// Longest comment, in characters (GitHub allows 65536 bytes).
pub const BODY_MAX: usize = 20_000;
const HOUR_MS: i64 = 3_600_000;

/// What a comment goes on.
#[derive(Debug, Clone)]
pub struct Target {
    pub repo: RepoRow,
    /// `ghp_…` or `ghi_…`.
    pub subject: String,
    pub number: i64,
    pub is_pr: bool,
}

impl Target {
    /// `PR #12` / `Issue #7`.
    pub fn label(&self) -> String {
        let kind = if self.is_pr { "PR" } else { "Issue" };
        format!("{kind} #{}", self.number)
    }
}

/// The PR or issue `subject` (`ghp_…` / `ghi_…`) of workspace `ws`, with its repo.
pub fn target(c: &rusqlite::Connection, ws: &str, subject: &str) -> ApiResult<Option<Target>> {
    let (repo_id, number, is_pr) = if subject.starts_with("ghp_") {
        match repo::pr_get(c, subject)?.filter(|p| p.workspace_id == ws) {
            Some(p) => (p.repo_id, p.number, true),
            None => return Ok(None),
        }
    } else if subject.starts_with("ghi_") {
        match repo::issue_get(c, subject)?.filter(|i| i.workspace_id == ws) {
            Some(i) => (i.repo_id, i.number, false),
            None => return Ok(None),
        }
    } else {
        return Ok(None);
    };
    Ok(repo::repo_get(c, &repo_id)?.map(|repo| Target {
        repo,
        subject: subject.into(),
        number,
        is_pr,
    }))
}

/// The text that may leave Steve: values injected into `session` are masked, then anything the
/// paste guard recognizes (tokens, keys, passwords, card numbers).
pub fn clean(session: Option<&str>, body: &str) -> String {
    let s = match session {
        Some(id) => redact::scrub_text(id, body).into_owned(),
        None => body.to_string(),
    };
    guard::redact(&s).unwrap_or(s)
}

/// The message to the session after a `no`, verbatim.
pub const DENIED: &str =
    "用户没有批准这条评论，没有发出。不要换个方式再发；需要的话用 steve_ask 问用户。";

/// The message to the session after the comment went out.
pub fn posted_message(t: &Target, url: &str, granted: bool) -> String {
    let mut s = format!("用户批准了这条评论，已发到 {}：{url}", t.label());
    if granted {
        s.push_str(&format!(
            "。接下来 {GRANT_HOURS} 小时内你在 {} 的评论不用再等批准",
            t.repo.full_name
        ));
    }
    s
}

impl GitHub {
    /// `POST /repos/{o}/{r}/issues/{n}/comments` (PRs are issues here); the comment's URL and id.
    pub(crate) async fn post_comment(
        &self,
        ws: &str,
        t: &Target,
        body: &str,
    ) -> Result<(String, i64), Fail> {
        let conn = self.comment_conn(ws, t, false)?;
        self.post_on(&conn, t, body).await
    }

    async fn post_on(
        &self,
        conn: &crate::api::GhConn,
        t: &Target,
        body: &str,
    ) -> Result<(String, i64), Fail> {
        let v = conn
            .post(
                &format!("/repos/{}/issues/{}/comments", t.repo.full_name, t.number),
                &json!({ "body": body }),
            )
            .await?;
        match comment_ref(&v) {
            // GitHub said yes without a comment id in it: it may be there
            (_, 0) => Err(Fail::unknown("GitHub 的回答里没有评论编号")),
            r => Ok(r),
        }
    }

    /// `effect`: for an approved write, a connection that waits is [`effects::Why::Wait`].
    fn comment_conn(&self, ws: &str, t: &Target, effect: bool) -> Result<crate::api::GhConn, Fail> {
        if !t.repo.enabled {
            return Err(Fail::definite("GitHub App 不再能访问这个仓库"));
        }
        if effect {
            return self.effect_conn(ws, &t.repo.connection_id);
        }
        self.conn_for_write(ws, &t.repo.connection_id)
            .map_err(|e| Fail::definite(e.to_string()))
    }

    /// The comment an earlier try posted: exactly `body`, not another effect's (two approvals of
    /// the same text are two comments), and the only such one. None found is no evidence (it may
    /// have been deleted), and neither are several: stuck, not posted again by itself.
    async fn posted_before(
        &self,
        conn: &crate::api::GhConn,
        t: &Target,
        body: &str,
    ) -> Result<(String, i64), Fail> {
        let all = conn
            .get_all_complete(
                &format!(
                    "/repos/{}/issues/{}/comments?per_page=100",
                    t.repo.full_name, t.number
                ),
                None,
            )
            .await?;
        let mut found = Vec::new();
        for c in all.iter().filter(|c| c["body"].as_str() == Some(body)) {
            let (url, id) = comment_ref(c);
            let claimed = self
                .store()
                .reader(|db| repo::comment_claimed(db, id).map_err(ApiError::from))
                .map_err(|e| Fail::unknown(e.to_string()))?;
            if !claimed {
                found.push((url, id));
            }
        }
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(Fail::stuck(
                "GitHub 上没有找到这条评论：可能没发出，也可能被删掉了；不会自动重发",
            )),
            n => Err(Fail::stuck(format!(
                "GitHub 上有 {n} 条一样的评论，不知道哪条是这次发的"
            ))),
        }
    }

    /// `audit(github.comment)`: who, where, how it was allowed. Never the body.
    fn audit_comment(&self, ws: &str, actor: &str, t: &Target, detail: Value) {
        let mut d = json!({"repo": t.repo.full_name, "number": t.number, "pr": t.is_pr});
        if let (Some(m), Value::Object(extra)) = (d.as_object_mut(), detail) {
            m.extend(extra);
        }
        let r = self.store().transaction(|tx| {
            base::audit(tx, ws, actor, "github.comment", &t.subject, &d).map_err(ApiError::from)
        });
        if let Err(e) = r {
            tracing::warn!(error = %e, "github comment not audited");
        }
    }

    /// `POST /github/comments {target, id, body}`: your own comment, posted now.
    pub async fn user_comment(
        &self,
        who: &Caller,
        kind: &str,
        id: &str,
        body: &str,
    ) -> VaultResult<Value> {
        let noun = if kind == "pr" { "PR" } else { "Issue" };
        let t = self
            .store()
            .reader(|c| target(c, &who.ws, id))?
            .filter(|t| t.is_pr == (kind == "pr"))
            .ok_or_else(|| ApiError::not_found(noun))?;
        let body = clean(None, body);
        let (url, _) = self
            .post_comment(&who.ws, &t, &body)
            .await
            .map_err(|e| ApiError::rule(not_posted(&e)))?;
        self.audit_comment(&who.ws, &who.me, &t, json!({"by": "me"}));
        self.notifier().gh(&who.ws, &t.subject);
        Ok(json!({ "url": url }))
    }

    /// An agent's comment (`steve_github_comment`): posted now under a grant, otherwise asked.
    pub(crate) async fn agent_comment(
        &self,
        m: &Modules,
        cx: &McpCx,
        t: Target,
        body: &str,
    ) -> Result<ToolOut, String> {
        let ws = cx.ws.clone();
        let body = clean(cx.session_id.as_deref(), body);
        let check = outbound::check(
            m,
            &CheckCx {
                ws: ws.clone(),
                actor: cx.bot_id.clone(),
                draft_model: None,
                audience_said: None,
                skip_model: false,
                internal_skip: false,
            },
            &Audience {
                external: true,
                description: format!("GitHub 仓库 {} 的读者，外部", t.repo.full_name),
                ..Default::default()
            },
            &body,
            &[],
        )
        .await;
        if check.verdict == Verdict::Blocked {
            return Err("The comment contains a secret value and was not posted.".into());
        }
        let now = self.now();
        let (grant, bot_name) = self
            .store()
            .reader(|c| {
                let g = if cx.bot_id.is_empty() {
                    None
                } else {
                    repo::comment_grant(c, &ws, &cx.bot_id, &t.repo.id, now)?
                };
                let name = repo::bot_name(c, &cx.bot_id)?;
                Ok::<_, ApiError>((g, name))
            })
            .map_err(|e| e.to_string())?;
        if let Some(g) = grant.filter(|_| check.verdict != Verdict::Leak) {
            let (url, _) = self
                .post_comment(&ws, &t, &body)
                .await
                .map_err(|e| not_posted(&e))?;
            self.audit_comment(&ws, &cx.bot_id, &t, json!({"by": "bot", "grant": g}));
            return Ok(ToolOut {
                text: format!("Commented: {url}"),
                awaiting_approval: false,
            });
        }
        let bot = bot_name.unwrap_or_else(|| "Bot".into());
        let mut question = format!("{bot} 想在 {} 评论", t.label());
        if check.verdict == Verdict::Leak {
            question.push_str("（披露检查提示可能泄露内部信息）");
        }
        let d = NewDecision {
            workspace_id: ws.clone(),
            kind: DecisionKind::Approve.code().into(),
            subject_type: COMMENT.into(),
            subject_id: t.subject.clone(),
            item_id: cx.item_id.clone(),
            session_id: cx.session_id.clone(),
            question,
            draft: Some(body),
            options: Some(OPTIONS.map(String::from).to_vec()),
            requested_by: if cx.bot_id.is_empty() {
                "bot".into()
            } else {
                cx.bot_id.clone()
            },
        };
        self.store()
            .transaction(|tx| base_decisions::open(tx, &d).map_err(ApiError::from))
            .map_err(|e| e.to_string())?;
        Ok(ToolOut {
            text: format!("Approval requested. {END}"),
            awaiting_approval: true,
        })
    }

    /// After a `gh_comment` answer: post (yes, grant_8h) or not (no), and tell the session.
    /// `reconcile`: an earlier try may have posted it, so GitHub is asked first.
    pub(crate) async fn comment_answered(&self, d: &Decision, reconcile: bool) -> Ran {
        let ws = d.workspace_id.clone();
        let (item, session) = (d.item_id.as_deref(), d.session_id.as_deref());
        if d.result["choice"] == "no" {
            self.tell_session(&ws, item, session, DENIED).await;
            return Ran::Done(json!({}));
        }
        let t = match self.store().reader(|c| target(c, &ws, &d.subject_id)) {
            Ok(Some(t)) => t,
            _ => {
                let m = "评论没有发出：PR 或 Issue 不在了";
                self.tell_session(&ws, item, session, m).await;
                return Ran::Failed(m.into());
            }
        };
        let text = d.result["text"]
            .as_str()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(&d.draft);
        let body = clean(session, text);
        let granted = d.result.get("grant").is_some();
        let conn = match self.comment_conn(&ws, &t, true) {
            Ok(c) => c,
            Err(f) => {
                if let Some(r) = f.ran() {
                    return r;
                }
                self.tell_session(&ws, item, session, &not_posted(&f)).await;
                return Ran::Failed(f.message);
            }
        };
        let posted = if reconcile {
            self.posted_before(&conn, &t, &body).await
        } else {
            match self.effect_sending(&d.id) {
                Ok(()) => self.post_on(&conn, &t, &body).await,
                Err(f) => Err(f),
            }
        };
        match posted {
            Ok((url, id)) => {
                self.audit_comment(
                    &ws,
                    &d.requested_by,
                    &t,
                    json!({"by": "bot", "decision": d.id}),
                );
                self.tell_session(&ws, item, session, &posted_message(&t, &url, granted))
                    .await;
                Ran::Done(json!({ "comment": id }))
            }
            Err(f) => match f.ran() {
                Some(r) => r,
                None => {
                    tracing::warn!(decision = %d.id, error = %f, "github comment not posted");
                    self.tell_session(&ws, item, session, &not_posted(&f)).await;
                    Ran::Failed(f.message)
                }
            },
        }
    }
}

/// `(html_url, id)` of a comment.
fn comment_ref(v: &Value) -> (String, i64) {
    (
        v["html_url"].as_str().unwrap_or_default().to_string(),
        v["id"].as_i64().unwrap_or_default(),
    )
}

/// What to say when a comment did not (certainly) go out.
fn not_posted(f: &Fail) -> String {
    if f.maybe() {
        format!("GitHub 没有回应，评论可能已经发出，先到 GitHub 上看一下再决定是否重发：{f}")
    } else {
        format!("评论没有发出：{f}")
    }
}

/// `gh_comment`: `yes` posts once, `grant_8h` also lets the bot comment in this repo for 8 hours,
/// `no` posts nothing.
pub(crate) struct CommentDecisions(pub(crate) Slot);

impl DecisionHandler for CommentDecisions {
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let gh = self.0.need()?;
        let t = target(tx, cx.ws, &d.subject_id)?.ok_or_else(|| ApiError::not_found("PR"))?;
        let mut out = choice_result(cx);
        if cx.choice == "grant_8h" {
            let now = gh.now();
            let id = steve_store::uid("gr");
            repo::insert_comment_grant(
                tx,
                &id,
                cx.ws,
                &d.requested_by,
                &t.repo.id,
                now + GRANT_HOURS * HOUR_MS,
                cx.actor,
                Some(&d.id),
                now,
            )?;
            base::audit(
                tx,
                cx.ws,
                cx.actor,
                "grant.add",
                &t.repo.id,
                &json!({"grant": id, "capability": repo::COMMENT_CAP, "bot": d.requested_by,
                        "hours": GRANT_HOURS, "decision": d.id}),
            )?;
            out["grant"] = json!(id);
        }
        effects::queue(tx, &gh, cx.ws, &d.id, effects::COMMENT)?;
        Ok(out)
    }

    /// The comment may be out by now; there is nothing to take back.
    fn before_undo(&self, _tx: &Tx, _d: &Decision) -> ApiResult<()> {
        Err(ApiError::rule(steve_i18n::t!("github.decision.no_undo")))
    }
}

#[derive(serde::Deserialize)]
struct CommentBody {
    target: String,
    id: String,
    body: String,
}

/// `POST /github/comments`, relative to `/api`, behind the `Caller` layer.
pub fn routes(gh: GitHub) -> Router<Modules> {
    Router::new()
        .route("/github/comments", post(user_comment))
        .layer(Extension(gh))
}

async fn user_comment(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Body(b): Body<CommentBody>,
) -> VaultResult<Json<Value>> {
    let mut c = steve_modules::Check::new();
    c.one_of("target", &b.target, &["pr", "issue"]);
    c.len("id", &b.id, 1, 100);
    c.that(
        "body",
        !b.body.trim().is_empty() && b.body.chars().count() <= BODY_MAX,
    );
    c.done()?;
    Ok(Json(
        gh.user_comment(&who, &b.target, &b.id, &b.body).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_masks_what_the_guard_finds() {
        let b = clean(None, "看日志 ghp_abcdefghijklmnopqrstuvwxyz0123456789 就好");
        assert!(!b.contains("ghp_"), "{b}");
        assert_eq!(clean(None, "LGTM"), "LGTM");
    }
}
