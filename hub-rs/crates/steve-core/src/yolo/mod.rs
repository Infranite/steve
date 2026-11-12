//! YOLO mode (with the user's decisions of 2026-11-01): Steve makes the decisions that
//! stay on this machine and can be undone, and leaves money, outside effects and other people's data to
//! the user. Switching it on is a plain on/off per workspace (no expiry), with per-project and per-item
//! overrides; there are no budgets.
//!
//! - [`scope`]: which layer applies to a decision, forced-off reasons (pure);
//! - [`judge`], [`limits`], [`guard`]: what YOLO would do with a decision (pure, Y1);
//! - [`Yolo`]: the switch, scopes, breaker state and windows in the database (Y0);
//! - [`engine`], [`facts`], [`ledger`], [`breaker`]: the executor that acts on decisions, what it
//!   knows about them, the record of what it did, and the breakers it trips (Y2).

pub mod agent;
pub mod answer;
pub mod breaker;
pub mod digest;
pub mod draft;
pub mod engine;
pub mod facts;
pub mod full_guard;
pub mod guard;
pub mod judge;
pub mod ledger;
pub mod limits;
pub mod release;
pub mod scope;
pub mod shell;
pub mod undo;

#[cfg(test)]
mod agent_tests;
#[cfg(test)]
mod answer_tests;
#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod full_tests;
#[cfg(test)]
mod guard_tests;
#[cfg(test)]
mod release_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod undo_tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Clock, SystemClock};
use steve_store::base::audit;
use steve_store::yolo::{self as repo, ITEM, PROJECT, ScopeChange, ScopeRow, WORKSPACE};

use crate::repo::{Db, Decision, Project};
pub use engine::{Engine, ToolInput};
pub use judge::{Env, Facts, Policy, Subject, Verdict, judge};
pub use scope::{Effective, Forced, Layers, Mode, ProjectLayer, Source};

/// Reason shown for projects that default to off.
pub const REAL_DIR_REASON: &str = "没有 worktree，直接改真实目录";

/// The YOLO switch of every workspace. Cheap to clone.
#[derive(Clone)]
pub struct Yolo(Arc<Inner>);

struct Inner {
    db: Db,
    forced: Option<Forced>,
    clock: Arc<dyn Clock>,
    env: Env,
}

/// `PUT /yolo` body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switch {
    pub on: bool,
    /// 完全放开 (implies `on`); only with the user's explicit `confirm: true`.
    pub full: bool,
    pub preview: bool,
    pub include_open: bool,
}

/// Text of the confirmation the enable flow shows and the notice when full access comes on.
pub const FULL_WARNING: &str = "完全放开：Agent 以你的身份运行任何命令、改任何文件，不再先问你。仍然要你确认：付款和银行卡、高敏感保险箱条目、经 Steve 以你的名义发出的消息和邮件（只发带 [AI] 标识并通过披露检查的）、其他项目的发布。Agent 自己的命令不再请求权限，Steve 也就不会拦截它们；仍然起保护作用的是：Steve 的机密尽量不放在 Agent 看得到的环境变量和文件里，Agent 的进程拿不到 GH_TOKEN 和 SSH agent。系统级的沙箱还在开发中。";

impl Switch {
    /// Parses the body: `{on}` or `{mode: off|on|full}`; full access also needs `confirm: true`, so it
    /// is never switched on by a client that merely sets `on`. `until` belongs to the time-boxed
    /// design the user dropped and is refused rather than silently ignored.
    pub fn parse(b: &Value) -> ApiResult<Switch> {
        let flag = |k: &str| -> ApiResult<bool> {
            match b.get(k) {
                None | Some(Value::Null) => Ok(false),
                Some(Value::Bool(v)) => Ok(*v),
                Some(_) => Err(ApiError::invalid(k)),
            }
        };
        if b.get("until").is_some_and(|v| !v.is_null()) {
            return Err(ApiError::invalid("until"));
        }
        let mode = match b.get("mode") {
            None | Some(Value::Null) => None,
            Some(Value::String(m)) => match m.as_str() {
                "off" => Some(Mode::Off),
                "on" => Some(Mode::On),
                "full" => Some(Mode::Full),
                _ => return Err(ApiError::invalid("mode")),
            },
            Some(_) => return Err(ApiError::invalid("mode")),
        };
        let (on, full) = match (b.get("on"), mode) {
            (Some(Value::Bool(v)), None) => (*v, false),
            (None | Some(Value::Null), Some(m)) => (m.yolo(), m == Mode::Full),
            (Some(Value::Bool(v)), Some(m)) if *v == m.yolo() => (*v, m == Mode::Full),
            (_, Some(_)) => return Err(ApiError::invalid("mode")),
            _ => return Err(ApiError::invalid("on")),
        };
        let preview = flag("preview")?;
        if full && !preview && !flag("confirm")? {
            return Err(ApiError::invalid("confirm"));
        }
        Ok(Switch {
            on,
            full,
            preview,
            include_open: flag("include_open")?,
        })
    }

