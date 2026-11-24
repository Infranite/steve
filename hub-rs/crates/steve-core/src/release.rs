//! Releases (orchestrator.ts): run the project's release script detached; it restarts the Hub, so the outcome
//! is reported on the next boot from `releases/last.json`.
//!
//! Hand-off: the script builds first, then asks this Hub over loopback (with a one-off token) to drain — no new
//! turns, new prompts queue — and polls until the running turns have ended (bounded by the script), then restarts.
//!
//! The script is a Hub-host operation: it runs on the Hub's own machine, outlives the Hub and restarts it, so
//! only a project on the Hub's device (`projects.device_id = "hub"`) can release or roll back. A project on a
//! remote Node is refused before anything runs; an in-process Node dies with the Hub, so it can't host it either.

use crate::error::Result;
use crate::gateway::{jstr, truthy};
use crate::git::{self, Rebase};
use crate::occupancy::Hold;
use crate::orchestrator::To;
use crate::repo::{Decision, Item, WS};
use crate::util::{now, token};
use crate::{Core, Releasing, Tx};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::time::{Duration, SystemTime};

/// The Hub's own device (`projects.device_id`), the only one a release script runs for.
pub(crate) const HUB_DEVICE: &str = "hub";

/// The outcome of an approved release whose script is running.
const RELEASING: &str = "releasing";

/// Why a release that never reported a result failed.
pub(crate) const NO_RESULT: &str = "发布脚本没有报告结果就退出了";

/// A `pending` last.json older than this is given up on even if the lock looks held.
const PENDING_LIMIT_S: i64 = 30 * 60;

/// How long a drain hold lasts if the script neither restarts the Hub nor exits.
const HOLD_LIMIT: Duration = Duration::from_secs(20 * 60);

/// How often the running-turn count of a hold is published while it lasts.
const HOLD_TICK: Duration = Duration::from_secs(2);

/// What the Hub is holding for (`/api/summary` `hold`, SSE `release.hold`): the releasing item, the turns
/// still running that the restart waits for, and since when new turns wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HoldStatus {
    pub item: String,
    pub running: usize,
    pub since: i64,
}

/// The drain endpoint's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DrainStatus {
    pub idle: bool,
    pub running: usize,
}

impl Core {
    /// A release is running or the Hub holds new turns for one (YOLO waits with release cards).
    pub fn release_busy(&self) -> bool {
        let t = self.tx();
        t.st.releasing.is_some() || t.holding()
    }

    /// The release script's drain call (`/internal/release/drain`): start holding (once, if `start`) and report
    /// whether running turns are left; `None` for a wrong token or when no release is running.
    pub fn drain(&self, token: &str, start: bool) -> Result<Option<DrainStatus>> {
        self.tx().drain(token, start)
    }

    /// The release hold, while there is one.
    pub fn source_release_busy(&self) -> bool {
        self.tx().st.releasing.is_some()
    }
    pub fn hold_status(&self) -> Option<HoldStatus> {
        self.tx().hold_status()
    }

    /// `release.sh --rollback <target>` for the release of `item` (YOLO's undo); returns the log path.
    /// `action`: the YOLO undo that asked for it, marked undone once the rollback went through
    /// (`report_release` reads `releases/rollback.json`).
    pub fn rollback_release(&self, item: &str, target: &str, action: &str) -> Result<String> {
        if let Some(why) = self.rollback_refusal(target) {
            return Err(crate::error::msg(steve_i18n::t!(
                "core.release.rollback_refused",
                target = target,
                why = why
            )));
        }
        self.tx().start_rollback(item, target, action)
    }

    /// Why the installed release `target` can't run on this Hub's database, if it can't. The rule of
    /// ops/schema_gate.py, which `release.sh --rollback` applies again: the database records the oldest
    /// schema a Hub must know (`steve_store::Store::schema_compat`), the release's `release.json` the schema its Hub
    /// knows (`schema.target`). A release that doesn't say is refused. Asked before a rollback is offered
    /// or started, so a refused one is neither shown as possible nor counted as undone.
    pub fn rollback_refusal(&self, target: &str) -> Option<String> {
        if target.is_empty() || target.contains('/') || target.starts_with('.') {
            return Some(steve_i18n::t!("core.release.not_installed"));
        }
        let manifest = self
            .config()
            .home
            .join("releases")
            .join(target)
            .join("release.json");
        let knows = std::fs::read(&manifest)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|m| m.pointer("/schema/target").and_then(Value::as_i64));
        let db = match self.store().store().schema_compat() {
            Ok(c) => c,
            Err(e) => return Some(steve_i18n::t!("core.release.schema_unreadable", error = e)),
        };
        match knows {
            _ if db.min_reader == 0 => None,
            None => Some(steve_i18n::t!(
                "core.release.schema_unknown",
                schema = db.schema
            )),
            Some(k) if k < db.min_reader => Some(steve_i18n::t!(
                "core.release.schema_too_old",
                knows = k,
                schema = db.schema,
                min = db.min_reader
            )),
            Some(_) => None,
        }
    }

    /// Read `releases/last.json` and `releases/rollback.json` and report each final result once (boot, file
    /// watcher, tests).
    pub fn report_release(&self) -> Result<()> {
        let mut t = self.tx();
        t.report_release()?;
        t.report_rollback()
    }

    /// The canonical release watcher: metadata changes and pending scripts, every three seconds.
    pub fn watch_releases(
        self,
        clock: std::sync::Arc<dyn steve_runtime::Clock>,
    ) -> steve_runtime::Task {
        fn one(f: &Path) -> Option<SystemTime> {
            std::fs::metadata(f).and_then(|m| m.modified()).ok()
        }
        fn stamp(f: &Path) -> (Option<SystemTime>, Option<SystemTime>) {
            (one(f), one(&f.with_file_name("rollback.json")))
        }
        let file = self.config().releases_last();
        let last = std::sync::Arc::new(std::sync::Mutex::new(None));
        steve_runtime::every(
            "core.releases",
            clock,
            Duration::from_secs(3),
            steve_runtime::FirstRun::Immediate,
            move || {
                let (c, file, last) = (self.clone(), file.clone(), last.clone());
                async move {
                    if let Err(e) = tokio::task::spawn_blocking(move || {
                        let cur = stamp(&file);
                        let seen = last.lock().unwrap_or_else(|e| e.into_inner()).replace(cur);
                        // An unfinished script may exit without changing its result file.
                        if (seen.is_some_and(|seen| seen != cur) || pending(&file))
                            && let Err(e) = c.report_release()
                        {
                            tracing::error!("report release {e}");
                        }
                    })
                    .await
                    {
                        tracing::error!("report release task {e}");
                    }
                }
            },
        )
    }
}

