//! Explicit GitHub approvals and their durable effects.
//! The runner performs writes after commit and reconciles uncertain results before retrying.

use steve_store::W;

use std::sync::{Arc, OnceLock, Weak};

use serde_json::{Value, json};
use steve_modules::decisions::{DecisionCx, DecisionHandler, DecisionOutcome, choice_result};
use steve_modules::{ApiError, ApiResult, Registry};
use steve_store::base::DecisionKind;
use steve_store::{Decision, NewDecision};

use crate::api::GhError;
use crate::effects::{self, Fail, Ran};
use crate::push::{self, PushSpec};
use crate::repos::Settings;
use crate::sync::Effects;
use crate::{GitHub, Inner};
use steve_store::github::PrRow;
use steve_store::github::RepoRow;

/// `subject_type` of “open a PR from this item's branch” (subject = the item).
pub const PR_OPEN: &str = "gh_pr_open";
/// `subject_type` of “merge this PR” (subject = the `ghp_…`).
pub const PR_MERGE: &str = "gh_pr_merge";
pub const OPTIONS: [&str; 2] = ["yes", "no"];
/// The message to an agent's session after you said no to its PR, verbatim.
pub const PR_DENIED: &str =
    "用户没有批准开 PR。不要自己推送分支或开 PR；需要的话用 steve_ask 问用户。";

/// The [`GitHub`] service for things registered before it exists (decision handlers and item
/// hooks go into the [`Registry`], which `Modules` needs, which the connections need, which
/// [`GitHub::new`] needs). Holds it weakly.
#[derive(Clone, Default)]
pub struct Slot(Arc<OnceLock<Weak<Inner>>>);

impl Slot {
    /// Binds the service, once.
    pub fn bind(&self, gh: &GitHub) {
        if self.0.set(Arc::downgrade(&gh.0)).is_err() {
            tracing::warn!("github slot already bound");
        }
    }

    pub fn get(&self) -> Option<GitHub> {
        self.0.get().and_then(Weak::upgrade).map(GitHub)
    }

    pub(crate) fn need(&self) -> ApiResult<GitHub> {
        self.get()
            .ok_or_else(|| ApiError::Internal("GitHub 模块还没有准备好".into()))
    }
}

pub fn register(r: &mut Registry) -> Slot {
    let slot = Slot::default();
    r.decision(PR_OPEN, Box::new(PrOpenDecisions(slot.clone())));
    r.decision(PR_MERGE, Box::new(PrMergeDecisions(slot.clone())));
    r.decision(
        crate::comment::COMMENT,
        Box::new(crate::comment::CommentDecisions(slot.clone())),
    );
    r.item_panel(Box::new(crate::panel::GhPanels(slot.clone())));
    crate::config::register(r, slot.clone());
    slot
}

/// The question of an agent's `steve_github_open_pr`: `在验收前开一个草稿 PR（<repo>）：<标题>`.
/// The PR is opened as a draft exactly when the question says so (the decision has no other
/// place for the flag, and the schema is frozen).
pub fn agent_question(full_name: &str, title: &str, draft: bool) -> String {
    let kind = if draft { DRAFT_ASK } else { PLAIN_ASK };
    format!("{kind}（{full_name}）：{title}")
}
const DRAFT_ASK: &str = "在验收前开一个草稿 PR";
const PLAIN_ASK: &str = "在验收前开一个 PR";

/// What a card about a write through a PAT connection adds: it goes out as your own account.
pub fn pat_note(login: &str) -> String {
    if login.is_empty() {
        "（以你的 GitHub 账号发出）".into()
    } else {
        format!("（以你的 GitHub 账号 {login} 发出）")
    }
}

/// Title and body from the decision: your edited text (first line = title), else the draft.
fn title_body(d: &Decision) -> (String, String) {
    let text = d.result["text"]
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(&d.draft);
    let mut lines = text.trim().splitn(2, '\n');
    let title: String = lines
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(256)
        .collect();
    let body = lines.next().unwrap_or("").trim().to_string();
    (title, body)
}