    fn mode(self) -> &'static str {
        match (self.on, self.full) {
            (_, true) => "full",
            (true, false) => "on",
            _ => "off",
        }
    }
}

fn store_err(e: steve_store::StoreError) -> ApiError {
    ApiError::from(e)
}

impl Yolo {
    /// `forced`: from [`Forced::from_env`]; `steve_home`: `STEVE_HOME`.
    pub fn new(db: Db, forced: Option<Forced>, steve_home: &Path) -> Yolo {
        Yolo::with_clock(db, forced, steve_home, Arc::new(SystemClock))
    }

    pub fn with_clock(
        db: Db,
        forced: Option<Forced>,
        steve_home: &Path,
        clock: Arc<dyn Clock>,
    ) -> Yolo {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Yolo(Arc::new(Inner {
            db,
            forced,
            clock,
            env: Env {
                steve_home: steve_home.to_path_buf(),
                home,
                tmp: Env::tmp_dirs(),
            },
        }))
    }

    pub fn forced(&self) -> Option<Forced> {
        self.0.forced
    }

    pub fn env(&self) -> &Env {
        &self.0.env
    }

    fn store(&self) -> &steve_store::Store {
        self.0.db.store()
    }

    fn now(&self) -> i64 {
        self.0.clock.now()
    }

    fn ws_row(&self, ws: &str) -> ApiResult<Option<ScopeRow>> {
        self.store()
            .reader(|c| repo::scope_get(c, ws, WORKSPACE, ws))
            .map_err(store_err)
    }