/// `now` (and the agent's answer, prefixed) when an accepted item has nothing for a release to ship.
pub(crate) const NOTHING_TO_RELEASE: &str = "没有需要发布的改动";

impl Tx<'_> {
    /// An item in a release project whose branch has no commits beyond main and a clean worktree (or neither
    /// exists): accepting it finishes it, there is nothing to publish. False whenever git can't tell.
    pub(crate) fn nothing_to_release(&mut self, i: &Item) -> Result<bool> {
        let Some(branch) = i.branch.as_deref().filter(|b| !b.is_empty()) else {
            return Ok(false);
        };
        let Some(p) = self.db().project(i.project_id.as_deref())? else {
            return Ok(false);
        };
        if self.git_on_node(&i.id) {
            /* as its Node says in the facts this section decides on (on whichever device it is); none
            is can't tell */
            let branch = branch.to_string();
            return Ok(matches!(self.git_take(i), Some(Ok(Some(s)))
                if s.want.branch.as_deref() == Some(branch.as_str()) && s.changes == Some(false)));
        }
        if p.device_id != HUB_DEVICE {
            /* no Node for it and its paths name the other device's files: git here can't tell; the
            release request is refused */
            return Ok(false);
        }
        let wt = i
            .cwd
            .as_deref()
            .filter(|c| !c.is_empty() && *c != p.path)
            .map(Path::new);
        Ok(git::has_changes(Path::new(&p.path), wt, branch, "main") == Some(false))
    }

    /// Rebase the item's branch onto main in its worktree; on any problem the agent is sent back and nothing is released.
    pub(crate) fn rebase_for_release(
        &mut self,
        item_id: &str,
        branch: &str,
        to: &To,
    ) -> Result<bool> {
        let Some(i) = self.db().item(item_id)? else {
            return Ok(true);
        };
        /* a project on another device: its cwd is that device's, the Hub touches no git here and
        start_release refuses it */
        if self
            .db()
            .project(i.project_id.as_deref())?
            .is_some_and(|p| p.device_id != HUB_DEVICE)
        {
            return Ok(true);
        }
        if self.git_on_node(item_id) {
            if i.branch.as_deref() != Some(branch) {
                return Ok(true);
            }
            /* on its Node, off the lock: the release starts when the rebase comes back done */
            let Some(d) = self.open_release(item_id)? else {
                return Ok(false);
            };
            self.rebase_on_node(item_id, branch, to, &d.id);
            return Ok(false);
        }
        let Some(cwd) = i
            .cwd
            .as_deref()
            .filter(|c| !c.is_empty() && Path::new(c).exists())
        else {
            return Ok(true);
        };
        if i.branch.as_deref() != Some(branch) {
            return Ok(true);
        }
        self.rebase_outcome(
            item_id,
            branch,
            to,
            git::rebase_onto(Path::new(cwd), "main"),
        )
    }

    /// A rebase on the item's Node came back. The release starts (`true`) only if it went through
    /// and the item still waits for the release it was for: the same branch, its approval still
    /// open, no release running.
    pub(crate) fn rebased(
        &mut self,
        item_id: &str,
        branch: &str,
        to: &To,
        r: Rebase,
    ) -> Result<bool> {
        let Some(i) = self.db().item(item_id)? else {
            return Ok(false);
        };
        if i.branch.as_deref() != Some(branch)
            || self
                .open_release(item_id)?
                .is_none_or(|d| d.result.get("outcome").is_some())
            || self.st.releasing.is_some()
        {
            return Ok(false);
        }
        self.rebase_outcome(item_id, branch, to, r)
    }

    /// What a rebase before a release means: on any problem the agent is sent back and nothing is
    /// released (`false`).
    fn rebase_outcome(&mut self, item_id: &str, branch: &str, to: &To, r: Rebase) -> Result<bool> {
        let (m, note, reason) = match r {
            Rebase::Ok { moved } => {
                if moved {
                    self.db().timeline_add(
                        item_id,
                        "steve",
                        "sys",
                        &format!("已把 {branch} rebase 到最新的 main"),
                        None,
                    )?;
                }
                return Ok(true);
            }
            Rebase::Dirty(d) => {
                let m = format!(
                    "工作区有未提交的改动（{d}），没有发布。先把改动提交到 {branch}，再用 steve_publish 请求发布。"
                );
                (m.clone(), m, format!("工作区有未提交的改动（{d}）"))
            }
            Rebase::Conflict(d) => (
                format!(
                    "发布前要把 {branch} rebase 到最新的 main，自动 rebase 有冲突（{d}），已经撤销，没有发布。请在工作区里执行 git rebase main，解决冲突，重新跑类型检查和测试并提交，然后再用 steve_publish 请求发布。不要强推，也不要改 main。"
                ),
                format!("rebase 到 main 有冲突，已撤销，交回给它处理：{d}"),
                format!("rebase 到 main 有冲突（{d}）"),
            ),
            Rebase::Error(d) => {
                let m = format!(
                    "发布前 rebase 到 main 失败（{d}），没有发布。请检查分支 {branch} 的状态，处理后再用 steve_publish 请求发布。"
                );
                (m.clone(), m, format!("rebase 到 main 失败（{d}）"))
            }
        };
        self.db()
            .timeline_add(item_id, "steve", "sys", &note, None)?;
        self.not_released(item_id, &reason, "已交回给它处理")?;
        self.send(to, &m)?;
        Ok(false)
    }

    /// The newest approved release decision of an item still waiting for its outcome ("releasing", or none
    /// recorded yet), so a result is never written onto an older, settled one.
    fn open_release(&self, item_id: &str) -> Result<Option<Decision>> {
        Ok(self
            .db()
            .decisions_of_item(item_id)?
            .into_iter()
            .rev()
            .find(|d| {
                d.kind == "approve"
                    && d.subject_type == "release"
                    && d.status == steve_types::DecisionStatus::Resolved
                    && d.result.get("approved").is_some_and(truthy)
                    && d.result
                        .get("outcome")
                        .is_none_or(|o| o.as_str() == Some(RELEASING))
            }))
    }

    /// Record on the approved release decision what became of it: `releasing`, `not_released` (it never
    /// started), `failed` or `released`. The card shows this instead of a bare "approved".
    pub(crate) fn release_outcome(
        &mut self,
        item_id: &str,
        outcome: &str,
        extra: Value,
    ) -> Result<()> {
        let Some(d) = self.open_release(item_id)? else {
            return Ok(());
        };
        self.record_release_outcome(d, item_id, outcome, extra)
    }

    fn record_release_outcome(
        &mut self,
        d: Decision,
        item_id: &str,
        outcome: &str,
        extra: Value,
    ) -> Result<()> {
        let mut r = match d.result {
            Value::Object(o) => o,
            _ => Map::new(),
        };
        r.insert("outcome".into(), outcome.into());
        if let Value::Object(x) = extra {
            r.extend(x);
        }
        self.mark_release(item_id, &d.id, outcome, &r); // stats (ST3)
        self.db().decision_set_result(&d.id, &Value::Object(r))
    }

    /// An approved release that did not start: the decision says so and the user is told, not only the agent.
    fn not_released(&mut self, item_id: &str, reason: &str, then: &str) -> Result<()> {
        self.release_outcome(item_id, "not_released", json!({"reason": reason}))?;
        let ws = self.item(item_id)?.workspace_id;
        crate::notify::notify(
            self.db(),
            &ws,
            "release",
            "fail",
            &steve_i18n::stored(|| {
                steve_i18n::t!(
                    "core.notice.not_released_then",
                    reason = reason,
                    then = then
                )
            }),
            Some(item_id),
        )?;
        Ok(())
    }

    /// The release could not start at all (no script, another one running): nothing changed, a fail card asks what next.
    fn refuse_release(&mut self, item_id: &str, reason: &str, why: &str) -> Result<()> {
        self.not_released(item_id, reason, "等你决定怎么处理")?;
        self.fail(item_id, why)
    }

    pub(crate) fn start_release(&mut self, branch: &str, item_id: &str) -> Result<()> {
        if self.db().store().maintenance() {
            return Err(crate::error::msg(steve_i18n::t!(
                "core.release.maintenance"
            )));
        }
        let Some(item) = self.db().item(item_id)? else {
            return Ok(());
        };
        let Some(approval) = self
            .open_release(item_id)?
            .filter(|approval| approval.subject_id == branch)
        else {
            return Ok(());
        };
        if item.is_closed() || crate::cycle::stopping(&item) {
            return self.release_outcome(
                item_id,
                "not_released",
                json!({"reason": "item_cycle_closed"}),
            );
        }
        let p = self.db().project(item.project_id.as_deref())?;
        /* before the script lookup: its path names a file on the other device, not on this one */
        if let Some(device) = p
            .as_ref()
            .map(|p| p.device_id.as_str())
            .filter(|d| *d != HUB_DEVICE)
        {
            return self.refuse_release(
                item_id,
                &format!("项目在设备 {device} 上，发布脚本只能在 Hub 所在的设备上运行"),
                &format!(
                    "这个项目在设备 {device} 上；发布脚本只能在 Hub 所在的设备上运行，无法发布"
                ),
            );
        }
        let script = p
            .as_ref()
            .and_then(|p| p.release.as_ref())
            .map(|r| r.cmd.clone());
        let Some((p, script)) = p.zip(script).filter(|(_, s)| Path::new(s).exists()) else {
            let shown = item_project_script(self, &item.project_id)?;
            return self.refuse_release(
                item_id,
                &format!("发布脚本不存在（{shown}）"),
                &format!("这个项目的发布脚本不存在（{shown}），无法发布"),
            );
        };
        if let Some(r) = &self.st.releasing {
            let other = r.item.clone();
            return self.refuse_release(
                item_id,
                &format!("另一个发布（{other}）正在进行"),
                &format!("另一个发布（{other}）正在进行；它结束后选“继续”，让它重新请求发布"),
            );
        }
        let logs = self.cfg().home.join("logs");
        std::fs::create_dir_all(&logs)?;
        let log = logs.join(format!("release-{}.log", now()));
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)?;
        let tok = token();
        let mut cmd = std::process::Command::new("/bin/zsh");
        for key in [
            "STEVE_UPGRADE_CONTROL_TOKEN",
            "STEVE_SUPERVISOR_PID",
            "STEVE_UPGRADE_ID",
        ] {
            cmd.env_remove(key);
        }
        cmd.args([script.as_str(), branch, item_id])
            .stdin(std::process::Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .env("STEVE_REPO", &p.path)
            .env("STEVE_HOME", &self.cfg().home)
            .env("STEVE_PORT", self.cfg().port.to_string())
            .env("STEVE_DRAIN_TOKEN", &tok);
        /* detached: its own process group, so it outlives the Hub it restarts */
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        self.st.release_generation += 1;
        let generation = self.st.release_generation;
        let (operation, fresh) = self.record_item_operation(
            &item,
            crate::cycle::OP_RELEASE,
            Some(&approval.id),
            false,
            "running",
            "发布",
            json!({"log": log.to_string_lossy(), "branch": branch, "scope": self.scope_revision(item_id)?}),
        )?;
        if !fresh {
            return Ok(());
        }
        cmd.env("STEVE_RELEASE_OPERATION", &operation.id);
        self.st.releasing = Some(Releasing {
            token: tok,
            item: item_id.into(),
            operation: Some(operation.id.clone()),
            generation,
            hold_since: None,
        });
        let core = self.c.clone();
        match cmd.spawn() {
            Ok(mut child) => {
                #[cfg(test)]
                let pid = test_spawned::add(&self.cfg().home, child.id());
                /* the script ended while we are still running: it failed before the restart, or it only switched the
                 * web build (its result is in last.json) — lift the hold */
                crate::detach(move || {
                    let _ = child.wait();
                    #[cfg(test)]
                    test_spawned::remove(pid);
                    core.release_done(generation);
                });
            }
            Err(e) => {
                tracing::error!("release spawn {e}");
                self.st.releasing = None;
                let reason = format!("发布脚本没有启动（{e}）");
                self.settle_item_operation(&operation.id, "failed", json!({"reason": reason}))?;
                self.release_outcome(item_id, "not_released", json!({"reason": reason}))?;
                let ws = item.workspace_id.clone();
                crate::notify::notify(
                    self.db(),
                    &ws,
                    "release",
                    "fail",
                    &steve_i18n::stored(|| {
                        steve_i18n::t!("core.notice.not_released", reason = reason)
                    }),
                    Some(item_id),
                )?;
                return self.fail(item_id, &format!("{reason}，没有发布"));
            }
        }
        self.release_outcome(item_id, RELEASING, json!({"log": log.to_string_lossy()}))?;
        self.db().timeline_add(
            item_id,
            "steve",
            "sys",
            &format!("开始发布 {branch}，日志 {}", log.display()),
            None,
        )?;
        Ok(())
    }

    /// Rolls the user's Steve back to `target` with the project's release script
    /// (`release.sh --rollback <target>`): same lock, drain and restart as a release. Used to undo a
    /// release YOLO approved; returns the log path.
    pub(crate) fn start_rollback(
        &mut self,
        item_id: &str,
        target: &str,
        action: &str,
    ) -> Result<String> {
        if self.db().store().maintenance() {
            return Err(crate::error::msg(steve_i18n::t!(
                "core.release.maintenance"
            )));
        }
        let item = self.item(item_id)?;
        let p = self.db().project(item.project_id.as_deref())?;
        if let Some(device) = p
            .as_ref()
            .map(|p| p.device_id.as_str())
            .filter(|d| *d != HUB_DEVICE)
        {
            return Err(crate::error::msg(steve_i18n::t!(
                "core.release.remote_rollback",
                device = device
            )));
        }
        let script = p
            .as_ref()
            .and_then(|p| p.release.as_ref())
            .map(|r| r.cmd.clone());
        let Some((p, script)) = p.zip(script).filter(|(_, s)| Path::new(s).exists()) else {
            return Err(crate::error::msg(steve_i18n::t!(
                "core.release.no_script_rollback"
            )));
        };
        if let Some(r) = &self.st.releasing {
            return Err(crate::error::msg(steve_i18n::t!(
                "core.release.busy_rollback",
                item = r.item
            )));
        }
        let logs = self.cfg().home.join("logs");
        std::fs::create_dir_all(&logs)?;
        let log = logs.join(format!("rollback-{}.log", now()));
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)?;
        let tok = token();
        let mut cmd = std::process::Command::new("/bin/zsh");
        for key in [
            "STEVE_UPGRADE_CONTROL_TOKEN",
            "STEVE_SUPERVISOR_PID",
            "STEVE_UPGRADE_ID",
        ] {
            cmd.env_remove(key);
        }
        cmd.args([script.as_str(), "--rollback", target, item_id, action])
            .stdin(std::process::Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .env("STEVE_REPO", &p.path)
            .env("STEVE_HOME", &self.cfg().home)
            .env("STEVE_PORT", self.cfg().port.to_string())
            .env("STEVE_DRAIN_TOKEN", &tok);
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        self.st.release_generation += 1;
        let generation = self.st.release_generation;
        let mut child = cmd.spawn().map_err(|e| {
            crate::error::msg(steve_i18n::t!(
                "core.release.rollback_not_started",
                error = e
            ))
        })?;
        self.st.releasing = Some(Releasing {
            token: tok,
            item: item_id.into(),
            operation: None,
            generation,
            hold_since: None,
        });
        #[cfg(test)]
        let pid = test_spawned::add(&self.cfg().home, child.id());
        let core = self.c.clone();
        crate::detach(move || {
            let _ = child.wait();
            #[cfg(test)]
            test_spawned::remove(pid);
            core.release_done(generation);
        });
        let log = log.to_string_lossy().to_string();
        self.db().timeline_add(
            item_id,
            "steve",
            "sys",
            &format!("撤销 YOLO 批准的发布：回滚到 {target}，日志 {log}"),
            None,
        )?;
        Ok(log)
    }

    pub(crate) fn drain(&mut self, tok: &str, start: bool) -> Result<Option<DrainStatus>> {
        let Some(r) = self.st.releasing.clone() else {
            return Ok(None);
        };
        if tok.is_empty() || tok != r.token {
            return Ok(None);
        }
        if start && !self.held_by(Hold::Release) {
            self.set_hold(Hold::Release, true);
            if let Some(r) = self.st.releasing.as_mut() {
                r.hold_since = Some(now());
            }
            self.publish_hold();
            let n = self.running_turns();
            let note = if n > 0 {
                format!(
                    "新版本已构建好，等 {n} 个正在进行的回合结束后重启；新消息先排队，重启后送达"
                )
            } else {
                "新版本已构建好，没有正在进行的回合，马上重启".into()
            };
            self.db()
                .timeline_add(&r.item, "steve", "sys", &note, None)?;
            /* the script restarts us or exits; if neither happens, don't hold forever */
            let core = self.c.clone();
            let generation = r.generation;
            /* the web shows how many turns the restart still waits for */
            let watch = self.c.clone();
            crate::detach(move || {
                let mut last = None;
                loop {
                    std::thread::sleep(HOLD_TICK);
                    let t = watch.tx();
                    let Some(h) = t.hold_status().filter(|_| {
                        t.st.releasing
                            .as_ref()
                            .is_some_and(|r| r.generation == generation)
                    }) else {
                        return;
                    };
                    if last != Some(h.running) {
                        last = Some(h.running);
                        t.publish_hold();
                    }
                }
            });
            crate::detach(move || {
                std::thread::sleep(HOLD_LIMIT);
                let mut t = core.tx();
                if t.st
                    .releasing
                    .as_ref()
                    .is_some_and(|r| r.generation == generation)
                    && let Err(e) = t.end_hold()
                {
                    tracing::error!("end hold {e}");
                }
            });
        }
        let n = self.running_turns();
        Ok(Some(DrainStatus {
            idle: n == 0,
            running: n,
        }))
    }

    pub(crate) fn hold_status(&self) -> Option<HoldStatus> {
        let r = self.st.releasing.as_ref()?;
        if !self.held_by(Hold::Release) {
            return None;
        }
        Some(HoldStatus {
            item: r.item.clone(),
            running: self.running_turns(),
            since: r.hold_since.unwrap_or_else(now),
        })
    }

    /// SSE `release.hold` with the current hold (or `null`).
    pub(crate) fn publish_hold(&self) {
        let hold = self
            .hold_status()
            .and_then(|h| serde_json::to_value(h).ok());
        self.db().store().publish(steve_types::DomainEvent::from(
            steve_types::DevicesEvent::ReleaseHold {
                hold: {
                    let value: Option<serde_json::Value> = hold;
                    value.map(|value| {
                        steve_types::RawJson::from_str(
                            &serde_json::to_string(&value).expect("JSON value"),
                        )
                        .expect("validated JSON")
                    })
                },
            },
        ));
    }

    pub(crate) fn end_hold(&mut self) -> Result<()> {
        if !self.held_by(Hold::Release) {
            return Ok(());
        }
        self.set_hold(Hold::Release, false);
        self.publish_hold();
        self.flush_queued()
    }

    /// `releases/rollback.json` (ops/release.sh `--rollback`), once: noted on the item it undid and as a
    /// notification; when it went through, the YOLO undo that asked for it becomes undone. A rollback is
    /// never the item's release: nothing about the item's state changes.
    pub(crate) fn report_rollback(&mut self) -> Result<()> {
        let f = self.cfg().home.join("releases/rollback.json");
        let Ok(raw) = std::fs::read_to_string(&f) else {
            return Ok(());
        };
        let Ok(mut r) = serde_json::from_str::<Value>(&raw) else {
            return Ok(());
        };
        if r.get("reported").is_some_and(truthy) {
            return Ok(());
        }
        if let Some(o) = r.as_object_mut() {
            o.insert("reported".into(), Value::Bool(true));
        }
        std::fs::write(&f, pretty1(&r))?;
        let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default();
        let ok = r.get("ok").is_some_and(truthy);
        let text = if ok {
            format!("已回滚到 {}（之前是 {}）", s("version"), s("prev"))
        } else {
            format!(
                "回滚到 {} 没有成功：{}",
                s("version"),
                Some(s("error"))
                    .filter(|e| !e.is_empty())
                    .unwrap_or("健康检查没有通过")
            )
        };
        let item = Some(s("item"))
            .filter(|i| !i.is_empty())
            .map(|i| self.db().item(i))
            .transpose()?
            .flatten();
        if let Some(i) = &item {
            self.db().timeline_add(&i.id, "steve", "sys", &text, None)?;
        }
        if ok && !s("action").is_empty() {
            self.db()
                .store()
                .write(|w| steve_store::yolo::stamp_undone(w, s("action"), now()))?;
        }
        let ws = item
            .as_ref()
            .map(|i| i.workspace_id.clone())
            .unwrap_or_else(|| WS.into());
        crate::notify::notify(
            self.db(),
            &ws,
            "release",
            if ok { "success" } else { "fail" },
            &text,
            item.as_ref().map(|i| i.id.as_str()),
        )?;
        Ok(())
    }

    pub(crate) fn report_release(&mut self) -> Result<()> {
        let f = self.cfg().releases_last();
        let Ok(raw) = std::fs::read_to_string(&f) else {
            return Ok(());
        };
        let Ok(mut r) = serde_json::from_str::<Value>(&raw) else {
            return Ok(());
        };
        if r.get("reported").is_some_and(truthy) {
            return Ok(());
        }
        /* still pending: wait while its script runs; once no script holds the release lock any more (or it has
         * been far too long) nothing will ever report it — it failed */
        let abandoned = r.get("pending").is_some_and(truthy);
        if abandoned {
            let at = r.get("at").and_then(Value::as_i64);
            if !pending_abandoned(&f, at, now() / 1000) {
                return Ok(());
            }
        }
        if let Some(o) = r.as_object_mut()
            && abandoned
        {
            o.insert("ok".into(), Value::Bool(false));
            o.insert("pending".into(), Value::Bool(false));
            o.insert("error".into(), NO_RESULT.into());
        }
        /* JS template literals: undefined → "undefined", `??` falls back on null and undefined */
        let tpl = |k: &str| r.get(k).map(jstr).unwrap_or_else(|| "undefined".into());
        let or = |k: &str, d: &str| match r.get(k) {
            None | Some(Value::Null) => d.to_string(),
            Some(v) => jstr(v),
        };
        /* `||` falls back on every falsy value */
        let or_falsy = |k: &str, d: &str| match r.get(k).filter(|v| truthy(v)) {
            Some(v) => jstr(v),
            None => d.to_string(),
        };
        /* kind (ops/release.sh): "hub" restarted the Hub, "web" only switched the web build, "hub+web" did both; absent before the split */
        let kind = r.get("kind").and_then(Value::as_str).unwrap_or_default();
        let what = match kind {
            "web" => "前端",
            "hub+web" => "Hub 和前端",
            _ => "",
        };
        /* degraded: the new Hub migrated the database past what the previous one reads, so it stays */
        let degraded = r.get("degraded").is_some_and(truthy);
        let back = if degraded {
            "没有回滚，新版本保留（降级运行）".to_string()
        } else if kind == "hub+web" && r.get("hub_ok").is_some_and(truthy) {
            format!(
                "Hub 已是新版本，前端仍是 {}",
                or_falsy("web_prev", "上一个版本")
            )
        } else {
            format!("已回到 {}", or_falsy("prev", "上一个版本"))
        };
        let text = if abandoned {
            /* nobody knows which version is live now: don't claim a rollback */
            format!(
                "发布{what} {} 失败：{NO_RESULT}，不确定现在是哪个版本；请看发布日志 {}",
                or("version", ""),
                or("log", "")
            )
            .replace("  ", " ")
        } else if r.get("ok").is_some_and(truthy) {
            format!(
                "已发布{what}新版本 {}{}",
                tpl("version"),
                if kind == "web" {
                    "（Hub 没有重启，刷新页面生效）"
                } else {
                    ""
                }
            )
        } else {
            format!(
                "发布{what} {} 失败，{back}：{}",
                or("version", ""),
                or("error", "健康检查没有通过")
            )
        };
        let item = match r.get("item").filter(|v| truthy(v)) {
            Some(v) => self.db().item(&jstr(v))?,
            None => None,
        };
        let version = r.get("version").filter(|v| truthy(v)).map(jstr);
        let ok = r.get("ok").is_some_and(truthy);
        let mut fresh = item.is_none();
        if let Some(i) = &item
            && let Some(operation) = r.get("operation").and_then(Value::as_str)
            && let Some((op, approval)) = self.running_release(operation, &i.id)?
        {
            let current = self.release_is_current(i, &op)?;
            let (state, outcome, mut result) = if abandoned {
                ("unknown", "unknown", json!({"reason": NO_RESULT}))
            } else if ok {
                ("succeeded", "released", json!({"prev": r.get("prev")}))
            } else {
                (
                    "failed",
                    "failed",
                    json!({
                        "reason": or("error", "健康检查没有通过"),
                        "note": back,
                        "hub_ok": r.get("hub_ok").is_some_and(truthy),
                        "web_ok": r.get("web_ok").is_some_and(truthy),
                        "degraded": degraded,
                    }),
                )
            };
            result["version"] = json!(version);
            result["kind"] = json!(kind);
            let store = self.db().store().clone();
            fresh = store.write(|w| -> Result<bool> {
                if !steve_store::operation_repo::set(
                    w,
                    &op.id,
                    state,
                    &json!({"version": version, "kind": kind, "commit": r.get("commit"),
                        "prev": r.get("prev"), "error": r.get("error"), "log": r.get("log")}),
                )? {
                    return Ok(false);
                }
                self.record_release_outcome(approval, &i.id, outcome, result)?;
                if ok {
                    self.db().timeline_add(&i.id, "steve", "sys", &text, None)?;
                    if current {
                        crate::thread::flowback::flow_quiet(
                            self.db(),
                            &i.id,
                            crate::thread::flowback::Update {
                                kind: "release",
                                text: version.clone().unwrap_or_default(),
                                ..Default::default()
                            },
                        );
                    }
                } else if current {
                    if abandoned {
                        self.release_unknown(&i.id, &text)?;
                    } else {
                        self.fail(&i.id, &text)?;
                    }
                }
                Ok(true)
            })?;
        } else if item.is_some() {
            metrics::counter!("steve_item_stale_delivery_total").increment(1);
        }
        if fresh {
            let ws = item
                .as_ref()
                .map(|i| i.workspace_id.clone())
                .unwrap_or_else(|| WS.into());
            let level = if ok { "success" } else { "fail" };
            crate::notify::notify(
                self.db(),
                &ws,
                "release",
                level,
                &text,
                item.as_ref().map(|i| i.id.as_str()),
            )?;
        }
        /* only now the file says it was reported: a crash before this reports it again, harmlessly */
        if let Some(o) = r.as_object_mut() {
            o.insert("reported".into(), Value::Bool(true));
        }
        std::fs::write(&f, pretty1(&r))?;
        Ok(())
    }
}

