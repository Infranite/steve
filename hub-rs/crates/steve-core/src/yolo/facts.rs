//! What the executor knows about a decision before [`super::judge`] sees it: the evidence each row of
//! the table needs, read from the database (check results, disclosure verdicts, release state, who
//! the message speaks as), the full tool input the runner handed over, and whether the user is around.

use std::path::{Path, PathBuf};

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
use super::push::Checkout;
use crate::git::git;
use crate::node_git::{Ask, NodeGit, block_on};
use crate::repo::{Decision, Item};

/// The facts, and the progress snapshot automatic continues are compared by.
pub struct Gathered {
    pub facts: Facts,
    pub progress: Value,
}

/// `会话出错：…` and the restart texts of the orchestrator (`on_turn_end`, `recover`).
pub(super) const SESSION_ERROR: &str = "会话出错：";
pub(super) const INTERRUPTED: &str = "Hub 重启时这个会话正在运行";
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

/// How long YOLO waits for an item's Node to say where its branch stands (an acceptance).
const NODE_FACTS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
/// How long YOLO waits for an item's Node to name its `HEAD` alone (a stall's progress).
const NODE_HEAD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// What YOLO reads of an item's branch: `HEAD` (the progress snapshot) and `(clean, ahead of
/// main)`. `None`: there is no checkout to read (or its Node could not tell).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BranchFacts {
    head: Option<String>,
    state: Option<(bool, bool)>,
}

/// [`branch_state`] and `HEAD` of an item whose workspace is on a Node, asked of that Node
/// (N0e-E11c): no git runs here. An offline Node, or one not answering in time, tells nothing.
async fn node_branch(port: &dyn NodeGit, item: &str, state: bool) -> BranchFacts {
    if !state {
        // the head alone: no status scan, no refs
        let head = port
            .facts(
                item,
                Ask {
                    heads_only: true,
                    ..Default::default()
                },
            )
            .await
            .ok()
            .map(|f| f.head)
            .filter(|h| !h.is_empty());
        return BranchFacts { head, state: None };
    }
    let Ok(f) = port
        .facts(
            item,
            Ask {
                refs: vec!["main".into(), "master".into()],
                ..Default::default()
            },
        )
        .await
    else {
        return BranchFacts::default();
    };
    let bases: Vec<(String, String)> = f
        .refs
        .iter()
        .filter(|(_, sha)| sha.is_some())
        .map(|(b, _)| (b.clone(), "HEAD".to_string()))
        .collect();
    let ahead = !bases.is_empty()
        && port
            .facts(
                item,
                Ask {
                    ranges: bases,
                    ..Default::default()
                },
            )
            .await
            .is_ok_and(|r| r.ranges.iter().any(|r| r.ahead > 0));
    BranchFacts {
        head: Some(f.head.clone()).filter(|h| !h.is_empty()),
        state: Some((!f.dirty, ahead)),
    }
}

/// The item's branch as YOLO reads it (`state`: its cleanliness too, for an acceptance): through its
/// Node when it is on one, else the Hub's own git in `items.cwd` (as before).
fn branch_facts(core: &crate::Core, item: &Item, state: bool) -> BranchFacts {
    if let Some(port) = core.node_git().filter(|p| p.on_node(&item.id)) {
        let deadline = if state {
            NODE_FACTS_DEADLINE
        } else {
            NODE_HEAD_DEADLINE
        };
        return block_on(async {
            tokio::time::timeout(deadline, node_branch(&*port, &item.id, state))
                .await
                .unwrap_or_default()
        })
        .unwrap_or_default();
    }
    match git_dir(item) {
        Some(dir) => BranchFacts {
            head: git(&dir, &["rev-parse", "HEAD"]).ok(),
            state: state.then(|| branch_state(&dir)),
        },
        None => BranchFacts::default(),
    }
}

