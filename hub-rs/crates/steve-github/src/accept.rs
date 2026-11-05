//! The wrap-up through GitHub: after you accept an item, Steve asks whether
//! to open a PR (`gh_pr_open`); with `finish_on=merged` it asks again when the checks are green
//! (`gh_pr_merge`). Pushing, opening and merging are outward effects: they happen only after your
//! `yes`, never from the hook or the sync.
//!
//! Decision handlers run inside the answer's transaction and cannot reach GitHub, so they only
//! check the answer and record the write ([`crate::effects`]); the runner does it after the commit,
//! reconciling first when an earlier try may have happened.

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_modules::decisions::{DecisionCx, DecisionHandler, choice_result};
use steve_modules::hooks::{ItemHooks, Takeover};
use steve_modules::{ApiError, ApiResult, Registry};
use steve_store::base::DecisionKind;
use steve_store::base::decisions as base_decisions;
use steve_store::{Decision, Item, NewDecision, Tx};

use crate::api::GhError;
use crate::effects::{self, Fail, Ran};
use crate::push::{self, PushSpec};
use crate::repo::{self, PrRow, RepoRow};
use crate::repos::{Settings, check_state};
use crate::sync::{Effects, latest_checks};
use crate::{GitHub, Inner};

/// `subject_type` of “open a PR from this item's branch” (subject = the item).
pub const PR_OPEN: &str = "gh_pr_open";
/// `subject_type` of “merge this PR” (subject = the `ghp_…`).
pub const PR_MERGE: &str = "gh_pr_merge";
pub const OPTIONS: [&str; 2] = ["yes", "no"];
/// `requested_by` of the decisions Steve opens itself (agents' requests carry their bot id).
pub const STEVE: &str = "steve";

/// The message to an agent's session after you said no to its PR, verbatim.
pub const PR_DENIED: &str =
    "用户没有批准开 PR。不要自己推送分支或开 PR；需要的话用 steve_ask 问用户。";

/// Longest PR title Steve proposes (GitHub's list shows about this much).
const TITLE_MAX: usize = 72;

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