impl Tx<'_> {
    fn running_release(
        &self,
        operation: &str,
        item: &str,
    ) -> Result<Option<(steve_store::ItemOperation, Decision)>> {
        let Some(op) = self
            .db()
            .store()
            .items()
            .operation(operation)?
            .filter(|op| {
                op.item_id == item && op.kind == crate::cycle::OP_RELEASE && op.status == "running"
            })
        else {
            return Ok(None);
        };
        let Some(approval) = op.effect_id.as_deref() else {
            return Ok(None);
        };
        let Some(approval) = self
            .db()
            .decision(approval)?
            .filter(|d| d.item_id.as_deref() == Some(item) && d.subject_type == "release")
        else {
            return Ok(None);
        };
        Ok(Some((op, approval)))
    }

    fn release_is_current(
        &self,
        item: &Item,
        operation: &steve_store::ItemOperation,
    ) -> Result<bool> {
        Ok(!item.is_closed()
            && operation.cycle == item.cycle
            && operation.revision == item.delivery_revision
            && operation.evidence.get("scope").and_then(Value::as_i64)
                == Some(self.scope_revision(&item.id)?))
    }

    /// The process we started exited without reporting its operation.
    fn unreported(&mut self, item: &str, operation: &str) -> Result<()> {
        let Some((op, approval)) = self.running_release(operation, item)? else {
            return Ok(());
        };
        let i = self.item(item)?;
        let why = format!("{NO_RESULT}，不知道有没有发布；请看发布日志");
        let current = self.release_is_current(&i, &op)?;
        let store = self.db().store().clone();
        store.write(|w| {
            steve_store::operation_repo::set(w, &op.id, "failed", &json!({"reason": NO_RESULT}))?;
            self.record_release_outcome(approval, item, "failed", json!({"reason": NO_RESULT}))?;
            if current {
                self.fail(item, &why)?;
            }
            crate::notify::notify(
                self.db(),
                &i.workspace_id,
                "release",
                "fail",
                &why,
                Some(item),
            )?;
            Ok(())
        })
    }
}