/// The item's checkout for a command that may push or open a pull request (#219, [`super::push`]):
/// the branch `HEAD` is on, asked of its Node when the workspace is on one (the branch only: its
/// settings stay unknown), else read with the Hub's own git in `dir` with the settings. Any other
/// command reads nothing, so no other permission answer waits for git or a Node.
fn checkout(
    core: &crate::Core,
    item: &Item,
    dir: Option<&Path>,
    command: &Value,
) -> Option<Checkout> {
    static MAY_PUSH: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\b(push|gh)\b").expect("valid regex"));
    if !MAY_PUSH.is_match(&command.to_string()) {
        return None;
    }
    if let Some(port) = core.node_git().filter(|p| p.on_node(&item.id)) {
        let ask = Ask {
            heads_only: true,
            ..Default::default()
        };
        let f = block_on(async {
            tokio::time::timeout(NODE_HEAD_DEADLINE, port.facts(&item.id, ask))
                .await
                .ok()
                .and_then(Result::ok)
        })
        .flatten()?;
        return Some(Checkout {
            head: f.branch.filter(|b| !b.is_empty()),
            config: None,
        });
    }
    local_checkout(dir?)
}

/// [`checkout`] of a workspace on the Hub's own machine: `HEAD`'s branch and the settings.
fn local_checkout(dir: &Path) -> Option<Checkout> {
    if !dir.is_dir() {
        return None;
    }
    let config = git(dir, &["config", "--list", "-z"]).ok()?;
    let head = git(dir, &["symbolic-ref", "--short", "-q", "HEAD"]).unwrap_or_default();
    Some(Checkout::from_git(&head, Some(&config)))
}

/// Acceptance uses only the pinned branch snapshot and the already computed progress streak.
fn acceptance_subject(
    d: &Decision,
    item: Option<&Item>,
    branch_now: Option<&BranchFacts>,
    streak: &Streak,
) -> Subject {
    match item {
        Some(i) => Subject::Accept(Accept {
            checks: parse_checks(&d.draft),
            unfinished: i
                .plan
                .iter()
                .filter(|p| !p.done)
                .map(|p| p.step.clone())
                .collect(),
            branch: match (&i.branch, branch_now.and_then(|b| b.state)) {
                (Some(_), Some(state)) => Some(state),
                (Some(_), None) => Some((false, false)),
                _ => None,
            },
            explicit_manual: i.accept_mode.as_deref() == Some("manual"),
            no_progress: streak.no_progress,
        }),
        None => Subject::Unverified("事项不在了".into()),
    }
}

