//! The agent's side of YOLO: the protocol lines a session starts with
//! while YOLO applies to its item, the permission mode its harness runs in, and what a running session
//! is told when YOLO changes under it.
//!
//! - While off, the protocol is byte for byte what it was before YOLO existed.
//! - `perm_mode`: `default` becomes `acceptEdits` for an item with its own directory (a worktree or
//!   scratch directory Steve made), with or without YOLO; `plan` stays read-only; `bypassPermissions` comes out of here only under 完全放开 (full access), which the user
//!   switches on explicitly — otherwise every other tool call still reaches the Hub's guard.
//! - A switch while a session runs is not a message of its own: it waits in front of the next message
//!   the session gets (the user's or Steve's), and the harness's mode is switched in place.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::Core;
use crate::repo::{Item, Project};

/// The `steve_update` line, unchanged.
pub const UPDATE: &str = "- steve_update: call it first with a short plan, and again whenever your step or plan changes. Write `now` in Chinese.";
pub const ON: &str = "- YOLO mode is on: the user is away and wants as little interruption as possible. Decide yourself whenever a reasonable choice exists; write the assumptions you made into steve_update and into the delivery summary.";
pub const ASK: &str = "- steve_ask: only when you are blocked by something only the user knows. Always give options and set `recommended` with `why`; Steve may answer for the user. After calling it, end your turn.";
pub const APPROVAL: &str = "- Work inside your working directory freely; do not ask for approval for that. Never push, publish, send messages or email, pay, or delete anything outside your working directory — these still wait for the user and will not happen while they are away; use steve_request_approval for them and continue with other work if you can. After calling it, end your turn.";
pub const DELIVER: &str = "- steve_deliver: when the goal is met and your checks pass. Run the real checks (build, type check, tests) and list them, failed ones too: they are shown to the user with your delivery. Tick off finished plan steps with steve_update first; list in `unfinished` the steps you deliberately left open. Then end your turn.";
/// Appended to the `steve_publish` line for the user's own Steve deployment.
pub const PUBLISH: &str = " In YOLO mode Steve approves releases of this project by itself and rolls back if the health check fails.";

/// Under 完全放开 (full access): the harness asks nothing, so Steve has nothing to answer and does
/// not intercept the agent's tool calls. The agent is told what to mind.
pub fn full_lines(steve_home: &str) -> Vec<String> {
    vec![
        "- Full access is on: your tools run without permission prompts. You act as the user on this machine, so stay careful:".into(),
        format!("  - do not touch {steve_home} (Steve's data, releases, web, backups) except your own working directory; do not touch port 8440 or launchd;"),
        "  - do not push to any remote and do not change GitHub with gh: Steve pushes your branch when the item is delivered;".into(),
        "  - do not send messages or mail as the user (lark-cli, sendmail, mail, osascript…); use Steve's tools for that;".into(),
        "  - stop only processes you started yourself, by PID; never use pkill or killall.".into(),
    ]
}

/// Put in front of the next message when YOLO comes on for a running session.
pub const NOTE_ON: &str = "（YOLO 已开启：用户不在，希望尽量少被打扰。有合理的选择就自己决定，把做的假设写进 steve_update 和交付摘要；steve_ask 只用于只有用户知道的事，并给出 recommended 和 why，Steve 可能代用户回答；工作目录里的事不用请求批准；推送、发布、发消息或邮件、付款、删除工作目录以外的东西仍然要等用户。交付前跑真实的检查并列出来。）";
/// …and when it goes off.
pub const NOTE_OFF: &str = "（YOLO 已结束：回到平常的工作方式，需要决定时用 steve_ask）";
/// …when full access comes on (on top of the YOLO note when it came from off).
pub const NOTE_FULL: &str = "（完全放开已开启：你的工具调用不再请求权限。仍然：不要碰 STEVE_HOME（你自己的工作目录除外）、8440 端口和 launchd；不要推送到远端，也不要用 gh 改 GitHub，交付时 Steve 会推送你的分支；不要以用户的名义发消息或邮件（lark-cli、sendmail、mail、osascript 等）；只按 PID 停你自己启动的进程，不用 pkill/killall。）";
/// …when full access ends but YOLO stays on.
pub const NOTE_FULL_OFF: &str = "（完全放开已结束：工具调用重新会请求权限，YOLO 仍开着）";

/// The bypass mode: Claude Code's id; steve-acp maps it to Codex `full-access` (danger-full-access
/// sandbox, approval policy never).
pub const BYPASS: &str = "bypassPermissions";

