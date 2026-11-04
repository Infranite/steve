//! Releases (orchestrator.ts): run the project's release script detached; it restarts the Hub, so the outcome
//! is reported on the next boot from `releases/last.json`.
//!
//! Hand-off: the script builds first, then asks this Hub over loopback (with a one-off token) to drain — no new
//! turns, new prompts queue — and polls until the running turns have ended (bounded by the script), then restarts.

use crate::error::Result;
use crate::gateway::{jstr, truthy};
use crate::git::{self, Rebase};
use crate::orchestrator::To;
use crate::patch;
use crate::repo::{Decision, Item, WS};
use crate::util::{now, token};
use crate::{Core, Releasing, Tx};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::time::{Duration, SystemTime};

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
        t.st.releasing.is_some() || t.rt().is_holding()
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
    pub fn rollback_release(&self, item: &str, target: &str) -> Result<String> {
        if let Some(why) = self.rollback_refusal(target) {
            return Err(crate::error::msg(format!("不能回滚到 {target}：{why}")));
        }
        self.tx().start_rollback(item, target)
    }

    /// Why the installed release `target` can't run on this Hub's database, if it can't. The rule of
    /// ops/schema_gate.py, which `release.sh --rollback` applies again: the database records the oldest
    /// schema a Hub must know (`steve_store::db::compat`), the release's `release.json` the schema its Hub
    /// knows (`schema.target`). A release that doesn't say is refused. Asked before a rollback is offered
    /// or started, so a refused one is neither shown as possible nor counted as undone.
    pub fn rollback_refusal(&self, target: &str) -> Option<String> {
        if target.is_empty() || target.contains('/') || target.starts_with('.') {
            return Some("不是一个已安装的版本".into());
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
            Err(e) => return Some(format!("读不到数据库的 schema（{e}）")),
        };
        match knows {
            _ if db.min_reader == 0 => None,
            None => Some(format!(
                "它说不清自己认识哪个 schema（release.json 里没有），不能打开 schema v{} 的数据库",
                db.schema
            )),
            Some(k) if k < db.min_reader => Some(format!(
                "它只认识到 schema v{k}，数据库（v{}）需要至少 v{}",
                db.schema, db.min_reader
            )),
            Some(_) => None,
        }
    }

    /// Read `releases/last.json` and report a final result once (boot, file watcher, tests).
    pub fn report_release(&self) -> Result<()> {
        self.tx().report_release()
    }

    /// Poll `releases/last.json` every 3 s and report when it changes (`fs.watchFile`). Runs forever.
    pub async fn watch_releases(self) {
        let f = self.config().releases_last();
        let stamp = |f: &Path| std::fs::metadata(f).and_then(|m| m.modified()).ok();
        let mut last: Option<SystemTime> = stamp(&f);
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        loop {
            tick.tick().await;
            let cur = stamp(&f);
            /* a pending result is looked at again on every tick: its script may die without touching the file */
            if cur != last || pending(&f) {
                last = cur;
                if let Err(e) = self.report_release() {
                    tracing::error!("report release {e}");
                }
            }
        }
    }
}

/// `now` (and the agent's answer, prefixed) when an accepted item has nothing for a release to ship.
pub(crate) const NOTHING_TO_RELEASE: &str = "没有需要发布的改动";