impl Engine {
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
        let accepting = (d.kind.as_str(), d.subject_type.as_str()) == ("accept", "item");
        // the progress snapshot counts only for the verdicts that send the agent back (an
        // acceptance, a stall): any other card reads no branch, so a slow Node never holds up a
        // permission answer
        let continues = accepting
            || (d.kind == "fail"
                && (d.subject_type == "item" || d.subject_type == crate::stall::STALL));
        let branch_now = item
            .as_ref()
            .filter(|_| continues)
            .map(|i| branch_facts(self.core(), i, accepting && i.branch.is_some()));
        let progress = match (&item, &branch_now) {
            (Some(i), Some(b)) => json!({
                "done": i.plan.iter().filter(|p| p.done).count(),
                "head": b.head,
            }),
            _ => Value::Null,
        };
        let streak = match &item {
            Some(i) => {
                let acts = self
                    .yolo()
                    .store()
                    .read(|c| repo::actions_of_item(c, &i.id, 50))?;
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
            ("approve", "tool_call") => {
                self.tool_subject(d, item.as_ref(), own, project_dir, branch)
            }
            ("accept", "item") => {
                acceptance_subject(d, item.as_ref(), branch_now.as_ref(), &streak)
            }
            ("fail", "item") | ("fail", crate::stall::STALL) => {
                self.stalled_subject(d, item.as_ref(), present, &streak, policy)?
            }
            ("approve", "release") => {
                match (&item, project.as_ref().and_then(|p| p.release.clone())) {
                    (Some(i), Some(r)) => Subject::Release(Release {
                        release_kind: r.kind,
                        cmd: r.cmd,
                        branch_ok: i.branch.as_deref() == Some(d.subject_id.as_str()),
                        accepted: self.accepted(&i.id)?,
                        // one at a time: a release running, the Hub holding for one, or another
                        // card YOLO already scheduled
                        releasing: self.core().release_busy() || self.release_queued(d)?,
                        spacing_wait_s: self.release_spacing_wait_s(&d.workspace_id)?,
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
                    // only what you asked for in a conversation; what someone else asked for there
                    // (`chat_proposal`) waits for your own yes
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

    /// Prefer the armed, untruncated request and its execution view over the stored draft.
    fn tool_subject(
        &self,
        d: &Decision,
        item: Option<&Item>,
        own: Option<PathBuf>,
        project_dir: Option<PathBuf>,
        branch: Option<String>,
    ) -> Subject {
        match self.tool_input(&d.id) {
            Some(t) => {
                let cwd = t.own_dir.as_deref().map(super::limits::canonical).or(own);
                let checkout = item
                    .zip(t.raw.get("command"))
                    .and_then(|(i, c)| checkout(self.core(), i, cwd.as_deref(), c));
                Subject::Tool(Tool {
                    full: self.full_access(d),
                    own_pids: t.own_pids.clone().unwrap_or_default(),
                    kind: t.kind,
                    title: d.question.trim_start_matches("允许执行：").to_string(),
                    raw: t.raw,
                    truncated: false,
                    locations: t.locations,
                    options: judge::tool_options(&d.options),
                    cwd,
                    project_dir,
                    branch,
                    checkout,
                })
            }
            None => judge::subject_from_row(d, own, project_dir, branch),
        }
    }

    /// Read the stopped session's current evidence before deciding whether a question can be answered.
    fn stalled_subject(
        &self,
        d: &Decision,
        item: Option<&Item>,
        present: bool,
        streak: &Streak,
        policy: &Policy,
    ) -> ApiResult<Subject> {
        let subject = {
            let cause = self.stall_cause(d, item)?;
            // it stopped on a question in plain text: answer it the way `steve_ask` is answered
            let sid = match d.session_id.clone() {
                Some(s) => Some(s),
                None => match item {
                    Some(i) => self.item_session(&i.id)?,
                    None => None,
                },
            };
            let tail = match (&cause, sid.as_deref()) {
                (StallCause::Stopped, Some(sid)) => self.session_tail(sid)?,
                _ => String::new(),
            };
            // its final reply, whole: what it waits for now, not what it did on the way (a
            // request to scan a code at its start is not lost behind a long explanation)
            let own_action = match sid.as_deref() {
                Some(sid) => {
                    super::limits::own_action(&self.core().store().final_reply(sid).map_err(ce)?)
                }
                _ => None,
            };
            let asked = match (&cause, sid.as_deref()) {
                (StallCause::Stopped, Some(_)) if !present => {
                    match answer::looks_like_question(&tail) {
                        Some(q) => Some(self.ask_facts(
                            d,
                            item,
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
                own_action,
                steve_tried: d.question.contains(crate::takeover::STEVE_TRIED),
                yours: match sid.as_deref() {
                    Some(sid) => self
                        .core()
                        .store()
                        .store()
                        .sessions()
                        .stop(sid)
                        .map_err(|e| ce(e.into()))?
                        .is_some_and(|k| crate::turn_wait::held(&k)),
                    None => false,
                },
            })
        };
        Ok(subject)
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

    /// The tool request comes from a session that runs with full access (完全放开) now: the item's
    /// effective setting is full and the session is not read-only (a read-only session never
    /// bypasses prompts, so its requests stay the guard's).
    pub(crate) fn full_access(&self, d: &Decision) -> bool {
        let full = self
            .yolo()
            .effective(&d.workspace_id, d.item_id.as_deref())
            .is_ok_and(|e| e.full);
        full && d.item_id.is_some()
            && d.session_id.as_deref().is_some_and(|s| {
                self.core()
                    .store()
                    .session(s)
                    .ok()
                    .flatten()
                    .is_some_and(|s| s.readonly == 0)
            })
    }

    /// The user acted on the item lately: a message or an answer of theirs on its timeline, or a
    /// message of theirs queued for its session, within the same window.
    fn present(&self, item: &str, presence_s: u64) -> ApiResult<bool> {
        let since = self.yolo().now() - presence_s as i64 * 1000;
        self.yolo().store().read(|r| {
            Ok(
                steve_store::item_repo::has_recent_user_note(r, item, since)?
                    || steve_store::session_repo::has_recent_queued_input(r, item, since)?,
            )
        })
    }

    fn secret_use(&self, d: &Decision) -> ApiResult<Subject> {
        let (template, sensitivity) = self
            .yolo()
            .store()
            .read(|r| steve_store::vault_use_repo::use_policy(r, &d.subject_id))?
            .unwrap_or_default();
        Ok(Subject::SecretUse {
            template,
            sensitivity,
        })
    }

    /// How many times YOLO sent `d`'s subject again after a failed send: its resolved `fail`
    /// cards it answered `retry` (the outbound paths mark those `decided_by`).
    fn yolo_retries(&self, d: &Decision) -> ApiResult<u32> {
        Ok(self.yolo().store().read(|r| {
            steve_store::decision_repo::retry_count(
                r,
                &d.subject_type,
                &d.subject_id,
                steve_modules::outbound::YOLO_ACTOR,
            )
        })?)
    }

    /// A channel message card: who it speaks as, the space's audience, the disclosure verdict.
    fn chan(&self, d: &Decision) -> ApiResult<Subject> {
        let Some(steve_store::channel_message_repo::OutboundFacts {
            on_behalf,
            status,
            external,
            mode,
            verdict,
            composed,
        }) = self.yolo().store().read(|r| {
            steve_store::channel_message_repo::outbound_facts(r, &d.workspace_id, &d.subject_id)
        })?
        else {
            return Ok(Subject::Unverified("消息不在了".into()));
        };
        let identity = if on_behalf == "proxy" {
            Identity::Proxy
        } else if mode.as_deref() == Some("self") {
            Identity::SelfAccount
        } else {
            Identity::Bot
        };
        let disclosure = match verdict.as_deref() {
            Some("pass") => Disclosure::Pass,
            Some("rewritten") => Disclosure::Rewritten,
            Some("drafted") => Disclosure::Leak,
            Some("checking") => Disclosure::Checking,
            // `unchecked` (the check did not finish), and no basis at all
            _ => Disclosure::Unchecked,
        };
        let has = |o: &str| d.options.iter().any(|x| x == o);
        let (stage, send) = match d.kind.as_str() {
            // Disclosure and permission recovery do not replace the owner's review of an edit.
            // This is persisted on the card, so a fresh Engine makes the same decision.
            "approve" if d.result["recovery_review"] == true => (OutboundStage::HumanReview, None),
            "approve" if disclosure == Disclosure::Leak => (OutboundStage::LeakDraft, None),
            "approve" => (OutboundStage::Draft, has("yes").then(|| "yes".to_string())),
            "fail" if d.result["recovery"]["requires_user_action"] == true => {
                (OutboundStage::Blocked, None)
            }
            "fail" if status == "failed" && has("retry") => {
                (OutboundStage::SendFailed, Some("retry".to_string()))
            }
            "fail" => (OutboundStage::Blocked, None),
            _ => (OutboundStage::Handoff, None),
        };
        Ok(Subject::Chan(Outbound {
            identity,
            disclosure,
            external,
            customer: false,
            // the outbound path (steve-channels `Outbox::send`) marks what YOLO approved in your name
            ai_prefix: matches!(identity, Identity::SelfAccount | Identity::Proxy),
            retries: if stage == OutboundStage::SendFailed {
                self.yolo_retries(d)?
            } else {
                0
            },
            send_option: send,
            stage,
            composed: composed.as_ref().is_some_and(|c| c["outcome"] == "reply"),
            commitment: composed
                .as_ref()
                .is_some_and(|c| c["needs_commitment"] == true),
        }))
    }

    /// A mail draft card: always in your name; external unless every address is in your organisation.
    fn mail(&self, d: &Decision) -> ApiResult<Subject> {
        let row = self
            .yolo()
            .store()
            .read::<_, steve_store::StoreError>(|r| {
                let Some(audience) =
                    steve_store::mail::draft_audience(r, &d.workspace_id, &d.subject_id)?
                else {
                    return Ok(None);
                };
                let mut external = audience.recipients.is_empty();
                for address in &audience.recipients {
                    let domain = address.rsplit_once('@').map_or("", |(_, domain)| domain);
                    if !steve_store::parties::in_own_domain(r, &d.workspace_id, domain)? {
                        external = true;
                        break;
                    }
                }
                Ok(Some((audience.status, audience.verdict, external)))
            })?;
        let Some((status, verdict, external)) = row else {
            return Ok(Subject::Unverified("邮件草稿不在了".into()));
        };
        let disclosure = match verdict.as_deref() {
            Some("pass") => Disclosure::Pass,
            Some("leak") => Disclosure::Leak,
            _ => Disclosure::Unchecked,
        };
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
            retries: if stage == OutboundStage::SendFailed {
                self.yolo_retries(d)?
            } else {
                0
            },
            send_option: send,
            stage,
            composed: true,
            commitment: false,
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

#[cfg(test)]
mod checkout_tests {
    use super::*;

    fn g(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn a_local_checkout_tells_its_branch_and_settings() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("r");
        std::fs::create_dir_all(&r).unwrap();
        g(&r, &["init", "-q", "-b", "main"]);
        g(&r, &["config", "user.email", "t@example.invalid"]);
        g(&r, &["config", "user.name", "t"]);
        g(&r, &["commit", "-q", "--allow-empty", "-m", "a"]);
        let wt = t.path().join("wt");
        g(
            &r,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "steve/st-21",
                &wt.to_string_lossy(),
                "main",
            ],
        );
        g(&r, &["config", "push.default", "matching"]);
        let c = local_checkout(&wt).unwrap();
        assert_eq!(c.head.as_deref(), Some("steve/st-21"));
        let cfg = c.config.as_ref().unwrap();
        assert!(
            cfg.iter()
                .any(|(k, v)| k == "push.default" && v == "matching"),
            "{cfg:?}"
        );
        g(&wt, &["checkout", "-q", "--detach"]);
        assert_eq!(local_checkout(&wt).unwrap().head, None);
        assert_eq!(local_checkout(&t.path().join("none")), None);
    }
    #[test]
    fn channel_recovery_metadata_keeps_retry_a_human_action() {
        let kit = crate::testkit::Kit::new();
        steve_store::core_fixture::yolo::outbound(
            kit.db().store(),
            steve_store::core_fixture::yolo::Outbound {
                space: "refused-space",
                system: "fixture",
                external: "fixture",
                name: "fixture",
                message: "refused-message",
                status: "failed",
                text: Some("full draft"),
                basis: None,
            },
        );
        let mut d = kit
            .db()
            .store()
            .write(|w| {
                steve_store::DecisionRows(w).open(&steve_store::NewDecision {
                    workspace_id: "default".into(),
                    kind: "fail".into(),
                    subject_type: "chan_message".into(),
                    subject_id: "refused-message".into(),
                    item_id: None,
                    session_id: None,
                    question: "channel target refusal".into(),
                    draft: Some("full draft".into()),
                    options: Some(vec!["retry".into(), "rewrite".into(), "discard".into()]),
                    requested_by: "steve".into(),
                })
            })
            .unwrap();
        let yolo = super::super::Yolo::with_clock(
            kit.db().clone(),
            None,
            kit.home(),
            std::sync::Arc::new(steve_modules::FakeClock::new(1_000_000)),
        );
        let engine = Engine::new(kit.core.clone(), yolo);
        d.result = json!({"recovery":{"kind":"channel_refusal","requires_user_action":true}});
        let Subject::Chan(refused) = engine.chan(&d).unwrap() else {
            panic!()
        };
        assert_eq!(refused.stage, OutboundStage::Blocked);
        assert_eq!(refused.send_option, None);
        d.result = Value::Null;
        let Subject::Chan(ordinary) = engine.chan(&d).unwrap() else {
            panic!()
        };
        assert_eq!(ordinary.stage, OutboundStage::SendFailed);
        assert_eq!(ordinary.send_option.as_deref(), Some("retry"));
    }
}

#[cfg(test)]
mod recovery_review_tests {
    use super::*;

    /// Regression for the independent review: these are the actual persisted fields of a
    /// rewritten recovery card, with a passed disclosure check in an internal bot group.
    #[test]
    fn recovery_review_must_remain_a_human_review() {
        let kit = crate::testkit::Kit::new();
        steve_store::core_fixture::yolo::outbound(
            kit.db().store(),
            steve_store::core_fixture::yolo::Outbound {
                space: "review-space",
                system: "fixture",
                external: "fixture",
                name: "fixture",
                message: "review-message",
                status: "draft",
                text: Some("edited body"),
                basis: Some(("review-basis", "rewritten")),
            },
        );
        let d = kit
            .db()
            .store()
            .write(|w| -> ApiResult<Decision> {
                let d = steve_store::DecisionRows(w).open(&steve_store::NewDecision {
                    workspace_id: "default".into(),
                    kind: "approve".into(),
                    subject_type: "chan_message".into(),
                    subject_id: "review-message".into(),
                    item_id: None,
                    session_id: None,
                    question: "edited for review".into(),
                    draft: Some("edited body".into()),
                    options: Some(vec!["yes".into(), "no".into()]),
                    requested_by: "steve".into(),
                })?;
                steve_store::core_fixture::decision(
                    kit.db().store(),
                    &d.id,
                    steve_store::core_fixture::DecisionState::Result(
                        &json!({"recovery_review": true}),
                    ),
                );
                Ok(steve_store::DecisionRows(w).get(&d.id)?.unwrap())
            })
            .unwrap();
        // A new core and store connection, rather than an in-memory flag surviving the rewrite.
        let core = kit.reopened();
        let yolo = super::super::Yolo::with_clock(
            core.store().clone(),
            None,
            kit.home(),
            std::sync::Arc::new(steve_modules::FakeClock::new(1_000_000)),
        );
        let engine = Engine::new(core, yolo);
        let subject = engine.chan(&d).unwrap();
        let mut facts = Facts {
            decision_id: d.id.clone(),
            kind: d.kind.clone(),
            subject_type: d.subject_type.clone(),
            question: d.question.clone(),
            draft: d.draft.clone(),
            options: d.options.clone(),
            in_scope: true,
            present: false,
            subject,
        };
        let env = judge::Env {
            steve_home: kit.home().into(),
            home: kit.home().into(),
            tmp: judge::Env::tmp_dirs(),
        };
        let actual = judge::judge(&facts, &Policy::default(), &env);
        eprintln!("recovery review verdict: {actual:?}");
        assert!(
            !actual.is_auto(),
            "a passed disclosure check is not human approval: {actual:?}"
        );

        let mut ordinary = d;
        ordinary.result = Value::Null;
        facts.subject = engine.chan(&ordinary).unwrap();
        assert!(
            judge::judge(&facts, &Policy::default(), &env).is_auto(),
            "ordinary authorized internal bot drafts retain their automatic approval"
        );
    }
}