/// How far YOLO goes for one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Off,
    On,
    Full,
}

impl Level {
    pub fn of(e: &super::Effective) -> Level {
        match (e.on, e.full) {
            (_, true) => Level::Full,
            (true, false) => Level::On,
            _ => Level::Off,
        }
    }
}

/// The mode a session's harness runs in: `bot` = the bot's `perm_mode`, `own_dir` = the item has its
/// own directory (edits there are the item's own). Never `bypassPermissions`: a read-only session
/// asks whatever the bot says; [`harness_mode`] decides when prompts are bypassed.
pub fn perm_mode(bot: Option<&str>, own_dir: bool) -> Option<String> {
    // a bot cannot carry full access around by itself: its bypass counts as `default`
    let m = bot.map(|m| if m == BYPASS { "default" } else { m })?;
    Some(if own_dir && (m == "default" || m.is_empty()) {
        "acceptEdits".into()
    } else {
        m.to_string()
    })
}

/// The harness mode for a session at `level`: full access bypasses every prompt whatever the bot says,
/// and so does a bot set to bypass them: its owner chose that, and Steve does not second-guess it
/// (decided by the user on 2026-11-09). Otherwise [`perm_mode`], the same with YOLO on or off.
pub fn harness_mode(bot: Option<&str>, level: Level, own_dir: bool) -> Option<String> {
    match level {
        Level::Full => Some(BYPASS.into()),
        Level::On | Level::Off if bot == Some(BYPASS) => Some(BYPASS.into()),
        Level::On | Level::Off => perm_mode(bot, own_dir),
    }
}

/// The mode Steve asks a session's harness for, always named: [`harness_mode`], or for a session
/// without a bot (or a read-only one whose bot names none) the harness's asking mode (`default`),
/// which is what a Node opens a session in when it is given no mode. Leaving full access then takes
/// such a session back to asking instead of leaving it where it was.
pub fn sync_mode(bot: Option<&str>, level: Level, own_dir: bool, readonly: bool) -> String {
    let m = if readonly {
        perm_mode(bot, false)
    } else {
        harness_mode(bot, level, own_dir)
    };
    m.unwrap_or_else(|| ASKING.into())
}

/// The harness's asking mode, as Claude Code names it (Steve's `ask`).
pub const ASKING: &str = "default";

/// Where the mode Steve wants for a session comes from (`wanted_from` of the item state): full
/// access, a bot set to bypass prompts, the bot's own mode (widened to edit in the item's own
/// directory), a read-only session, or no bot at all (the Node's opening default).
pub fn mode_source(bot: Option<&str>, level: Level, readonly: bool) -> &'static str {
    match (readonly, level, bot) {
        (true, _, _) => "readonly",
        (false, Level::Full, _) => "full_access",
        (false, _, Some(BYPASS)) => "bot_bypass",
        (false, _, Some(_)) => "bot",
        (false, _, None) => "node_default",
    }
}

/// How much a mode lets the harness do without asking (Steve's names); `None`: not a mode.
fn reach(mode: &str) -> Option<u8> {
    Some(match mode {
        "plan" => 0,
        "ask" => 1,
        "accept_edits" => 2,
        "auto" => 3,
        "full_access" => 4,
        _ => return None,
    })
}

/// The harness was confirmed in a mode that lets it do more than `wanted` (Steve's name), and
/// nothing is on its way to change that: no change waits or is being sent, none was refused, and
/// the last one's outcome is known. A sync then asks for `wanted` again (after a restart the Hub no
/// longer remembers what it told the session, and the harness still runs as it was left).
fn wider_than(wanted: &str, st: &crate::runtime::ModeState) -> bool {
    let (Some(w), Some(a)) = (reach(wanted), st.applied.as_deref().and_then(reach)) else {
        return false;
    };
    a > w && st.desired.is_none() && !st.sending && !st.unknown && st.refused.is_none()
}