/// last.json says a release is in progress.
fn pending(f: &Path) -> bool {
    std::fs::read_to_string(f)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .is_some_and(|r| {
            r.get("pending").is_some_and(truthy) && !r.get("reported").is_some_and(truthy)
        })
}

/// A pending release nobody will finish: no live process holds `releases/.release.lock`, or it was
/// written (`at`, epoch seconds) longer than [`PENDING_LIMIT_S`] ago.
pub(crate) fn pending_abandoned(last: &Path, at: Option<i64>, now_s: i64) -> bool {
    if at.is_some_and(|at| now_s - at > PENDING_LIMIT_S) {
        return true;
    }
    let lock = last.with_file_name(".release.lock");
    !lock_holder(&lock).is_some_and(alive)
}

/// The pid holding a write lock on `f` (ops/release.sh takes it with `zsystem flock`, an fcntl lock).
pub(crate) fn lock_holder(f: &Path) -> Option<i32> {
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f)
        .ok()?;
    // SAFETY: a zeroed flock is a valid argument; F_GETLK only fills it in.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_WRLCK as libc::c_short;
    fl.l_whence = libc::SEEK_SET as libc::c_short;
    // SAFETY: the fd is open for the duration of the call and `fl` outlives it.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut fl) };
    (rc == 0 && fl.l_type != libc::F_UNLCK as libc::c_short).then_some(fl.l_pid)
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    pid > 0
        && (unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

/// `${script ?? "没有配置"}` for the missing-script failure.
fn item_project_script(t: &Tx, project: &Option<String>) -> Result<String> {
    Ok(t.db()
        .project(project.as_deref())?
        .and_then(|p| p.release)
        .map(|r| r.cmd)
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| "没有配置".into()))
}

