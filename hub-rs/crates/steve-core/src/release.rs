//! Releases (orchestrator.ts): run the project's release script detached; it restarts the Hub, so the outcome
//! is reported on the next boot from `releases/last.json`.
//!
//! Hand-off: the script builds first, then asks this Hub over loopback (with a one-off token) to drain — no new
//! turns, new prompts queue — and polls until the running turns have ended (bounded by the script), then restarts.
//!
//! The script is a Hub-host operation: it runs on the Hub's own machine, outlives the Hub and restarts it, so
//! only a project on the Hub's device (`projects.device_id = "hub"`) can release or roll back. A project on a
//! remote Node is refused before anything runs; an in-process Node dies with the Hub, so it can't host it either.

use crate::cycle::{Closing, OP_RELEASE, fingerprint_head};
use crate::error::Result;
use crate::gateway::{jstr, truthy};
use crate::git::{self, Rebase};
use crate::occupancy::Hold;
use crate::orchestrator::To;
use crate::patch;
use crate::repo::{Decision, Item, WS};
use crate::util::{now, token};
use crate::{Core, Releasing, Tx};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::time::{Duration, SystemTime};

/// The Hub's own device (`projects.device_id`), the only one a release script runs for.
const HUB_DEVICE: &str = "hub";

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

/// What the Hub is holding for (`/api/state` `hold`, SSE `release.hold`): the releasing item, the turns
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
    /// schema a Hub must know (`steve_store::db::compat`), the release's `release.json` the schema its Hub
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
        let db = match self.store().store().with_conn(steve_store::db::compat) {
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

    /// Poll `releases/last.json` every 3 s and report when it changes (`fs.watchFile`). Runs forever.
    pub async fn watch_releases(self) {
        let f = self.config().releases_last();
        fn one(f: &Path) -> Option<SystemTime> {
            std::fs::metadata(f).and_then(|m| m.modified()).ok()
        }
        fn stamp(f: &Path) -> (Option<SystemTime>, Option<SystemTime>) {
            (one(f), one(&f.with_file_name("rollback.json")))
        }
        let mut last: (Option<SystemTime>, Option<SystemTime>) = stamp(&f);
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        loop {
            tick.tick().await;
            /* the files are read and the result reported (the core's lock, the store) off the runtime's
             * workers */
            let (c, f, seen) = (self.clone(), f.clone(), last);
            let looked = tokio::task::spawn_blocking(move || {
                let cur = stamp(&f);
                /* a pending result is looked at again on every tick: its script may die without touching the file */
                if (cur != seen || pending(&f))
                    && let Err(e) = c.report_release()
                {
                    tracing::error!("report release {e}");
                }
                cur
            })
            .await;
            match looked {
                Ok(cur) => last = cur,
                Err(e) => tracing::error!("report release task {e}"),
            }
        }
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
        if p.device_id != HUB_DEVICE {
            /* its paths name the other device's files: git here can't tell; the release request is refused */
            return Ok(false);
        }
        if self.git_on_node(&i.id) {
            /* as its Node says in the facts this section decides on; none is can't tell */
            let branch = branch.to_string();
            return Ok(matches!(self.git_take(i), Some(Ok(Some(s)))
                if s.want.branch.as_deref() == Some(branch.as_str()) && s.changes == Some(false)));
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
        let (m, note, now_text, reason) = match r {
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
                (
                    m.clone(),
                    m,
                    "发布前要先处理分支",
                    format!("工作区有未提交的改动（{d}）"),
                )
            }
            Rebase::Conflict(d) => (
                format!(
                    "发布前要把 {branch} rebase 到最新的 main，自动 rebase 有冲突（{d}），已经撤销，没有发布。请在工作区里执行 git rebase main，解决冲突，重新跑类型检查和测试并提交，然后再用 steve_publish 请求发布。不要强推，也不要改 main。"
                ),
                format!("rebase 到 main 有冲突，已撤销，交回给它处理：{d}"),
                "要先 rebase 解决冲突",
                format!("rebase 到 main 有冲突（{d}）"),
            ),
            Rebase::Error(d) => {
                let m = format!(
                    "发布前 rebase 到 main 失败（{d}），没有发布。请检查分支 {branch} 的状态，处理后再用 steve_publish 请求发布。"
                );
                (
                    m.clone(),
                    m,
                    "发布前要先处理分支",
                    format!("rebase 到 main 失败（{d}）"),
                )
            }
        };
        self.db()
            .timeline_add(item_id, "steve", "sys", &note, None)?;
        self.db()
            .item_patch(item_id, patch![stage: "收尾", now: now_text])?;
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
                    && d.status == "resolved"
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
            &format!("批准了发布，但没有发布：{reason}；{then}"),
            Some(item_id),
        )?;
        Ok(())
    }

    /// The release could not start at all (no script, another one running): nothing changed, a fail card asks what next.
    fn refuse_release(&mut self, item_id: &str, reason: &str, why: &str) -> Result<()> {
        self.db()
            .item_patch(item_id, patch![stage: "收尾", now: "没有发布"])?;
        self.not_released(item_id, reason, "等你决定怎么处理")?;
        self.fail(item_id, why)
    }

    /// A release that started but ended without going live: reopen the item with a fail card.
    pub(crate) fn release_failed(
        &mut self,
        item_id: &str,
        now_text: &str,
        why: &str,
        extra: Value,
    ) -> Result<()> {
        self.release_outcome(item_id, "failed", extra)?;
        let i = self.item(item_id)?;
        if i.phase == crate::cycle::CLOSED {
            /* a release after the cycle closed is a fact of its own: the cycle stays as it closed */
            return Ok(());
        }
        /* the requirement is still open: a fail card asks what next (retry, rework, give up) */
        if let Some(op) = self.lc_open_op(item_id, crate::cycle::OP_RELEASE)? {
            self.lc_op_set(&op.id, "pending", json!({"last_failure": why}))?;
        }
        self.db()
            .item_patch(item_id, patch![stage: "收尾", now: now_text])?;
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
        self.st.releasing = Some(Releasing {
            token: tok,
            item: item_id.into(),
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
                self.release_outcome(item_id, "not_released", json!({"reason": reason}))?;
                let ws = item.workspace_id.clone();
                crate::notify::notify(
                    self.db(),
                    &ws,
                    "release",
                    "fail",
                    &format!("批准了发布，但没有发布：{reason}"),
                    Some(item_id),
                )?;
                self.db()
                    .item_patch(item_id, patch![stage: "收尾", now: "没有发布"])?;
                return self.fail(item_id, &format!("{reason}，没有发布"));
            }
        }
        self.release_outcome(item_id, RELEASING, json!({"log": log.to_string_lossy()}))?;
        if let Some(op) = self.lc_open_op(item_id, crate::cycle::OP_RELEASE)? {
            self.lc_op_set(
                &op.id,
                "running",
                json!({"log": log.to_string_lossy(), "branch": branch}),
            )?;
        }
        self.db()
            .item_patch(item_id, patch![stage: "收尾", now: "正在发布"])?;
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
        self.db()
            .publish(crate::repo::HubEvent::ReleaseHold { hold });
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
            self.db().exec(
                "UPDATE yolo_actions SET undone = ? WHERE id = ? AND undone IS NULL",
                &[&now(), &s("action")],
            )?;
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
        /* one attempt per result, keyed by the result and the release it answers (the item's newest approved
         * release card): a result reported again (a crash between the database and the file) is recognised and
         * told nobody twice, while a later release that leaves an identical file is still its own attempt */
        let card = match &item {
            Some(i) => self
                .db()
                .decisions_of_item(&i.id)?
                .into_iter()
                .rev()
                .find(|d| {
                    d.kind == "approve"
                        && d.subject_type == "release"
                        && d.result.get("approved").is_some_and(truthy)
                })
                .map(|d| d.id),
            None => None,
        };
        let effect = format!(
            "{}:{}:{}:{}",
            card.unwrap_or_default(),
            version.clone().unwrap_or_default(),
            r.get("at").map(jstr).unwrap_or_default(),
            kind
        );
        let mut fresh = true;
        if let Some(i) = &item {
            let status = if abandoned {
                "unknown"
            } else if ok {
                "succeeded"
            } else {
                "failed"
            };
            let (_, new) = self.lc_op_add(
                i,
                crate::cycle::OP_RELEASE,
                Some(&format!("attempt:{effect}")),
                false,
                status,
                "发布",
                json!({"version": version, "kind": kind, "commit": r.get("commit"), "prev": r.get("prev"),
                       "error": r.get("error"), "log": r.get("log")}),
            )?;
            fresh = new;
        }
        if let (Some(i), true) = (&item, fresh) {
            let required = self.lc_open_op(&i.id, crate::cycle::OP_RELEASE)?;
            if ok {
                self.db().timeline_add(&i.id, "steve", "sys", &text, None)?;
                /* `prev`: what a rollback of this release goes back to (YOLO's undo) */
                let prev = r.get("prev").filter(|v| truthy(v)).map(jstr);
                self.release_outcome(
                    &i.id,
                    "released",
                    json!({"version": version, "kind": kind, "prev": prev}),
                )?;
                crate::thread::flowback::flow_quiet(
                    self.db(),
                    &i.id,
                    crate::thread::flowback::Update {
                        kind: "release",
                        text: version.clone().unwrap_or_default(),
                        ..Default::default()
                    },
                );
                /* a release is evidence for the closing gate, never a close by itself */
                if let Some(op) = required {
                    let head = crate::cycle::closing_of(i)
                        .and_then(|c| c.fingerprint)
                        .map(|f| crate::cycle::fingerprint_head(&f).to_string());
                    let attempt = steve_store::ItemOperation {
                        evidence: json!({"commit": r.get("commit")}),
                        ..op.clone()
                    };
                    match self.release_holds(i, &attempt, head.as_deref()) {
                        Some(true) => {
                            self.lc_op_set(
                                &op.id,
                                "succeeded",
                                json!({"version": version, "commit": r.get("commit"), "kind": kind}),
                            )?;
                        }
                        Some(false) => {
                            self.lc_op_set(
                                &op.id,
                                "pending",
                                json!({"last_failure": "发布的版本不包含验收的结果"}),
                            )?;
                            self.fail(&i.id, "发布的版本不包含验收的结果，需要重新发布")?;
                        }
                        None => {
                            /* its Node says once its facts are here (`node_git`) */
                            self.st.git_proofs.entry(i.id.clone()).or_default().push(
                                crate::node_git::PendingProof {
                                    op: op.id.clone(),
                                    head: head.clone().unwrap_or_default(),
                                    commit: attempt.evidence["commit"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .into(),
                                    version: json!(version),
                                    kind: json!(kind),
                                },
                            );
                            self.git_gather(i);
                        }
                    }
                }
            } else if abandoned {
                /* nobody knows what is live: not a failure, a result to check */
                self.release_outcome(
                    &i.id,
                    "unknown",
                    json!({"reason": NO_RESULT, "version": version, "kind": kind}),
                )?;
                if let Some(op) = required {
                    self.lc_op_set(&op.id, "unknown", json!({"note": NO_RESULT}))?;
                }
                if i.phase != crate::cycle::CLOSED {
                    self.release_unknown(&i.id, &text)?;
                }
            } else {
                let hub_only = kind == "hub+web" && r.get("hub_ok").is_some_and(truthy);
                let extra = json!({
                    "reason": or("error", "健康检查没有通过"),
                    "note": back,
                    "version": version,
                    "kind": kind,
                    "hub_ok": r.get("hub_ok").is_some_and(truthy),
                    "web_ok": r.get("web_ok").is_some_and(truthy),
                    "degraded": degraded,
                });
                let now_text = if degraded {
                    "发布失败，新版本降级运行"
                } else if hub_only {
                    "Hub 已发布，前端发布失败"
                } else {
                    "发布失败"
                };
                self.release_failed(&i.id, now_text, &text, extra)?;
            }
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
    /// The release script of `item` exited and nothing reported a result for it.
    fn unreported(&mut self, item: &str) -> Result<()> {
        let pending = self
            .open_release(item)?
            .is_some_and(|d| d.result.get("outcome").and_then(Value::as_str) == Some(RELEASING));
        if !pending {
            return Ok(());
        }
        let why = format!("{NO_RESULT}，不知道有没有发布；请看发布日志");
        let ws = self.item(item)?.workspace_id;
        crate::notify::notify(self.db(), &ws, "release", "fail", &why, Some(item))?;
        self.release_failed(item, "发布失败", &why, json!({"reason": NO_RESULT}))
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
            let item = t.st.releasing.take().map(|r| r.item);
            if let Err(e) = t.end_hold() {
                tracing::error!("end hold {e}");
            }
            /* its result may have just been written: report it now rather than on the next poll; a script that
             * ended without one must not leave the item "正在发布" for ever */
            if let Some(item) = item
                && let Err(e) = t.report_release().and_then(|()| t.unreported(&item))
            {
                tracing::error!("release done {e}");
            }
        }
    }
}

/// Does a succeeded release `op` (its `evidence.commit`) contain the frozen `head`? Without a frozen
/// head (no worktree) any release of the cycle counts; without a commit to compare, none does.
pub(crate) fn release_contains(
    i: &Item,
    op: &steve_store::ItemOperation,
    head: Option<&str>,
) -> bool {
    let Some(head) = head.filter(|h| !h.is_empty()) else {
        return true;
    };
    let Some(commit) = op.evidence["commit"].as_str().filter(|c| !c.is_empty()) else {
        return false;
    };
    if commit.starts_with(head) || head.starts_with(commit) {
        return true;
    }
    let Some(cwd) = i.cwd.as_deref().filter(|c| Path::new(c).exists()) else {
        return false;
    };
    crate::git::command()
        .args(["-C", cwd, "merge-base", "--is-ancestor", head, commit])
        .output()
        .is_ok_and(|o| o.status.success())
}

impl Tx<'_> {
    /// [`release_contains`] for the item where its git is: on a Node as its facts say (`None`:
    /// they don't say yet).
    pub(crate) fn release_holds(
        &self,
        i: &Item,
        op: &steve_store::ItemOperation,
        head: Option<&str>,
    ) -> Option<bool> {
        if !self.git_on_node(&i.id) {
            return Some(release_contains(i, op, head));
        }
        let Some(head) = head.filter(|h| !h.is_empty()) else {
            return Some(true);
        };
        let Some(commit) = op.evidence["commit"].as_str().filter(|c| !c.is_empty()) else {
            return Some(false);
        };
        if commit.starts_with(head) || head.starts_with(commit) {
            return Some(true);
        }
        self.git_contains(&i.id, head, commit)
    }

    /// The release adapter of the closing gate. A release project: the accepted result must be live. Satisfied by a release of this cycle whose
    /// commit contains the frozen `HEAD` (the agent may have released it already); otherwise a pending
    /// requirement, and the agent is asked to publish. A branch with nothing to release is not proof of
    /// a deployment: without such a release it stays a requirement you can waive. Once you said no to
    /// releasing in this cycle (and nothing since said yes), every later result of it is waived too:
    /// your no holds for the cycle, not just for the revision it was asked about.
    pub(crate) fn release_requirement(&mut self, i: &Item, c: &Closing) -> Result<()> {
        if !self.release_pending(i)? {
            return Ok(());
        }
        /* the project's own completion rule (merged, tagged) stands for the release on this machine */
        if let Some(rule) = self.done_rule(i)? {
            return self.contract_requirement(i, &rule);
        }
        /* on a Node: what it shipped and what there is to ship are its facts; none yet holds this
         * until they are here (the gathering checks the closing again) */
        if self.git_on_node(&i.id) && !matches!(self.git_take(i), Some(Ok(_))) {
            return Ok(());
        }
        let head = c.fingerprint.as_deref().map(fingerprint_head);
        let proof = self
            .db()
            .store()
            .items()
            .operations(&i.id, i.cycle)?
            .into_iter()
            .find(|o| {
                o.kind == OP_RELEASE
                    && o.status == "succeeded"
                    && self.release_holds(i, o, head) == Some(true)
            });
        let key = format!("{}:{}:{}", OP_RELEASE, i.cycle, i.revision);
        match proof {
            Some(p) => {
                self.lc_op_add(
                    i,
                    OP_RELEASE,
                    Some(&key),
                    true,
                    "succeeded",
                    "发布上线",
                    json!({"proof": p.id, "version": p.evidence["version"], "note": "本周期已经发布过包含这个结果的版本"}),
                )?;
            }
            None => {
                let declined = self.release_declined(i)?;
                let (op, new) = self.lc_op_add(
                    i,
                    OP_RELEASE,
                    Some(&key),
                    true,
                    "pending",
                    "发布上线",
                    json!({}),
                )?;
                if let Some(reason) = declined {
                    if op.status == "pending" {
                        /* set here, not through lc_op_set: the gate that called this checks right after */
                        let mut e = json!({"note": "你选择不发布"});
                        if !reason.is_empty() {
                            e["reason"] = json!(reason);
                        }
                        self.db().store().transaction(|tx| {
                            steve_store::operation_set_in(tx, &op.id, "waived", &e)
                        })?;
                        self.db().timeline_add(
                            &i.id,
                            "steve",
                            "sys",
                            "你选择了不发布，发布这一步不再等",
                            None,
                        )?;
                    }
                    return Ok(());
                }
                if new {
                    if self.nothing_to_release(i)? {
                        self.release_unproven(&i.id)?;
                    } else {
                        self.await_release_for(&i.id)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Your last word on releasing in the item's current cycle, when it was no: the reason you gave (may
    /// be empty). A release card you declined or a check you answered with "no release needed"; a later
    /// approval, or asking for the release again, takes it back.
    pub(crate) fn release_declined(&self, i: &Item) -> Result<Option<String>> {
        let since = self
            .db()
            .store()
            .items()
            .cycles(&i.id)?
            .iter()
            .filter(|c| c.cycle < i.cycle)
            .map(|c| c.closed)
            .max()
            .unwrap_or(0);
        let last = self
            .db()
            .decisions_of_item(&i.id)?
            .into_iter()
            .filter(|d| d.status == "resolved" && d.created >= since)
            .filter_map(|d| {
                let no = match (d.kind.as_str(), d.subject_type.as_str()) {
                    ("approve", "release") => d.result["approved"] == false,
                    ("fail", _) => match d.result["choice"].as_str() {
                        Some("release_waive") => true,
                        Some("release_retry") => false,
                        _ => return None,
                    },
                    _ => return None,
                };
                let reason = d.result["text"].as_str().unwrap_or("").trim().to_string();
                Some((d.resolved.unwrap_or(d.created), no, reason))
            })
            .max_by_key(|(at, _, _)| *at);
        Ok(last.filter(|(_, no, _)| *no).map(|(_, _, r)| r))
    }

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
        self.db()
            .item_patch(item_id, patch![now: "发布结果未知，等你核对"])?;
        Ok(())
    }

    /// Asks the agent of the item to publish (the required release of an accepted result).
    pub(crate) fn await_release_for(&mut self, item_id: &str) -> Result<()> {
        let to = match self
            .active_session(item_id)?
            .filter(|s| s.status != "ended")
        {
            Some(s) => To::Session(s.id),
            None => To::Decision {
                live: None,
                item: Some(item_id.into()),
            },
        };
        self.await_release(item_id, &to)
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