/// How far a session's harness is from the mode Steve wants (`wanted`, Steve's name; `None`: one
/// this runtime cannot name), given where its backend says it stands (`None`: nothing runs it
/// here): `applied` only once the backend confirmed it, never for a mode that cannot be named.
pub(crate) fn mode_status(
    wanted: Option<&str>,
    st: Option<&crate::runtime::ModeState>,
) -> &'static str {
    let Some(st) = st else {
        return "not_running";
    };
    let Some(wanted) = wanted else {
        return "unknown";
    };
    let wanted_now = |m: &Option<String>| m.as_deref() == Some(wanted);
    if st.unknown {
        "unknown"
    } else if wanted_now(&st.desired) {
        if st.sending {
            "sending"
        } else if st.busy {
            "after_turn"
        } else {
            "next_open"
        }
    } else if st.refused.is_some() && !wanted_now(&st.applied) {
        "refused"
    } else if st.desired.is_some() {
        // a change Steve no longer wants still waits: the next sync takes it back
        "not_sent"
    } else if wanted_now(&st.applied) {
        "applied"
    } else if st.released || st.applied.is_none() {
        "next_open"
    } else {
        "not_sent"
    }
}

fn note(from: Level, to: Level) -> Option<&'static str> {
    use Level::*;
    match (from, to) {
        (Off, On) => Some(NOTE_ON),
        (Off, Full) | (On, Full) => Some(NOTE_FULL),
        (On | Full, Off) => Some(NOTE_OFF),
        (Full, On) => Some(NOTE_FULL_OFF),
        _ => None,
    }
}

/// What each running session was last told about YOLO, the note waiting for its next message, and
/// the sessions whose harness mode switch did not go through yet.
#[derive(Default)]
pub(crate) struct Sessions {
    told: Mutex<HashMap<String, Level>>,
    /// The note waiting for the next message, with the level the session was at before it.
    notes: Mutex<HashMap<String, (Level, &'static str)>>,
    /// The mode switch failed (a store error, say): the next sync tries it again, whatever the
    /// note did meanwhile. Apart from `told`, so a retry never makes a note again.
    mode_pending: Mutex<HashSet<String>>,
    /// The mode the backend said it cannot express for a session: not asked again for a harness
    /// left wider than wanted (it would be every sync), only when Steve wants another one.
    unsupported: Mutex<HashMap<String, String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Sessions {
    pub(crate) fn started(&self, sid: &str, level: Level) {
        lock(&self.told).insert(sid.into(), level);
        lock(&self.notes).remove(sid);
        lock(&self.mode_pending).remove(sid);
    }
    /// The note for the next message to `sid`, taken.
    pub(crate) fn take_note(&self, sid: &str) -> Option<&'static str> {
        lock(&self.notes).remove(sid).map(|(_, n)| n)
    }
    /// Records the level; the level the session was told before when it differs (unknown = off,
    /// the protocol of a session started before YOLO).
    fn changed(&self, sid: &str, level: Level) -> Option<Level> {
        let was = lock(&self.told)
            .insert(sid.into(), level)
            .unwrap_or(Level::Off);
        if was == level {
            return None;
        }
        let mut notes = lock(&self.notes);
        // the note compares with what the session last heard (on then off again before it heard of
        // it: nothing to say)
        let heard = notes.remove(sid).map_or(was, |(from, _)| from);
        if let Some(n) = note(heard, level) {
            notes.insert(sid.into(), (heard, n));
        }
        Some(was)
    }
    /// Whether the last mode switch for `sid` is still to be made.
    fn mode_pending(&self, sid: &str) -> bool {
        lock(&self.mode_pending).contains(sid)
    }
    fn unsupported(&self, sid: &str, mode: &str) -> bool {
        lock(&self.unsupported).get(sid).is_some_and(|m| m == mode)
    }
    fn set_unsupported(&self, sid: &str, mode: Option<&str>) {
        let mut u = lock(&self.unsupported);
        match mode {
            Some(m) => u.insert(sid.into(), m.into()),
            None => u.remove(sid),
        };
    }
    fn set_mode_pending(&self, sid: &str, pending: bool) {
        let mut p = lock(&self.mode_pending);
        if pending {
            p.insert(sid.into());
        } else {
            p.remove(sid);
        }
    }
}

impl Core {
    /// YOLO applies to this item now (false without YOLO wired).
    pub(crate) fn yolo_on(&self, item: &Item) -> bool {
        self.yolo_level(item) != Level::Off
    }

    /// How far YOLO goes for this item now (off without YOLO wired).
    pub(crate) fn yolo_level(&self, item: &Item) -> Level {
        self.yolo()
            .and_then(|y| y.effective(&item.workspace_id, Some(&item.id)).ok())
            .map_or(Level::Off, |e| Level::of(&e))
    }