impl Tx<'_> {
    /// An item in a release project whose branch has no commits beyond main and a clean worktree (or neither
    /// exists): accepting it finishes it, there is nothing to publish. False whenever git can't tell.
    pub(crate) fn nothing_to_release(&self, i: &Item) -> Result<bool> {
        let Some(branch) = i.branch.as_deref().filter(|b| !b.is_empty()) else {
            return Ok(false);
        };
        let Some(p) = self.db().project(i.project_id.as_deref())? else {
            return Ok(false);
        };
        let wt = i
            .cwd
            .as_deref()
            .filter(|c| !c.is_empty() && *c != p.path)
            .map(Path::new);
        Ok(git::has_changes(Path::new(&p.path), wt, branch, "main") == Some(false))
    }

    /// Finish an accepted item that has nothing to release, saying so in the timeline.
    pub(crate) fn finish_unreleased(
        &mut self,
        item_id: &str,
        decision: Option<&str>,
    ) -> Result<()> {
        self.db().timeline_add(
            item_id,
            "steve",
            "sys",
            "分支上没有需要发布的改动，不用发布，事项直接完成",
            None,
        )?;
        self.accept_finish(item_id, decision, NOTHING_TO_RELEASE)
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
        let (m, note, now_text, reason) = match git::rebase_onto(Path::new(cwd), "main") {
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
    fn release_failed(
        &mut self,
        item_id: &str,
        now_text: &str,
        why: &str,
        extra: Value,
    ) -> Result<()> {
        self.release_outcome(item_id, "failed", extra)?;
        /* a failed release reopens the item, whatever state it was in */
        self.db()
            .item_patch(item_id, patch![status: "run", stage: "收尾", now: now_text])?;
        self.fail(item_id, why)
    }

    pub(crate) fn start_release(&mut self, branch: &str, item_id: &str) -> Result<()> {
        if self.db().store().maintenance() {
            return Err(crate::error::msg(
                "正在准备二进制升级，暂不启动源码发布或回滚",
            ));
        }
        let Some(item) = self.db().item(item_id)? else {
            return Ok(());
        };
        let p = self.db().project(item.project_id.as_deref())?;
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
    pub(crate) fn start_rollback(&mut self, item_id: &str, target: &str) -> Result<String> {
        if self.db().store().maintenance() {
            return Err(crate::error::msg(
                "正在准备二进制升级，暂不启动源码发布或回滚",
            ));
        }
        let item = self.item(item_id)?;
        let p = self.db().project(item.project_id.as_deref())?;
        let script = p
            .as_ref()
            .and_then(|p| p.release.as_ref())
            .map(|r| r.cmd.clone());
        let Some((p, script)) = p.zip(script).filter(|(_, s)| Path::new(s).exists()) else {
            return Err(crate::error::msg("这个项目的发布脚本不存在，没法回滚"));
        };
        if let Some(r) = &self.st.releasing {
            return Err(crate::error::msg(format!(
                "另一个发布（{}）正在进行，等它结束再回滚",
                r.item
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
        cmd.args([script.as_str(), "--rollback", target, item_id])
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
        let mut child = cmd
            .spawn()
            .map_err(|e| crate::error::msg(format!("回滚脚本没有启动（{e}）")))?;
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
        if start && !self.rt().is_holding() {
            self.rt().hold(true);
            if let Some(r) = self.st.releasing.as_mut() {
                r.hold_since = Some(now());
            }
            self.publish_hold();
            let n = self.rt().running_turns();
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
        let n = self.rt().running_turns();
        Ok(Some(DrainStatus {
            idle: n == 0,
            running: n,
        }))
    }

    pub(crate) fn hold_status(&self) -> Option<HoldStatus> {
        let r = self.st.releasing.as_ref()?;
        if !self.rt().is_holding() {
            return None;
        }
        Some(HoldStatus {
            item: r.item.clone(),
            running: self.rt().running_turns(),
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
        if !self.rt().is_holding() {
            return Ok(());
        }
        self.rt().hold(false);
        self.publish_hold();
        self.flush_queued()
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
        if let Some(o) = r.as_object_mut() {
            o.insert("reported".into(), Value::Bool(true));
            if abandoned {
                o.insert("ok".into(), Value::Bool(false));
                o.insert("pending".into(), Value::Bool(false));
                o.insert("error".into(), NO_RESULT.into());
            }
        }
        std::fs::write(&f, pretty1(&r))?;
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
        let text = if kind == "rollback" && !abandoned {
            if r.get("ok").is_some_and(truthy) {
                format!(
                    "已回滚到 {}（之前是 {}）",
                    or("version", ""),
                    or_falsy("prev", "上一个版本")
                )
            } else {
                format!(
                    "回滚到 {} 没有成功：{}",
                    or("version", ""),
                    or("error", "健康检查没有通过")
                )
            }
        } else if abandoned {
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
        if let Some(i) = &item.as_ref().filter(|_| kind == "rollback") {
            /* a rollback (YOLO's undo) is not the item's release: only noted on it */
            self.db().timeline_add(&i.id, "steve", "sys", &text, None)?;
        } else if let Some(i) = &item {
            if r.get("ok").is_some_and(truthy) {
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
                self.finish(&i.id, "已发布")?;
            } else {
                let hub_only = kind == "hub+web" && r.get("hub_ok").is_some_and(truthy);
                let extra = json!({
                    "reason": or("error", "健康检查没有通过"),
                    "note": if abandoned { String::new() } else { back },
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
        let ws = item
            .as_ref()
            .map(|i| i.workspace_id.clone())
            .unwrap_or_else(|| WS.into());
        let level = if r.get("ok").is_some_and(truthy) {
            "success"
        } else {
            "fail"
        };
        crate::notify::notify(
            self.db(),
            &ws,
            "release",
            level,
            &text,
            item.as_ref().map(|i| i.id.as_str()),
        )?;
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