impl GitHub {
    fn timeline(&self, item: &str, text: &str) {
        let now = self.now();
        let r = self.store().write(|tx| {
            steve_store::item_repo::add_timeline(tx, item, text, now)?;
            let ws = steve_store::item_repo::item_workspace(tx, item)?
                .ok_or_else(|| ApiError::not_found("事项"))?;
            tx.emit(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Item {
                    ws: ws.into(),
                    id: {
                        let value: String = item.into();
                        steve_types::ItemId::from(value)
                    },
                },
            ));
            Ok::<_, ApiError>(())
        });
        if let Err(e) = r {
            tracing::warn!(item = %item, error = %e, "github timeline line not written");
        }
    }

    pub(crate) async fn open_pr(
        &self,
        d: &Decision,
        e: &steve_store::github::EffectRow,
        reconcile: bool,
    ) -> Ran {
        let ws = d.workspace_id.clone();
        let Some(item_id) = d.item_id.clone() else {
            return Ran::Failed("决定没有事项".into());
        };
        let session = d.session_id.as_deref();
        if d.result["choice"] != "yes" {
            self.tell_session(&ws, Some(&item_id), session, PR_DENIED)
                .await;
            return Ran::Done(json!({}));
        }
        match self.push_and_open(d, &item_id, e, reconcile).await {
            Ok(pr) => {
                let kind = if pr.draft { "草稿 PR" } else { "PR" };
                let text = format!(
                    "用户批准了开 PR。分支已推送，{kind} #{} 已开：{}",
                    pr.number, pr.url
                );
                self.tell_session(&ws, Some(&item_id), session, &text).await;
                Ran::Done(effects::pr_result(pr.number))
            }
            Err(f) => {
                if let Some(r) = f.ran() {
                    return r;
                }
                let text = format!("开 PR 失败：{f}");
                tracing::warn!(item = %item_id, "{text}");
                self.timeline(&item_id, &text);
                self.tell_session(&ws, Some(&item_id), session, &text).await;
                Ran::Failed(f.message)
            }
        }
    }

    /// Pushes the item's branch and opens the PR. The commit is the branch's when the first run starts, written down
    /// before anything is sent ([`GitHub::effect_intent`]): a later run pushes that one or
    /// nothing (a branch that moved is a refusal), and first looks for a PR GitHub has from it
    /// ([`GitHub::find_opened_pull_request`]).
    async fn push_and_open(
        &self,
        d: &Decision,
        item_id: &str,
        e: &steve_store::github::EffectRow,
        reconcile: bool,
    ) -> Result<PrRow, Fail> {
        let ws = d.workspace_id.clone();
        let item = self
            .store()
            .items()
            .get(item_id)
            .map_err(|e| e.to_string())?
            .filter(|i| i.workspace_id == ws)
            .ok_or("事项不在了".to_string())?;
        let branch = item
            .branch
            .clone()
            .filter(|b| !b.is_empty())
            .ok_or("事项没有分支".to_string())?;
        let project = item
            .project_id
            .clone()
            .ok_or("事项不属于项目".to_string())?;
        let (r, path) = self
            .store()
            .read(|c| {
                let r = steve_store::github::repo_of_project(c, &ws, &project)?;
                let p = steve_store::projects::project(c, &ws, &project)?.map(|p| p.path);
                Ok::<_, ApiError>((r, p))
            })
            .map_err(|e| e.to_string())?;
        let r = r
            .filter(|r| r.enabled)
            .ok_or("项目没有关联可用的仓库".to_string())?;
        let nodes = self.git_nodes();
        let cwd = item.cwd.clone().filter(|c| !c.is_empty()).or(path);
        let conn = self.effect_conn(&ws, &r.connection_id)?;
        let sha = match e.result["sha"].as_str() {
            Some(s) => s.to_string(),
            None => {
                let on_node = match &nodes {
                    Some(n) => n.head_of(item_id, &branch).await,
                    None => None,
                };
                let head = match on_node {
                    Some(h) => h.map_err(|e| e.to_string())?,
                    None => {
                        let c = cwd.clone().ok_or("事项没有工作目录".to_string())?;
                        let b = branch.clone();
                        tokio::task::spawn_blocking(move || {
                            push::head_of(std::path::Path::new(&c), &b)
                        })
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())?
                    }
                };
                self.effect_intent(&d.id, &json!({"sha": head, "branch": branch}))?;
                head
            }
        };
        let existing = if reconcile {
            self.find_opened_pull_request(&conn, &r, &branch, &sha)
                .await?
        } else {
            None
        };
        let resp = match existing {
            Some(pr) => pr,
            None => {
                let token = conn.cx().access_token().await.map_err(|e| e.to_string())?;
                let (full, git_base, b, want) = (
                    r.full_name.clone(),
                    self.env().git_base.clone(),
                    branch.clone(),
                    sha.clone(),
                );
                self.mark_pull_write(d, false)?;
                let on_node = match &nodes {
                    Some(n) => {
                        if !push::branch_ok(&b) {
                            return Err(push::PushError::BadBranch(b).to_string().into());
                        }
                        push::check_git_base(&git_base).map_err(|e| e.to_string())?;
                        let url = format!("{}/{full}.git", git_base.trim_end_matches('/'));
                        n.push(&push::NodePush {
                            item: item_id,
                            key: &d.id,
                            branch: &b,
                            full_name: &full,
                            url: &url,
                            token: token.expose(),
                            expect: &want,
                        })
                        .await
                    }
                    None => None,
                };
                match on_node {
                    Some(Ok(_)) => {}
                    // whether it landed is what the branch on GitHub says now; never pushed again
                    Some(Err(push::PushError::Lost)) => {
                        self.confirm_push(&conn, &r, &branch, &sha).await?;
                    }
                    // it may still land: this run's end is unknown, the next one asks again
                    Some(Err(e @ push::PushError::Running)) => {
                        return Err(Fail::unknown(e.to_string()));
                    }
                    Some(Err(e)) => return Err(e.to_string().into()),
                    None => {
                        let cwd = cwd.ok_or("事项没有工作目录".to_string())?;
                        // the approved commit, or nothing; pushing it again is harmless
                        tokio::task::spawn_blocking(move || {
                            push::push(&PushSpec {
                                cwd: std::path::Path::new(&cwd),
                                branch: &b,
                                full_name: &full,
                                git_base: &git_base,
                                token: token.expose(),
                                expect: Some(&want),
                            })
                        })
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())?;
                    }
                }
                let (title, body) = title_body(d);
                let draft = !d.question.starts_with(PLAIN_ASK);
                self.mark_pull_write(d, true)?;
                let opened = conn
                    .post(
                        &format!("/repos/{}/pulls", r.full_name),
                        &json!({"title": title, "head": branch, "base": r.default_branch, "body": body, "draft": draft}),
                    )
                    .await;
                match opened {
                    Ok(v) if v["number"].as_i64().is_some() => v,
                    // GitHub said yes without a PR in it: it may be open
                    Ok(_) => return Err(Fail::unknown("GitHub 的回答里没有 PR 编号")),
                    // one is open from the branch (an earlier try's after all, or another): only
                    // the one from this commit is ours
                    Err(GhError::Status { status: 422, .. }) => self
                        .find_opened_pull_request(&conn, &r, &branch, &sha)
                        .await?
                        .ok_or_else(|| {
                            Fail::definite("GitHub 拒绝开 PR（422），这个提交也没有 PR")
                        })?,
                    Err(e) => return Err(e.into()),
                }
            }
        };
        // GitHub has it from here on: a local failure is unknown, the next run finds the PR
        let now = self.now();
        let res = self.store().write(|tx| {
            let mut fx = Effects::default();
            let pr = self
                .sync_pr(
                    tx,
                    &r,
                    &resp,
                    "opened",
                    &[],
                    &Value::Null,
                    Some(item_id),
                    now,
                    &mut fx,
                )?
                .ok_or_else(|| ApiError::Internal("GitHub 没有给出 PR 编号".into()))?;
            crate::after::emit(tx, &r.workspace_id, &r.connection_id, &mut fx)?;
            Ok::<_, ApiError>(pr)
        });
        let pr = res.map_err(|e| Fail::unknown(e.to_string()))?;
        tracing::info!(repo = %r.full_name, number = pr.number, item = %item_id, "github PR opened");
        Ok(pr)
    }

    /// After a push through a Node whose end is not known: `Ok` when GitHub's `branch` is at
    /// `sha` (it landed), else a refusal (it is not pushed a second time).
    async fn confirm_push(
        &self,
        conn: &crate::api::GhConn,
        r: &RepoRow,
        branch: &str,
        sha: &str,
    ) -> Result<(), Fail> {
        let at = match conn
            .get(&format!("/repos/{}/git/ref/heads/{branch}", r.full_name))
            .await
        {
            Ok(v) => v["object"]["sha"].as_str().map(str::to_string),
            Err(GhError::Status { status: 404, .. }) => None,
            Err(e) => return Err(e.into()),
        };
        if at.as_deref() == Some(sha) {
            return Ok(());
        }
        Err(Fail::definite(format!(
            "{}；GitHub 上的分支 {branch} 不是批准的提交，需要重新确认",
            push::PushError::Lost
        )))
    }

    /// The PR GitHub has from `branch` at `sha` (any state: a PR can't be deleted, so none means
    /// none was opened). Several, or a list too long to read whole, is no evidence: stuck.
    async fn find_opened_pull_request(
        &self,
        conn: &crate::api::GhConn,
        r: &RepoRow,
        branch: &str,
        sha: &str,
    ) -> Result<Option<Value>, Fail> {
        let owner = r.full_name.split('/').next().unwrap_or_default();
        let list = conn
            .get_all_complete(
                &format!(
                    "/repos/{}/pulls?state=all&head={owner}:{branch}&base={}&per_page=100",
                    r.full_name, r.default_branch
                ),
                None,
            )
            .await?;
        let mut from: Vec<Value> = list
            .into_iter()
            .filter(|p| p["head"]["sha"].as_str() == Some(sha))
            .collect();
        match from.len() {
            0 => Ok(None),
            1 => Ok(from.pop()),
            n => Err(Fail::stuck(format!(
                "GitHub 上这个提交有 {n} 个 PR，不知道哪个是这次开的"
            ))),
        }
    }

    /// After a `gh_pr_merge` `yes`: merge at the approved head and apply the branch cleanup policy.
    /// `reconcile`: an earlier try may have merged it, so GitHub is asked first; only a merge of
    /// the approved head counts.
    pub(crate) async fn merge_pr(&self, d: &Decision, reconcile: bool) -> Ran {
        let ws = d.workspace_id.clone();
        let Some(sha) = d.result["sha"].as_str().map(String::from) else {
            return Ran::Failed("批准里没有要合并的提交".into());
        };
        let loaded = self.store().read(|c| {
            let pr =
                steve_store::github::pr_get(c, &d.subject_id)?.filter(|p| p.workspace_id == ws);
            let r = match &pr {
                Some(p) => steve_store::github::repo_get(c, &p.repo_id)?,
                None => None,
            };
            Ok::<_, ApiError>(pr.zip(r))
        });
        let (pr, r) = match loaded {
            Ok(Some(x)) => x,
            Ok(None) => return Ran::Failed("PR 不在了".into()),
            Err(e) => return Ran::Unknown(e.to_string()),
        };
        let item = pr.item_id.clone().or(d.item_id.clone());
        let note = |text: String| {
            if let Some(i) = &item {
                self.timeline(i, &text);
            }
            Ran::Failed(text)
        };
        let conn = match self.effect_conn(&ws, &r.connection_id) {
            Ok(c) => c,
            Err(f) => {
                return f
                    .ran()
                    .unwrap_or_else(|| note(format!("合并 PR #{} 失败：{f}", pr.number)));
            }
        };
        let s = Settings::of(&r.settings);
        let path = format!("/repos/{}/pulls/{}", r.full_name, pr.number);
        let merged_before = if reconcile {
            match conn.get_if_changed(&path, None).await {
                Ok(v) if v.body["merged"].as_bool() == Some(true) => {
                    if v.body["head"]["sha"].as_str() != Some(sha.as_str()) {
                        return note(format!(
                            "PR #{} 已经合并，但合并的不是批准的那个提交",
                            pr.number
                        ));
                    }
                    true
                }
                Ok(_) => false,
                Err(e) => return Ran::Unknown(e.to_string()),
            }
        } else {
            false
        };
        if !merged_before {
            if let Err(f) = self.mark_pull_write(d, true) {
                return f.ran().unwrap_or(Ran::Unknown(f.message));
            }
            let res = conn
                .put(
                    &format!("{path}/merge"),
                    &json!({"merge_method": s.merge_method, "sha": sha}),
                )
                .await;
            match res {
                Ok(v) if v["merged"].as_bool() == Some(true) => {}
                Ok(_) => return Ran::Unknown("GitHub 的回答没有说已经合并".into()),
                Err(GhError::Status { status: 409, .. }) => {
                    return note(format!(
                        "没有合并 PR #{}：PR 有新的提交，等检查重新通过",
                        pr.number
                    ));
                }
                Err(e) => {
                    let f = Fail::from(e);
                    return f
                        .ran()
                        .unwrap_or_else(|| note(format!("合并 PR #{} 失败：{f}", pr.number)));
                }
            }
        }
        // the branch only if it is still at the merged commit (not reused since)
        if s.delete_branch && push::branch_ok(&pr.head_ref) {
            let one = format!("/repos/{}/git/ref/heads/{}", r.full_name, pr.head_ref);
            let refs = format!("/repos/{}/git/refs/heads/{}", r.full_name, pr.head_ref);
            match conn.get_if_changed(&one, None).await {
                Ok(v) if v.body["object"]["sha"].as_str() == Some(sha.as_str()) => {
                    if let Err(e) = conn.delete(&refs).await {
                        tracing::warn!(repo = %r.full_name, error = %e, "github branch not deleted");
                    }
                }
                Ok(_) => {
                    tracing::info!(repo = %r.full_name, branch = %pr.head_ref, "github branch moved since the merge: kept");
                }
                Err(e) => {
                    tracing::warn!(repo = %r.full_name, error = %e, "github branch not checked")
                }
            }
        }
        let fresh = conn.get_if_changed(&path, None).await.ok();
        let now = self.now();
        let fx = self.store().write(|tx| {
            let mut fx = Effects::default();
            if let Some(v) = &fresh {
                self.sync_pr(
                    tx,
                    &r,
                    &v.body,
                    "closed",
                    &[],
                    &Value::Null,
                    None,
                    now,
                    &mut fx,
                )?;
            }
            crate::after::emit(tx, &r.workspace_id, &r.connection_id, &mut fx)?;
            Ok::<_, ApiError>(())
        });
        match fx {
            Ok(()) => {}
            // merged on GitHub; what is left is ours: the next run sees it merged and does it
            Err(e) => return Ran::Unknown(e.to_string()),
        }
        Ran::Done(json!({ "merged": sha }))
    }
}