/// `JSON.stringify(v, null, 1)`
fn pretty1(v: &Value) -> String {
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    if v.serialize(&mut ser).is_err() {
        return v.to_string();
    }
    String::from_utf8(buf).unwrap_or_else(|_| v.to_string())
}

impl Core {
    /// The release script exited while this Hub is still running: clear the release and lift the hold.
    fn release_done(&self, generation: u64) {
        let mut t = self.tx();
        if t.st
            .releasing
            .as_ref()
            .is_some_and(|r| r.generation == generation)
        {
            let release = t.st.releasing.take();
            if let Err(e) = t.end_hold() {
                tracing::error!("end hold {e}");
            }
            /* its result may have just been written: report it now rather than on the next poll; a script that
             * ended without one must not leave the item "正在发布" for ever */
            if let Err(e) = t.report_release().and_then(|()| {
                if let Some(release) = release
                    && let Some(operation) = release.operation
                {
                    t.unreported(&release.item, &operation)?;
                }
                Ok(())
            }) {
                tracing::error!("release done {e}");
            }
        }
    }
}

impl Tx<'_> {
    /// A release whose result nobody knows: the requirement waits for you to check, nothing retries it.
    pub(crate) fn release_unknown(&mut self, item_id: &str, text: &str) -> Result<()> {
        let i = self.item(item_id)?;
        crate::lifecycle::supersede(self.db(), item_id, &["fail"], "发布结果未知")?;
        self.db().decision_create(crate::repo::NewDecision {
            workspace_id: i.workspace_id.clone(),
            kind: "fail".into(),
            subject_type: "item".into(),
            subject_id: item_id.into(),
            item_id: Some(item_id.into()),
            question: format!("{text}。请核对线上版本：已经上线，还是需要重新发布？"),
            options: Some(vec![
                "release_live".into(),
                "release_retry".into(),
                "look".into(),
            ]),
            requested_by: "steve".into(),
            ..Default::default()
        })?;
        Ok(())
    }
}

