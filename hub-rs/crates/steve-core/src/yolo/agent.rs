//! The agent's side of YOLO: the protocol lines a session starts with
//! while YOLO applies to its item, the permission mode its harness runs in, and what a running session
//! is told when YOLO changes under it.
//!
//! - While off, the protocol is byte for byte what it was before YOLO existed.
//! - `perm_mode`: `default` becomes `acceptEdits` only for an item with its own directory; `plan` stays
//!   read-only; `bypassPermissions` comes out of here only under 完全放开 (full access), which the user
//!   switches on explicitly — otherwise every other tool call still reaches the Hub's guard.
//! - A switch while a session runs is not a message of its own: it waits in front of the next message
//!   the session gets (the user's or Steve's), and the harness's mode is switched in place.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::Core;
use crate::repo::{Item, Project};

/// The `steve_update` line, unchanged.
pub const UPDATE: &str = "- steve_update: call it first with a short plan, and again whenever your step or plan changes. Write `now` in Chinese.";
pub const ON: &str = "- YOLO mode is on: the user is away and wants as little interruption as possible. Decide yourself whenever a reasonable choice exists; write the assumptions you made into steve_update and into the delivery summary.";
pub const ASK: &str = "- steve_ask: only when you are blocked by something only the user knows. Always give options and set `recommended` with `why`; Steve may answer for the user. After calling it, end your turn.";
pub const APPROVAL: &str = "- Work inside your working directory freely; do not ask for approval for that. Never push, publish, send messages or email, pay, or delete anything outside your working directory — these still wait for the user and will not happen while they are away; use steve_request_approval for them and continue with other work if you can. After calling it, end your turn.";
pub const DELIVER: &str = "- steve_deliver: when the goal is met and your checks pass. Run the real checks (build, type check, tests) and list them; a delivery without checks, or with a failed one, is sent back to you instead of being accepted. Tick off finished plan steps with steve_update first; list in `unfinished` the steps you deliberately left open. Then end your turn.";
/// Appended to the `steve_publish` line for the user's own Steve deployment.
pub const PUBLISH: &str = " In YOLO mode Steve approves releases of this project by itself and rolls back if the health check fails.";

/// Under 完全放开 (full access): the harness asks nothing, so these are the only brakes left on the agent.
pub fn full_lines(steve_home: &str) -> Vec<String> {
    vec![
        "- Full access is on: your tools run without permission prompts. You act as the user on this machine, so stay careful:".into(),
        format!("  - do not touch {steve_home} (Steve's data, releases, web, backups) except your own working directory; do not touch port 8440 or launchd;"),
        "  - do not push to any remote unless the item explicitly asks for it;".into(),
        "  - stop only processes you started yourself, by PID; never use pkill or killall.".into(),
    ]
}

/// Put in front of the next message when YOLO comes on for a running session.
pub const NOTE_ON: &str = "（YOLO 已开启：用户不在，希望尽量少被打扰。有合理的选择就自己决定，把做的假设写进 steve_update 和交付摘要；steve_ask 只用于只有用户知道的事，并给出 recommended 和 why，Steve 可能代用户回答；工作目录里的事不用请求批准；推送、发布、发消息或邮件、付款、删除工作目录以外的东西仍然要等用户。交付前跑真实的检查并列出来。）";
/// …and when it goes off.
pub const NOTE_OFF: &str = "（YOLO 已结束：回到平常的工作方式，需要决定时用 steve_ask）";
/// …when full access comes on (on top of the YOLO note when it came from off).
pub const NOTE_FULL: &str = "（完全放开已开启：你的工具调用不再请求权限。仍然：不要碰 STEVE_HOME（你自己的工作目录除外）、8440 端口和 launchd；除非事项要求，不要推送到远端；只按 PID 停你自己启动的进程，不用 pkill/killall。）";
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

/// The mode a session's harness runs in: `bot` = the bot's `perm_mode`, `yolo` = YOLO applies to
/// the item and the item has its own directory. Never `bypassPermissions`: that is [`harness_mode`]'s
/// answer for full access only.
pub fn perm_mode(bot: Option<&str>, yolo: bool) -> Option<String> {
    // a bot cannot carry full access around by itself: its bypass counts as `default`
    let m = bot.map(|m| if m == BYPASS { "default" } else { m })?;
    Some(if yolo && (m == "default" || m.is_empty()) {
        "acceptEdits".into()
    } else {
        m.to_string()
    })
}

/// The harness mode for a session at `level`: full access bypasses every prompt whatever the bot says;
/// otherwise [`perm_mode`] (switching off restores the bot's own mode).
pub fn harness_mode(bot: Option<&str>, level: Level, own_dir: bool) -> Option<String> {
    match level {
        Level::Full => Some(BYPASS.into()),
        Level::On => perm_mode(bot, own_dir),
        Level::Off => perm_mode(bot, false),
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

/// What each running session was last told about YOLO, and the note waiting for its next message.
#[derive(Default)]
pub(crate) struct Sessions {
    told: Mutex<HashMap<String, Level>>,
    /// The note waiting for the next message, with the level the session was at before it.
    notes: Mutex<HashMap<String, (Level, &'static str)>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Sessions {
    pub(crate) fn started(&self, sid: &str, level: Level) {
        lock(&self.told).insert(sid.into(), level);
        lock(&self.notes).remove(sid);
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
    /// The change to `level` did not reach the session: it counts as told `was` again, so the next
    /// sync tries once more (the note stays; it is made again from what the session last heard).
    fn unmark(&self, sid: &str, was: Level) {
        lock(&self.told).insert(sid.into(), was);
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
    pub(crate) fn yolo_full_lines(&self, item: &Item) -> Vec<String> {
        match self.yolo() {
            Some(y) if self.yolo_level(item) == Level::Full => {
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

    /// The harness mode session `s` starts in: the bot's `perm_mode`, widened to `acceptEdits` while
    /// YOLO applies to an item with its own directory, `bypassPermissions` under full access.
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

    /// The text for session `sid`, with what changed about YOLO in front of it (once).
    pub(crate) fn with_yolo_note(&self, sid: &str, text: &str) -> String {
        match self.0.yolo_sessions.take_note(sid) {
            Some(n) => format!("{n}\n\n{text}"),
            None => text.to_string(),
        }
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
            let Some(was) = self.0.yolo_sessions.changed(&s.id, level) else {
                continue;
            };
            let bot = s
                .bot_id
                .as_deref()
                .and_then(|b| self.store().bot(b).ok().flatten())
                .map(|b| b.perm_mode);
            let own = y.own_dir(item.cwd.as_deref()).is_some();
            if let Some(mode) = harness_mode(bot.as_deref(), level, own)
                && let Err(e) = self.0.runtime.set_mode(&s.id, &mode)
            {
                // not lost: still marked as changed for the next sync
                tracing::warn!(session = %s.id, mode, "yolo sync: set mode: {e}");
                self.0.yolo_sessions.unmark(&s.id, was);
                continue;
            }
            out.push(s.id);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_never_widen_past_accept_edits() {
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
        for m in ["default", "plan", "acceptEdits", "auto", "", BYPASS] {
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
        assert_eq!(
            harness_mode(None, Level::Full, false).as_deref(),
            Some(BYPASS)
        );
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