/// `gh_pr_open`: `yes` (with your edited title and body in `text`) or `no`.
struct PrOpenDecisions(Slot);

impl DecisionHandler for PrOpenDecisions {
    fn resolve(&self, tx: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<DecisionOutcome> {
        let gh = self.0.need()?;
        let item = d
            .item_id
            .as_deref()
            .ok_or_else(|| ApiError::not_found("事项"))?;
        if cx.choice == "yes" && steve_store::item_repo::open_cycle(tx, cx.ws, item)?.is_none() {
            return Err(ApiError::conflict("item cycle is closed"));
        }
        effects::queue(tx, &gh, cx.ws, &d.id, effects::OPEN)?;
        Ok(choice_result(cx).into())
    }

    /// The branch may be pushed and the PR open by now; there is nothing to take back.
    fn before_undo(&self, _tx: &mut W<'_>, _d: &Decision) -> ApiResult<()> {
        Err(ApiError::rule(steve_i18n::t!("github.decision.no_undo")))
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct MergeRequest {
    head: String,
}

pub(crate) async fn request_merge(
    axum::Extension(gh): axum::Extension<GitHub>,
    who: steve_modules::Caller,
    axum::extract::Path(id): axum::extract::Path<String>,
    steve_modules::Body(body): steve_modules::Body<MergeRequest>,
) -> ApiResult<axum::Json<String>> {
    gh.store().write(|tx| {
        if steve_store::UserRows(tx).role(&who.me)?.as_deref() != Some("owner") {
            return Err(ApiError::forbidden(
                "only the owner can approve a pull request merge",
            ));
        }
        let pr = steve_store::github::pr_get(tx, &id)?
            .filter(|pr| pr.workspace_id == who.ws)
            .ok_or_else(|| ApiError::not_found("PR"))?;
        let repo = steve_store::github::repo_get(tx, &pr.repo_id)?
            .filter(|repo| repo.enabled)
            .ok_or_else(|| ApiError::not_found("repository"))?;
        if let Some(item) = pr.item_id.as_deref()
            && steve_store::item_repo::open_cycle(tx, &who.ws, item)?.is_none()
        {
            return Err(ApiError::conflict("item cycle is closed"));
        }
        if pr.head_sha != body.head || !merge_ready(&pr) {
            return Err(ApiError::conflict(steve_i18n::t!("github.pr.new_commits")));
        }
        if let Some(existing) = steve_store::decision_repo::open_for(tx, &who.ws, PR_MERGE, &id)? {
            if existing.draft == body.head {
                return Ok(axum::Json(existing.id));
            }
            steve_store::DecisionRows(tx).settle(
                &existing.id,
                &json!({"reason": "pull_request_head_changed"}),
                "expired",
                &Value::Null,
            )?;
        }
        let decision = steve_store::DecisionRows(tx).open(&NewDecision {
            workspace_id: who.ws.clone(),
            kind: DecisionKind::Approve.code().into(),
            subject_type: PR_MERGE.into(),
            subject_id: id.clone(),
            item_id: pr.item_id,
            session_id: None,
            question: String::new(),
            draft: Some(body.head.clone()),
            options: Some(OPTIONS.map(String::from).to_vec()),
            requested_by: who.me.clone(),
            question_template: Some(steve_types::DecisionPrompt::MergePullRequest {
                number: pr.number,
                repo: repo.full_name,
            }),
            source: Some(steve_store::DecisionSource::Revision {
                revision: body.head,
            }),
        })?;
        Ok(axum::Json(decision.id))
    })
}

fn merge_ready(pr: &PrRow) -> bool {
    pr.state == "open"
        && !pr.draft
        && pr.checks_state == "success"
        && pr.review_state != "changes_requested"
        && pr.mergeable.as_deref() == Some("clean")
}

/// Merges only the head shown when the owner opened the approval.
struct PrMergeDecisions(Slot);

impl DecisionHandler for PrMergeDecisions {
    fn resolve(&self, tx: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<DecisionOutcome> {
        let gh = self.0.need()?;
        let pr = steve_store::github::pr_get(tx, &d.subject_id)?
            .filter(|p| p.workspace_id == cx.ws)
            .ok_or_else(|| ApiError::not_found("PR"))?;
        if pr.state != "open" {
            return Err(ApiError::conflict(steve_i18n::t!("github.pr.closed")));
        }
        if cx.choice == "yes"
            && steve_store::UserRows(tx).role(cx.actor)?.as_deref() != Some("owner")
        {
            return Err(ApiError::forbidden(
                "only the owner can approve a pull request merge",
            ));
        }
        if pr.item_id != d.item_id {
            return Err(ApiError::conflict("pull request association changed"));
        }
        if cx.choice == "yes" && (!merge_ready(&pr) || pr.head_sha != d.draft) {
            return Err(ApiError::conflict(steve_i18n::t!("github.pr.new_commits")));
        }
        let mut out = choice_result(cx);
        out["sha"] = json!(pr.head_sha);
        if cx.choice == "yes" {
            effects::queue(tx, &gh, cx.ws, &d.id, effects::MERGE)?;
        }
        Ok(out.into())
    }

    fn before_undo(&self, _tx: &mut W<'_>, _d: &Decision) -> ApiResult<()> {
        Err(ApiError::rule(steve_i18n::t!("github.decision.no_undo")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(draft: &str, result: Value) -> Decision {
        Decision {
            revision: 1,
            source_revision: None,
            question_template: None,
            id: "dec_1".into(),
            workspace_id: "default".into(),
            kind: "approve".into(),
            subject_type: PR_OPEN.into(),
            subject_id: "ST-1".into(),
            item_id: Some("ST-1".into()),
            session_id: None,
            question: String::new(),
            draft: draft.into(),
            options: OPTIONS.map(String::from).to_vec(),
            status: steve_types::DecisionStatus::Resolved,
            result,
            requested_by: "owner".into(),
            created: 0,
            resolved: None,
            undo: Value::Null,
        }
    }

    #[test]
    fn title_and_body_from_the_answer_or_the_draft() {
        let d = decision(
            "修复返回\n\n摘要\n\nST-1 · 由 Steve 开出",
            json!({"choice": "yes"}),
        );
        assert_eq!(
            title_body(&d),
            ("修复返回".into(), "摘要\n\nST-1 · 由 Steve 开出".into())
        );
        let d = decision(
            "x\n\ny",
            json!({"choice": "yes", "text": "  新标题 \n新正文"}),
        );
        assert_eq!(title_body(&d), ("新标题".into(), "新正文".into()));
    }
}