    /// The protocol lines for full access (empty when it does not apply).
    /// The harness asks nothing for this item (完全放开, or a bot set to bypass prompts): what the agent
    /// is told to mind.
    pub(crate) fn yolo_full_lines(&self, item: &Item) -> Vec<String> {
        let bypass = || {
            item.bot_id
                .as_deref()
                .and_then(|b| self.store().bot(b).ok().flatten())
                .is_some_and(|b| b.perm_mode == BYPASS)
        };
        match self.yolo() {
            Some(y) if self.yolo_level(item) == Level::Full || bypass() => {
                full_lines(&y.env().steve_home.to_string_lossy())
            }
            _ => Vec::new(),
        }
    }

    /// The protocol's `steve_publish` sentence for YOLO applies: the project releases the user's own Steve.
    pub(crate) fn yolo_releases_itself(&self, p: &Project) -> bool {
        let Some(y) = self.yolo() else { return false };
        p.release
            .as_ref()
            .is_some_and(|r| r.kind == "script" && super::limits::own_release(&r.cmd, y.env()))
    }

    /// The harness mode session `s` starts in: the bot's `perm_mode`, widened to `acceptEdits` for an
    /// item with its own directory (YOLO on or off), `bypassPermissions` under full access.
    pub fn session_perm_mode(
        &self,
        s: &crate::repo::Session,
        bot: Option<String>,
    ) -> Option<String> {
        let (Some(y), Some(item)) = (
            self.yolo(),
            s.item_id
                .as_deref()
                .and_then(|i| self.store().item(i).ok().flatten()),
        ) else {
            return bot;
        };
        if s.readonly != 0 {
            return perm_mode(bot.as_deref(), false);
        }
        let own = y.own_dir(item.cwd.as_deref()).is_some();
        harness_mode(bot.as_deref(), self.yolo_level(&item), own)
    }

    /// The harness mode session `s` of `item` should run in now, as [`Core::yolo_sync`] asks for
    /// it ([`sync_mode`]), and where that comes from ([`mode_source`]).
    fn wanted_mode(
        &self,
        y: &super::Yolo,
        s: &crate::repo::Session,
        item: &Item,
    ) -> (String, &'static str) {
        let bot = s
            .bot_id
            .as_deref()
            .and_then(|b| self.store().bot(b).ok().flatten())
            .map(|b| b.perm_mode);
        let own = y.own_dir(item.cwd.as_deref()).is_some();
        let level = self.yolo_level(item);
        let readonly = s.readonly != 0;
        (
            sync_mode(bot.as_deref(), level, own, readonly),
            mode_source(bot.as_deref(), level, readonly),
        )
    }

    /// `GET /yolo/items/{id}`: what applies to the item and which layer said so, and for each of its
    /// sessions the permission mode Steve wants, the one its backend confirmed the harness runs in,
    /// and why they differ. Nothing reads as in force before the backend confirmed it.
    pub fn yolo_item_state(
        &self,
        ws: &str,
        item_id: &str,
    ) -> steve_modules::ApiResult<serde_json::Value> {
        use serde_json::json;
        let Some(y) = self.yolo() else {
            return Err(steve_modules::ApiError::not_found("YOLO"));
        };
        let item = self
            .store()
            .item(item_id)
            .ok()
            .flatten()
            .filter(|i| i.workspace_id == ws)
            .ok_or_else(|| steve_modules::ApiError::not_found("事项"))?;
        let layers = y.layers(ws, Some(item_id))?;
        let eff = super::scope::resolve(&layers);
        let level = match Level::of(&eff) {
            Level::Off => "off",
            Level::On => "on",
            Level::Full => "full",
        };
        let rt = &*self.0.runtime;
        let sessions = self
            .store()
            .sessions_of_item(item_id)
            .map_err(|e| steve_modules::ApiError::Internal(e.to_string()))?;
        let list: Vec<serde_json::Value> = sessions
            .iter()
            .filter(|s| s.status != "ended")
            .map(|s| {
                let (mode, from) = self.wanted_mode(y, s, &item);
                let wanted = rt.mode_name(&mode);
                let st = rt.mode_state(&s.id);
                let status = mode_status(wanted.as_deref(), st.as_ref());
                // full access is wanted but the harness still runs in a mode that asks: what it asks
                // this turn YOLO answers as full access promises
                let residual = wanted.as_deref() == Some("full_access")
                    && st
                        .as_ref()
                        .is_some_and(|m| m.busy && m.applied.as_deref() != Some("full_access"));
                json!({
                    "session_id": s.id, "readonly": s.readonly != 0, "status": status,
                    "wanted": wanted, "wanted_from": from,
                    "applied": st.as_ref().and_then(|m| m.applied.clone()),
                    "desired": st.as_ref().and_then(|m| m.desired.clone()),
                    "busy": st.as_ref().is_some_and(|m| m.busy),
                    "refused": st.as_ref().and_then(|m| m.refused.clone()),
                    "residual": residual,
                })
            })
            .collect();
        let mode = |m: Option<super::scope::Mode>| m.map(super::scope::Mode::as_str);
        Ok(json!({
            "item_id": item_id,
            "level": level,
            "source": eff.source,
            "layers": {
                "workspace": layers.workspace.as_str(),
                "project": layers.project.map(|p| json!({ "mode": mode(p.mode), "real_dir": p.real_dir })),
                "item": mode(layers.item),
                "forced": layers.forced.map(super::scope::Forced::code),
                "paused": layers.paused,
            },
            "sessions": list,
        }))
    }