/// Registers the `gh_pr_open` / `gh_pr_merge` / `gh_comment` decision handlers, the item hooks
/// (`on_accepted`) and the item panels (`github.pr:…`). Bind the returned slot to the service once
/// it is built.
pub fn register(r: &mut Registry) -> Slot {
    let slot = Slot::default();
    r.decision(PR_OPEN, Box::new(PrOpenDecisions(slot.clone())));
    r.decision(PR_MERGE, Box::new(PrMergeDecisions(slot.clone())));
    r.decision(
        crate::comment::COMMENT,
        Box::new(crate::comment::CommentDecisions(slot.clone())),
    );
    r.item_hooks(Box::new(GhItemHooks(slot.clone())));
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

/// The draft flag of a `gh_pr_open`: Steve's own follow the repo's `draft_pr`; an agent's say it
/// in the question.
fn wants_draft(d: &Decision, settings: &Settings) -> bool {
    if d.requested_by == STEVE {
        settings.draft_pr
    } else {
        !d.question.starts_with(PLAIN_ASK)
    }
}

/// `Item` hooks of this module.
struct GhItemHooks(Slot);

#[async_trait]
impl ItemHooks for GhItemHooks {
    fn name(&self) -> &'static str {
        "github"
    }

    async fn on_accepted(&self, item: &Item) -> Option<Takeover> {
        let gh = self.0.get()?;
        match gh.offer_pr(item).await {
            Ok(true) => Some(Takeover { by: "github" }),
            Ok(false) => None,
            Err(e) => {
                tracing::warn!(item = %item.id, error = %e, "github wrap-up not offered");
                None
            }
        }
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

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `✓ build` lines of a PR's latest checks.
fn check_lines(c: &rusqlite::Connection, repo: &RepoRow, pr: &PrRow) -> rusqlite::Result<String> {
    let rows = repo::checks_for_sha(c, &repo.id, &pr.head_sha)?;
    Ok(latest_checks(&rows)
        .into_iter()
        .map(|k| {
            let icon = match check_state(k) {
                "ok" => "✓",
                "fail" => "✗",
                "skipped" => "–",
                _ => "…",
            };
            format!("{icon} {}", k.name)
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

impl GitHub {
    /// `finish(item, now)` unless it is finished already (the merge, the webhook that reports it
    /// and a repeated outbox delivery can all get here); `Err` when finishing failed (the outbox
    /// tries again).
    pub(crate) async fn finish_item_checked(
        &self,
        ws: &str,
        item: &str,
        now: &str,
    ) -> Result<(), String> {
        let status = self
            .store()
            .reader(|c| repo::item_status(c, ws, item).map_err(ApiError::from))
            .map_err(|e| e.to_string())?;
        if !matches!(status.as_deref(), Some(s) if s != "done" && s != "drop") {
            return Ok(());
        }
        self.core()
            .finish(ws, item, now)
            .await
            .map_err(|e| e.to_string())
    }

    fn timeline(&self, item: &str, text: &str) {
        let now = self.now();
        let r = self.store().transaction(|tx| {
            repo::add_timeline(tx, item, text, now)?;
            tx.publish(steve_store::ChangeEvent::Item { id: item.into() });
            Ok::<_, ApiError>(())
        });
        if let Err(e) = r {
            tracing::warn!(item = %item, error = %e, "github timeline line not written");
        }
    }

    /// `on_item_accepted`: offers a PR (and takes the wrap-up over) when the item's project is
    /// linked, `after_accept=ask_pr`, the item has a branch with commits the base lacks, the project
    /// has no release action, and no PR from that branch is open. Opens the `gh_pr_open` decision;
    /// pushes nothing.
    pub async fn offer_pr(&self, item: &Item) -> Result<bool, String> {
        let ws = item.workspace_id.clone();
        let (Some(branch), Some(project)) = (
            item.branch.clone().filter(|b| !b.is_empty()),
            item.project_id.clone(),
        ) else {
            return Ok(false);
        };
        let pre = self
            .store()
            .reader(|c| {
                let Some(r) = repo::repo_of_project(c, &ws, &project)?.filter(|r| r.enabled) else {
                    return Ok(None);
                };
                if Settings::of(&r.settings).after_accept != "ask_pr"
                    || repo::project_release(c, &ws, &project)?.is_some()
                    || repo::has_open_pr_from(c, &r.id, &branch)?
                    || base_decisions::open_for(c, &ws, PR_OPEN, &item.id)?.is_some()
                {
                    return Ok(None);
                }
                let path = repo::project(c, &ws, &project)?.map(|p| p.path);
                let summary = repo::delivery_summary(c, &item.id)?;
                let prefix = repo::item_prefix(c, &ws)?;
                Ok::<_, ApiError>(Some((r, path, summary, prefix)))
            })
            .map_err(|e| e.to_string())?;
        let Some((r, path, summary, _prefix)) = pre else {
            return Ok(false);
        };
        let Some(cwd) = item
            .cwd
            .clone()
            .filter(|c| !c.is_empty())
            .or(path)
            .filter(|c| !c.is_empty())
        else {
            return Ok(false);
        };
        let (base, b) = (r.default_branch.clone(), branch.clone());
        let ahead =
            tokio::task::spawn_blocking(move || push::ahead(std::path::Path::new(&cwd), &base, &b))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        if ahead == 0 {
            return Ok(false);
        }
        let title = cut(item.title.trim(), TITLE_MAX);
        let mut body = summary.unwrap_or_default().trim().to_string();
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&format!("{} · 由 Steve 开出", item.id));
        let d = NewDecision {
            workspace_id: ws.clone(),
            kind: DecisionKind::Approve.code().into(),
            subject_type: PR_OPEN.into(),
            subject_id: item.id.clone(),
            item_id: Some(item.id.clone()),
            session_id: None,
            question: format!("在 {} 开 PR：{title}", r.full_name),
            draft: Some(format!("{title}\n\n{body}")),
            options: Some(OPTIONS.map(String::from).to_vec()),
            requested_by: STEVE.into(),
        };
        self.store()
            .transaction(|tx| base_decisions::open(tx, &d).map_err(ApiError::from))
            .map_err(|e| e.to_string())?;
        if let Err(e) = self
            .core()
            .progress(&ws, &item.id, "收尾", "验收通过，等你确认开 PR")
            .await
        {
            tracing::warn!(item = %item.id, error = %e, "github: item progress not set");
        }
        Ok(true)
    }

    /// After a `gh_pr_open` answer: `yes` pushes the branch and opens the PR; `no` finishes the
    /// item (when Steve asked on acceptance). A later run of `e` (after unknown) reconciles first.
    pub(crate) async fn open_pr(
        &self,
        d: &Decision,
        e: &crate::repo::EffectRow,
        reconcile: bool,
    ) -> Ran {
        let ws = d.workspace_id.clone();
        let Some(item_id) = d.item_id.clone() else {
            return Ran::Failed("决定没有事项".into());
        };
        let by_steve = d.requested_by == STEVE;
        let session = d.session_id.as_deref();
        if d.result["choice"] != "yes" {
            if by_steve {
                if let Err(f) = self.finish_later(&ws, &item_id, "已验收，没有开 PR") {
                    return Ran::Unknown(f.message);
                }
            } else {
                self.tell_session(&ws, Some(&item_id), session, PR_DENIED)
                    .await;
            }
            return Ran::Done(json!({}));
        }
        match self.push_and_open(d, &item_id, e, reconcile).await {
            Ok((pr, settings)) => {
                let done = Ran::Done(effects::pr_result(pr.number));
                if !by_steve {
                    let kind = if pr.draft { "草稿 PR" } else { "PR" };
                    let text = format!(
                        "用户批准了开 PR。分支已推送，{kind} #{} 已开：{}",
                        pr.number, pr.url
                    );
                    self.tell_session(&ws, Some(&item_id), session, &text).await;
                    return done;
                }
                if settings.finish_on == "merged" {
                    let now = format!("PR #{} 等检查", pr.number);
                    if let Err(e) = self.core().progress(&ws, &item_id, "收尾", &now).await {
                        tracing::warn!(item = %item_id, error = %e, "github: item progress not set");
                    }
                } else if let Err(f) =
                    self.finish_later(&ws, &item_id, &format!("已验收，PR #{} 已开", pr.number))
                {
                    // the PR is open; finishing is what is left: the next run finds the PR
                    return Ran::Unknown(f.message);
                }
                done
            }
            Err(f) => {
                if let Some(r) = f.ran() {
                    return r;
                }
                let text = format!("开 PR 失败：{f}");
                tracing::warn!(item = %item_id, "{text}");
                if by_steve {
                    if let Err(e) = self.core().fail(&ws, &item_id, &text).await {
                        tracing::warn!(item = %item_id, error = %e, "github: item not failed");
                    }
                } else {
                    self.timeline(&item_id, &text);
                    self.tell_session(&ws, Some(&item_id), session, &text).await;
                }
                Ran::Failed(f.message)
            }
        }
    }

    /// Pushes the item's branch and opens the PR; the stored PR row (linked `steve`) and the
    /// repo's settings. The commit is the branch's when the first run starts, written down
    /// before anything is sent ([`GitHub::effect_intent`]): a later run pushes that one or
    /// nothing (a branch that moved is a refusal), and first looks for a PR GitHub has from it
    /// ([`GitHub::pr_from`]).
    async fn push_and_open(
        &self,
        d: &Decision,
        item_id: &str,
        e: &crate::repo::EffectRow,
        reconcile: bool,
    ) -> Result<(PrRow, Settings), Fail> {
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
            .reader(|c| {
                let r = repo::repo_of_project(c, &ws, &project)?;
                let p = repo::project(c, &ws, &project)?.map(|p| p.path);
                Ok::<_, ApiError>((r, p))
            })
            .map_err(|e| e.to_string())?;
        let r = r
            .filter(|r| r.enabled)
            .ok_or("项目没有关联可用的仓库".to_string())?;
        let cwd = item
            .cwd
            .clone()
            .filter(|c| !c.is_empty())
            .or(path)
            .ok_or("事项没有工作目录".to_string())?;
        let settings = Settings::of(&r.settings);
        let conn = self.effect_conn(&ws, &r.connection_id)?;
        let sha = match e.result["sha"].as_str() {
            Some(s) => s.to_string(),
            None => {
                let (c, b) = (cwd.clone(), branch.clone());
                let head = tokio::task::spawn_blocking(move || {
                    push::head_of(std::path::Path::new(&c), &b)
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
                self.effect_intent(&d.id, &json!({"sha": head, "branch": branch}))?;
                head
            }
        };
        let existing = if reconcile {
            self.pr_from(&conn, &r, &branch, &sha).await?
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
                let (title, body) = title_body(d);
                let draft = wants_draft(d, &settings);
                self.effect_sending(&d.id)?;
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
                        .pr_from(&conn, &r, &branch, &sha)
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
        let res = self.store().transaction(|tx| {
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
            self.ask_merges(tx, &r, &fx)?;
            crate::after::emit(tx, &r.workspace_id, &r.connection_id, &mut fx)?;
            Ok::<_, ApiError>((pr, fx))
        });
        let (pr, fx) = res.map_err(|e| Fail::unknown(e.to_string()))?;
        self.after_commit(&ws, &r.connection_id, fx).await;
        tracing::info!(repo = %r.full_name, number = pr.number, item = %item_id, "github PR opened");
        Ok((pr, settings))
    }

    /// The PR GitHub has from `branch` at `sha` (any state: a PR can't be deleted, so none means
    /// none was opened). Several, or a list too long to read whole, is no evidence: stuck.
    async fn pr_from(
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

    /// Does the item wait for its PR to merge: `finish_on=merged`, Steve's PR was approved on
    /// acceptance, and the item is not finished?
    pub(crate) fn waits_for_merge(
        &self,
        c: &rusqlite::Connection,
        repo: &RepoRow,
        item: &str,
    ) -> Result<bool, ApiError> {
        if Settings::of(&repo.settings).finish_on != "merged" {
            return Ok(false);
        }
        let open = repo::item_status(c, &repo.workspace_id, item)?
            .is_some_and(|s| s != "done" && s != "drop");
        Ok(open && repo::wrap_up_by_pr(c, &repo.workspace_id, item)?)
    }

    /// For every PR this transaction touched, ask to merge when it is ready (checks green,
    /// no changes requested, mergeable) and its item waits for it; once per head commit.
    pub(crate) fn ask_merges(&self, tx: &Tx, repo: &RepoRow, fx: &Effects) -> Result<(), ApiError> {
        for id in fx.gh.iter().filter(|i| i.starts_with("ghp_")) {
            let Some(pr) = repo::pr_get(tx, id)? else {
                continue;
            };
            let Some(item) = pr.item_id.clone() else {
                continue;
            };
            let ready = pr.state == "open"
                && pr.checks_state == "success"
                && pr.review_state != "changes_requested"
                && pr.mergeable.as_deref() == Some("clean");
            if !ready
                || !self.waits_for_merge(tx, repo, &item)?
                || repo::merge_asked(tx, &repo.workspace_id, &pr.id, &pr.head_sha)?
            {
                continue;
            }
            let s = Settings::of(&repo.settings);
            let mut draft = check_lines(tx, repo, &pr)?;
            if !draft.is_empty() {
                draft.push('\n');
            }
            draft.push_str(&format!("合并方式：{}", s.merge_method));
            if s.delete_branch {
                draft.push_str(&format!("，合并后删除分支 {}", pr.head_ref));
            }
            base_decisions::open(
                tx,
                &NewDecision {
                    workspace_id: repo.workspace_id.clone(),
                    kind: DecisionKind::Approve.code().into(),
                    subject_type: PR_MERGE.into(),
                    subject_id: pr.id.clone(),
                    item_id: Some(item),
                    session_id: None,
                    question: format!("合并 PR #{}（{}）？", pr.number, repo.full_name),
                    draft: Some(draft),
                    options: Some(OPTIONS.map(String::from).to_vec()),
                    requested_by: STEVE.into(),
                },
            )?;
        }
        Ok(())
    }

    /// After a `gh_pr_merge` `yes`: merge at the approved head, delete the branch, finish.
    /// `reconcile`: an earlier try may have merged it, so GitHub is asked first; only a merge of
    /// the approved head counts.
    pub(crate) async fn merge_pr(&self, d: &Decision, reconcile: bool) -> Ran {
        let ws = d.workspace_id.clone();
        let Some(sha) = d.result["sha"].as_str().map(String::from) else {
            return Ran::Failed("批准里没有要合并的提交".into());
        };
        let loaded = self.store().reader(|c| {
            let pr = repo::pr_get(c, &d.subject_id)?.filter(|p| p.workspace_id == ws);
            let r = match &pr {
                Some(p) => repo::repo_get(c, &p.repo_id)?,
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
            if let Err(f) = self.effect_sending(&d.id) {
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
        // the merged state now, rather than when the webhook comes; the item finishes through
        // the outbox in the same transaction
        let finish = item
            .as_ref()
            .map(|i| (i.clone(), format!("PR #{} 已合并", pr.number)));
        let fresh = conn.get_if_changed(&path, None).await.ok();
        let now = self.now();
        let fx = self.store().transaction(|tx| {
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
            fx.finish.extend(finish.clone());
            crate::after::emit(tx, &r.workspace_id, &r.connection_id, &mut fx)?;
            Ok::<_, ApiError>(fx)
        });
        match fx {
            Ok(fx) => self.after_commit(&ws, &r.connection_id, fx).await,
            // merged on GitHub; what is left is ours: the next run sees it merged and does it
            Err(e) => return Ran::Unknown(e.to_string()),
        }
        Ran::Done(json!({ "merged": sha }))
    }
}

/// `gh_pr_open`: `yes` (with your edited title and body in `text`) or `no`.
struct PrOpenDecisions(Slot);

impl DecisionHandler for PrOpenDecisions {
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let gh = self.0.need()?;
        let item = d
            .item_id
            .as_deref()
            .ok_or_else(|| ApiError::not_found("事项"))?;
        if cx.choice == "yes" && repo::item_status(tx, cx.ws, item)?.is_none() {
            return Err(ApiError::not_found("事项"));
        }
        effects::queue(tx, &gh, cx.ws, &d.id, effects::OPEN)?;
        Ok(choice_result(cx))
    }

    /// The branch may be pushed and the PR open by now; there is nothing to take back.
    fn before_undo(&self, _tx: &Tx, _d: &Decision) -> ApiResult<()> {
        Err(ApiError::rule(steve_i18n::t!("github.decision.no_undo")))
    }
}

/// `gh_pr_merge`: `yes` merges at the head the checks passed on; `no` leaves the PR open (and is
/// not asked again for that head).
struct PrMergeDecisions(Slot);

impl DecisionHandler for PrMergeDecisions {
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let gh = self.0.need()?;
        let pr = repo::pr_get(tx, &d.subject_id)?
            .filter(|p| p.workspace_id == cx.ws)
            .ok_or_else(|| ApiError::not_found("PR"))?;
        if pr.state != "open" {
            return Err(ApiError::conflict(steve_i18n::t!("github.pr.closed")));
        }
        if cx.choice == "yes" && pr.checks_state != "success" {
            return Err(ApiError::conflict(steve_i18n::t!("github.pr.new_commits")));
        }
        let mut out = choice_result(cx);
        out["sha"] = json!(pr.head_sha);
        if cx.choice == "yes" {
            effects::queue(tx, &gh, cx.ws, &d.id, effects::MERGE)?;
        }
        Ok(out)
    }

    fn before_undo(&self, _tx: &Tx, _d: &Decision) -> ApiResult<()> {
        Err(ApiError::rule(steve_i18n::t!("github.decision.no_undo")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(draft: &str, result: Value) -> Decision {
        Decision {
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
            status: "resolved".into(),
            result,
            requested_by: STEVE.into(),
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