/// Release scripts spawned in tests, by `STEVE_HOME`. A script runs in its own process group so it outlives the
/// Hub it restarts; in a test nothing restarts, so the test kit kills the groups it started when it drops,
/// before its temp dir goes (else the fake scripts poll a deleted directory forever).
#[cfg(test)]
pub(crate) mod test_spawned {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static SPAWNED: Mutex<Vec<(PathBuf, u32)>> = Mutex::new(Vec::new());

    fn list() -> std::sync::MutexGuard<'static, Vec<(PathBuf, u32)>> {
        SPAWNED.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn add(home: &Path, pid: u32) -> u32 {
        list().push((home.to_path_buf(), pid));
        pid
    }
    /// The script was reaped: its pid may be reused, so it must never be killed by number again.
    pub(crate) fn remove(pid: u32) {
        list().retain(|(_, p)| *p != pid);
    }
    /// Process groups of scripts started under `home` and not yet reaped.
    pub(crate) fn of(home: &Path) -> Vec<u32> {
        list()
            .iter()
            .filter(|(h, _)| h == home)
            .map(|(_, p)| *p)
            .collect()
    }
    /// SIGKILL every process group started under `home`; the list lock is held so a reap can't race the kill.
    pub(crate) fn kill(home: &Path) {
        let mut l = list();
        l.retain(|(h, pid)| {
            if h != home {
                return true;
            }
            if let Ok(g) = i32::try_from(*pid) {
                // SAFETY: plain kill(2) on a process group this test started and that was not reaped yet.
                unsafe { libc::kill(-g, libc::SIGKILL) };
            }
            false
        });
    }
}