    /// `GET /yolo`.
    pub fn status(&self, ws: &str) -> ApiResult<Value> {
        let projects = self.store().projects().list(ws, false).map_err(store_err)?;
        let (row, scopes, window) = self
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>((
                    repo::scope_get(c, ws, WORKSPACE, ws)?,
                    repo::scopes(c, ws)?,
                    repo::window_current(c, ws)?,
                ))
            })
            .map_err(store_err)?;
        let counts = match &window {
            Some(w) => self
                .store()
                .reader(|c| repo::counts(c, &w.id))
                .map_err(store_err)?,
            None => repo::Counts::default(),
        };
        let mode = ws_mode(row.as_ref());
        let on = mode.yolo();
        let full = mode == Mode::Full;
        let paused = row.as_ref().and_then(|r| r.paused.clone());
        let mut list: Vec<Value> = scopes
            .iter()
            .filter(|s| s.object_type != WORKSPACE)
            .map(|s| {
                json!({ "object_type": s.object_type, "object_id": s.object_id, "mode": s.mode,
                        "reason": judge::reason_shown(&s.reason), "updated": s.updated })
            })
            .collect();
        for p in projects.iter().filter(|p| real_dir(p)) {
            if !scopes
                .iter()
                .any(|s| s.object_type == PROJECT && s.object_id == p.id)
            {
                list.push(
                    json!({ "object_type": PROJECT, "object_id": p.id, "mode": "off",
                                  "reason": judge::reason_shown(REAL_DIR_REASON), "default": true }),
                );
            }
        }
        let active = on && self.0.forced.is_none() && paused.is_none();
        Ok(json!({
            "on": on,
            "mode": mode.as_str(),
            "full": full,
            "active": active,
            "full_active": full && active,
            "forced_off": self.0.forced.map(Forced::code),
            "forced_reason": self.0.forced.map(Forced::message),
            "paused": paused,
            "window": window.map(|w| json!({ "id": w.id, "started": w.started, "include_open": w.include_open })),
            "scopes": list,
            "counts": counts,
        }))
    }

    /// `PUT /yolo`: the workspace switch (off / on / full), or with `preview` what switching on would do.
    pub fn put(&self, ws: &str, me: &str, s: Switch) -> ApiResult<Value> {
        let row = self.ws_row(ws)?;
        let from = ws_mode(row.as_ref());
        let to = s.mode();
        if s.on {
            if let Some(f) = self.0.forced {
                return Err(ApiError::conflict(f.message()));
            }
            if let Some(p) = row.as_ref().and_then(|r| r.paused.as_ref()) {
                let why = p.get("reason").and_then(Value::as_str).unwrap_or("");
                return Err(ApiError::conflict(steve_i18n::t!(
                    "core.yolo.paused_why",
                    why = why
                )));
            }
            if s.preview {
                return self.preview(ws);
            }
            if from.as_str() == to {
                return self.status(ws);
            }
            let now = self.now();
            self.store()
                .transaction(|tx| {
                    repo::scope_put(
                        tx,
                        ws,
                        WORKSPACE,
                        ws,
                        &ScopeChange {
                            mode: Some(to),
                            ..Default::default()
                        },
                        me,
                        now,
                    )?;
                    // on ↔ full keeps the window: it is one stretch of YOLO
                    let (action, window) = if from.yolo() {
                        ("yolo.mode", repo::window_current(tx, ws)?.map(|w| w.id))
                    } else {
                        let w = repo::window_open(tx, ws, s.include_open, now)?;
                        ("yolo.on", Some(w.id))
                    };
                    audit(
                        tx,
                        ws,
                        me,
                        action,
                        &format!("workspace:{ws}"),
                        &json!({
                        "object_type": WORKSPACE, "object_id": ws, "mode": to, "from": from.as_str(),
                        "full": s.full, "include_open": s.include_open, "window": window }),
                    )
                })
                .map_err(store_err)?;
            if s.full {
                self.notice(ws, "info", FULL_WARNING);
            } else if from == Mode::Full {
                self.notice(
                    ws,
                    "info",
                    "完全放开已结束：回到 YOLO，Agent 的工具调用重新先问",
                );
            }
            return self.status(ws);
        }
        if s.preview {
            return Err(ApiError::invalid("preview"));
        }
        if !from.yolo() {
            return self.status(ws);
        }
        let now = self.now();
        let (decided, declined) = self
            .store()
            .transaction(|tx| {
                repo::scope_put(
                    tx,
                    ws,
                    WORKSPACE,
                    ws,
                    &ScopeChange {
                        mode: Some("off"),
                        ..Default::default()
                    },
                    me,
                    now,
                )?;
                let mut n = repo::Counts::default();
                let mut window = None;
                if let Some(w) = repo::window_current(tx, ws)? {
                    repo::cancel_scheduled(tx, ws, &w.id, now)?;
                    repo::window_close(tx, &w.id, "off", now)?;
                    n = repo::counts(tx, &w.id)?;
                    window = Some(w.id);
                }
                audit(
                    tx,
                    ws,
                    me,
                    "yolo.off",
                    &format!("workspace:{ws}"),
                    &json!({
                    "object_type": WORKSPACE, "object_id": ws, "mode": "off", "from": from.as_str(),
                    "window": window, "decided": n.decided, "declined": n.declined }),
                )?;
                Ok::<_, steve_store::StoreError>((n.decided, n.declined))
            })
            .map_err(store_err)?;
        let text = steve_i18n::stored(|| {
            if from == Mode::Full {
                steve_i18n::t!(
                    "core.notice.full_ended",
                    decided = decided,
                    declined = declined
                )
            } else {
                steve_i18n::t!(
                    "core.notice.yolo_ended",
                    decided = decided,
                    declined = declined
                )
            }
        });
        self.notice(ws, "info", &text);
        self.status(ws)
    }

    fn notice(&self, ws: &str, level: &str, text: &str) {
        if let Err(e) = crate::notify::notify(&self.0.db, ws, "system", level, text, None) {
            tracing::error!("yolo notice: {e}");
        }
    }

    /// `PUT /yolo/scopes/:type/:id` (`type` = project | item) with `{mode}`.
    pub fn put_scope(
        &self,
        ws: &str,
        me: &str,
        object_type: &str,
        id: &str,
        b: &Value,
    ) -> ApiResult<Value> {
        if !matches!(object_type, PROJECT | ITEM) {
            return Err(ApiError::invalid("type"));
        }
        if b.get("until").is_some_and(|v| !v.is_null()) {
            return Err(ApiError::invalid("until"));
        }
        let mode = b
            .get("mode")
            .and_then(Value::as_str)
            .and_then(Mode::parse)
            .ok_or_else(|| ApiError::invalid("mode"))?;
        if mode == Mode::Full {
            // full access is the user's explicit choice, never a side effect of another request
            if let Some(f) = self.0.forced {
                return Err(ApiError::conflict(f.message()));
            }
            if b.get("confirm") != Some(&Value::Bool(true)) {
                return Err(ApiError::invalid("confirm"));
            }
        }
        let owner = if object_type == PROJECT {
            self.store()
                .projects()
                .get(id)
                .map_err(store_err)?
                .map(|p| p.workspace_id)
        } else {
            self.store()
                .items()
                .get(id)
                .map_err(store_err)?
                .map(|i| i.workspace_id)
        };
        if owner.as_deref() != Some(ws) {
            return Err(ApiError::not_found(if object_type == PROJECT {
                "项目"
            } else {
                "事项"
            }));
        }
        let now = self.now();
        self.store()
            .transaction(|tx| {
                let (before, after) = repo::scope_put(
                    tx,
                    ws,
                    object_type,
                    id,
                    &ScopeChange {
                        mode: Some(mode.as_str()),
                        reason: Some(""),
                        ..Default::default()
                    },
                    me,
                    now,
                )?;
                if before.as_ref() != Some(&after) {
                    let from = before.as_ref().map(|b| b.mode.clone());
                    audit(
                        tx,
                        ws,
                        me,
                        "yolo.scope",
                        &format!("{object_type}:{id}"),
                        &json!({
                        "object_type": object_type, "object_id": id, "mode": mode.as_str(),
                        "from": from, "full": mode == Mode::Full }),
                    )?;
                }
                Ok::<_, steve_store::StoreError>(())
            })
            .map_err(store_err)?;
        self.status(ws)
    }

    /// `POST /yolo/resume`: clears the breaker; a switch that is on starts a new window (counters from zero).
    pub fn resume(&self, ws: &str, me: &str) -> ApiResult<Value> {
        let row = self.ws_row(ws)?;
        let Some(row) = row.filter(|r| r.paused.is_some()) else {
            return Err(ApiError::conflict(steve_i18n::t!("core.yolo.not_paused")));
        };
        let now = self.now();
        self.store()
            .transaction(|tx| {
                repo::scope_put(tx, ws, WORKSPACE, ws, &ScopeChange { paused: Some(None), ..Default::default() }, me, now)?;
                let window = if Mode::parse(&row.mode).is_some_and(Mode::yolo) && repo::window_current(tx, ws)?.is_none() {
                    Some(repo::window_open(tx, ws, false, now)?.id)
                } else {
                    None
                };
                audit(tx, ws, me, "yolo.resume", &format!("workspace:{ws}"), &json!({
                    "object_type": WORKSPACE, "object_id": ws, "mode": row.mode,
                    "reason": row.paused.as_ref().and_then(|p| p.get("reason")).cloned(), "window": window }))
            })
            .map_err(store_err)?;
        self.status(ws)
    }

    /// The breaker: pauses the whole workspace until the user resumes it, ends the window,
    /// cancels what was scheduled and leaves a failure notice. A second trip while paused does nothing.
    pub fn trip(&self, ws: &str, code: &str, reason: &str) -> ApiResult<bool> {
        let now = self.now();
        let tripped = self
            .store()
            .transaction(|tx| {
                if repo::scope_get(tx, ws, WORKSPACE, ws)?.is_some_and(|r| r.paused.is_some()) {
                    return Ok(false);
                }
                let paused = json!({ "code": code, "reason": reason, "at": now });
                repo::scope_put(
                    tx,
                    ws,
                    WORKSPACE,
                    ws,
                    &ScopeChange {
                        paused: Some(Some(&paused)),
                        ..Default::default()
                    },
                    "yolo",
                    now,
                )?;
                if let Some(w) = repo::window_current(tx, ws)? {
                    repo::cancel_scheduled(tx, ws, &w.id, now)?;
                    repo::window_close(tx, &w.id, "tripped", now)?;
                }
                audit(
                    tx,
                    ws,
                    "yolo",
                    "yolo.trip",
                    &format!("workspace:{ws}"),
                    &json!({
                    "object_type": WORKSPACE, "object_id": ws, "code": code, "reason": reason }),
                )?;
                Ok::<_, steve_store::StoreError>(true)
            })
            .map_err(store_err)?;
        if tripped
            && let Err(e) = crate::notify::notify(
                &self.0.db,
                ws,
                "system",
                "fail",
                &steve_i18n::stored(|| steve_i18n::t!("core.notice.yolo_paused", reason = reason)),
                None,
            )
        {
            tracing::error!("yolo trip notice: {e}");
        }
        Ok(tripped)
    }

    /// The layers that apply to a decision in `ws` about `item` (`None`: a decision without an item).
    pub fn layers(&self, ws: &str, item: Option<&str>) -> ApiResult<Layers> {
        let row = self.ws_row(ws)?;
        let workspace = ws_mode(row.as_ref());
        let mut l = Layers {
            forced: self.0.forced,
            paused: row.as_ref().is_some_and(|r| r.paused.is_some()),
            workspace,
            project: None,
            item: None,
        };
        let Some(id) = item else { return Ok(l) };
        let Some(it) = self
            .store()
            .items()
            .get(id)
            .map_err(store_err)?
            .filter(|i| i.workspace_id == ws)
        else {
            return Err(ApiError::not_found("事项"));
        };
        let mode_of = |ty: &str, oid: &str| -> ApiResult<Option<Mode>> {
            Ok(self
                .store()
                .reader(|c| repo::scope_get(c, ws, ty, oid))
                .map_err(store_err)?
                .and_then(|r| Mode::parse(&r.mode)))
        };
        l.item = Some(mode_of(ITEM, id)?.unwrap_or(Mode::Inherit));
        if let Some(pid) = it.project_id.as_deref()
            && let Some(p) = self.store().projects().get(pid).map_err(store_err)?
        {
            l.project = Some(ProjectLayer {
                mode: mode_of(PROJECT, pid)?,
                real_dir: real_dir(&p),
            });
        }
        Ok(l)
    }

    /// Whether YOLO applies to a decision about `item` in `ws`, and which layer said so.
    pub fn effective(&self, ws: &str, item: Option<&str>) -> ApiResult<Effective> {
        Ok(scope::resolve(&self.layers(ws, item)?))
    }

    /// The item's own directory if it has one (a worktree or scratch dir under `STEVE_HOME`).
    pub fn own_dir(&self, cwd: Option<&str>) -> Option<PathBuf> {
        let cwd = Path::new(cwd?);
        let home = limits::canonical(&self.0.env.steve_home);
        let c = limits::canonical(cwd);
        let own = [home.join("worktrees"), home.join("scratch")]
            .iter()
            .any(|d| c.starts_with(d) && c != *d);
        own.then_some(c)
    }

    /// What switching on would do with the cards open now, judged with the default policy as if
    /// the workspace switch were on. Only what a decision row tells is known here; the rest is left to you.
    pub fn preview(&self, ws: &str) -> ApiResult<Value> {
        let open: Vec<Decision> = self.store().decisions().open(ws).map_err(store_err)?;
        let policy = Policy::default();
        let mut would = Vec::new();
        let (mut keep, mut hard, mut muted) = (0, 0, 0);
        for d in &open {
            let mut l = self.layers(ws, d.item_id.as_deref()).unwrap_or(Layers {
                forced: None,
                paused: false,
                workspace: Mode::On,
                project: None,
                item: None,
            });
            l.workspace = Mode::On;
            if !scope::resolve(&l).on {
                keep += 1;
                continue;
            }
            let item = match d.item_id.as_deref() {
                Some(id) => self.store().items().get(id).map_err(store_err)?,
                None => None,
            };
            let project_dir = match item.as_ref().and_then(|i| i.project_id.as_deref()) {
                Some(pid) => self
                    .store()
                    .projects()
                    .get(pid)
                    .map_err(store_err)?
                    .map(|p| PathBuf::from(p.path)),
                None => None,
            };
            let subject = judge::subject_from_row(
                d,
                self.own_dir(item.as_ref().and_then(|i| i.cwd.as_deref())),
                project_dir,
                item.as_ref().and_then(|i| i.branch.clone()),
            );
            let f = judge::facts_of(d, subject);
            match judge(&f, &policy, &self.0.env) {
                Verdict::Auto { choice, rule, delay_s, .. } => would.push(json!({
                    "decision_id": d.id, "question": d.question, "choice": choice, "rule": rule, "delay_s": delay_s })),
                v @ Verdict::Decline { .. } if v.is_hard_limit() => hard += 1,
                Verdict::Mute { .. } => muted += 1,
                _ => keep += 1,
            }
        }
        Ok(
            json!({ "preview": true, "would_handle": would, "keep_for_you": keep, "hard_limited": hard, "muted": muted }),
        )
    }
}

/// The workspace switch as stored: `On`, `Full` or `Off`.
fn ws_mode(row: Option<&ScopeRow>) -> Mode {
    match row.and_then(|r| Mode::parse(&r.mode)) {
        Some(m @ (Mode::On | Mode::Full)) => m,
        _ => Mode::Off,
    }
}

/// The project's cwd is the real directory (`kind=dir`, or git without worktrees).
pub fn real_dir(p: &Project) -> bool {
    p.kind != "git" || !p.worktree
}