    /// The text for session `sid`, with what changed about YOLO in front of it (once).
    pub(crate) fn with_yolo_note(&self, sid: &str, text: &str) -> String {
        let sessions = &self.0.yolo_sessions;
        let mut out = text.to_string();
        if let Some(n) = sessions.take_note(sid) {
            out = format!("{n}\n\n{out}");
        }
        out
    }

    /// Brings every running session of `ws` in line with YOLO as it is now: a note for its next
    /// message and its harness mode switched, for each one whose state changed. Returns the sessions
    /// that changed.
    pub fn yolo_sync(&self, ws: &str) -> Vec<String> {
        let Some(y) = self.yolo() else {
            return Vec::new();
        };
        let sessions = match self.store().sessions_all() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("yolo sync: {e}");
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for s in sessions
            .into_iter()
            .filter(|s| s.workspace_id == ws && s.status != "ended" && s.readonly == 0)
        {
            let Some(item) = s
                .item_id
                .as_deref()
                .and_then(|i| self.store().item(i).ok().flatten())
            else {
                continue;
            };
            let level = self.yolo_level(&item);
            let sessions = &self.0.yolo_sessions;
            let rt = &*self.0.runtime;
            let (mode, _) = self.wanted_mode(y, &s, &item);
            // a level change makes the note; the mode is switched for it, again after a failure, or
            // when the harness is still left wider than Steve wants (a restart forgot the change)
            let changed = sessions.changed(&s.id, level).is_some();
            let left_wide = || {
                !sessions.unsupported(&s.id, &mode)
                    && rt
                        .mode_name(&mode)
                        .zip(rt.mode_state(&s.id))
                        .is_some_and(|(w, st)| wider_than(&w, &st))
            };
            if !changed && !sessions.mode_pending(&s.id) && !left_wide() {
                continue;
            }
            match rt.set_mode(&s.id, &mode) {
                Err(e) => {
                    // not lost: the next sync tries again (the note is not made again)
                    tracing::warn!(session = %s.id, mode, "yolo sync: set mode: {e}");
                    sessions.set_mode_pending(&s.id, true);
                    continue;
                }
                Ok(crate::runtime::Configured::Unsupported) => {
                    tracing::warn!(session = %s.id, mode, "yolo sync: a mode its backend cannot express: unchanged");
                    sessions.set_unsupported(&s.id, Some(&mode));
                }
                Ok(_) => sessions.set_unsupported(&s.id, None),
            }
            sessions.set_mode_pending(&s.id, false);
            out.push(s.id);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_widen_only_for_full_access_or_a_bypassing_bot() {
        assert_eq!(
            perm_mode(Some("default"), true).as_deref(),
            Some("acceptEdits")
        );
        assert_eq!(
            perm_mode(Some("default"), false).as_deref(),
            Some("default")
        );
        assert_eq!(perm_mode(Some("plan"), true).as_deref(), Some("plan"));
        assert_eq!(
            perm_mode(Some("acceptEdits"), true).as_deref(),
            Some("acceptEdits")
        );
        assert_eq!(perm_mode(Some("auto"), true).as_deref(), Some("auto"));
        assert_eq!(perm_mode(None, true), None);
        for on in [true, false] {
            /* a bot set to bypass prompts is taken at its word (2026-11-09); a read-only session still asks */
            assert_eq!(perm_mode(Some(BYPASS), on), perm_mode(Some("default"), on));
            for level in [Level::On, Level::Off] {
                assert_eq!(
                    harness_mode(Some(BYPASS), level, on).as_deref(),
                    Some(BYPASS)
                );
            }
        }
        for m in ["default", "plan", "acceptEdits", "auto", ""] {
            for on in [true, false] {
                assert_ne!(perm_mode(Some(m), on).as_deref(), Some("bypassPermissions"));
                assert_ne!(
                    harness_mode(Some(m), Level::On, on).as_deref(),
                    Some(BYPASS)
                );
                assert_ne!(
                    harness_mode(Some(m), Level::Off, on).as_deref(),
                    Some(BYPASS)
                );
                assert_eq!(
                    harness_mode(Some(m), Level::Full, on).as_deref(),
                    Some(BYPASS)
                );
            }
        }
        // switching off restores the bot's own mode
        assert_eq!(
            harness_mode(Some("plan"), Level::Off, true).as_deref(),
            Some("plan")
        );
        // #38: an own directory widens `default` without YOLO too; a real directory never
        assert_eq!(
            harness_mode(Some("default"), Level::Off, true).as_deref(),
            Some("acceptEdits")
        );
        assert_eq!(
            harness_mode(Some("default"), Level::Off, false).as_deref(),
            Some("default")
        );
        assert_eq!(
            harness_mode(None, Level::Full, false).as_deref(),
            Some(BYPASS)
        );
    }

    /// Nothing reads as in force before the backend confirmed it: a mode that waits for the turn's
    /// end, one on its way, one refused, and one the Hub has not asked for yet each say so.
    #[test]
    fn a_mode_is_applied_only_once_the_backend_confirmed_it() {
        use crate::runtime::ModeState;
        let full = Some("full_access");
        let st = |applied: Option<&str>, desired: Option<&str>| ModeState {
            applied: applied.map(String::from),
            desired: desired.map(String::from),
            ..Default::default()
        };
        assert_eq!(mode_status(full, None), "not_running");
        assert_eq!(mode_status(full, Some(&st(full, None))), "applied");
        // a mode Steve cannot name is never in force, whatever the harness runs in
        assert_eq!(mode_status(None, Some(&st(Some("ask"), None))), "unknown");
        assert_eq!(mode_status(None, Some(&st(full, None))), "unknown");
        // a turn runs: it goes at its end
        let mut s = st(Some("auto"), full);
        s.busy = true;
        assert_eq!(mode_status(full, Some(&s)), "after_turn");
        // sent, not confirmed yet
        s.sending = true;
        assert_eq!(mode_status(full, Some(&s)), "sending");
        // released: the next open carries it
        let mut s = st(Some("auto"), full);
        s.released = true;
        assert_eq!(mode_status(full, Some(&s)), "next_open");
        assert_eq!(mode_status(full, Some(&st(None, None))), "next_open");
        // refused by the Node or the harness, falling back
        let mut s = st(Some("auto"), None);
        s.refused = Some("this node's owner allows at most permission mode auto".into());
        assert_eq!(mode_status(full, Some(&s)), "refused");
        // …but a refusal before the mode Steve wants now was confirmed says nothing
        assert_eq!(mode_status(Some("auto"), Some(&s)), "applied");
        // not asked for yet (the sync retries), or a change Steve no longer wants still waits
        assert_eq!(mode_status(full, Some(&st(Some("auto"), None))), "not_sent");
        assert_eq!(
            mode_status(Some("auto"), Some(&st(Some("auto"), full))),
            "not_sent"
        );
        let mut s = st(Some("auto"), full);
        s.unknown = true;
        assert_eq!(mode_status(full, Some(&s)), "unknown");
    }

    #[test]
    fn a_note_waits_once_and_a_flip_back_cancels_it() {
        use Level::*;
        let s = Sessions::default();
        s.started("a", Off);
        assert_eq!(s.changed("a", Off), None);
        assert_eq!(s.changed("a", On), Some(Off));
        assert_eq!(s.take_note("a"), Some(NOTE_ON));
        assert_eq!(s.take_note("a"), None);
        assert_eq!(s.changed("a", Off), Some(On));
        assert_eq!(s.changed("a", On), Some(Off));
        assert_eq!(
            s.take_note("a"),
            None,
            "off then on again before any message"
        );
        // full access: told once, and stepping back down says so
        assert_eq!(s.changed("a", Full), Some(On));
        assert_eq!(s.take_note("a"), Some(NOTE_FULL));
        assert_eq!(s.changed("a", On), Some(Full));
        assert_eq!(s.take_note("a"), Some(NOTE_FULL_OFF));
        assert_eq!(s.changed("a", Full), Some(On));
        assert_eq!(s.changed("a", Off), Some(Full));
        assert_eq!(s.take_note("a"), Some(NOTE_OFF), "on → full → off: off");
    }
}
