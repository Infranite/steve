//! Which layer decides whether YOLO applies: item > project > workspace,
//! the first that is not `inherit`; an item without a project skips the project layer, a decision
//! without an item looks only at the workspace. Forced off (the environment, a production-copy Hub)
//! and a tripped breaker beat every layer.
//!
//! No expiry: every layer is plain on/off/inherit until changed.
//!
//! `full` (完全放开, user decision 2026-11-02) is YOLO plus agents that run without permission prompts
//! (`bypassPermissions` / Codex `full-access`). It is only ever set by the user, layer by layer, and a
//! forced-off Hub or a tripped breaker turns it off like YOLO.

use serde::Serialize;

/// A layer's setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    On,
    Off,
    Inherit,
    /// 完全放开: YOLO, and the agent's harness asks nothing.
    Full,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "on" => Some(Mode::On),
            "off" => Some(Mode::Off),
            "inherit" => Some(Mode::Inherit),
            "full" => Some(Mode::Full),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::On => "on",
            Mode::Off => "off",
            Mode::Inherit => "inherit",
            Mode::Full => "full",
        }
    }
    /// YOLO decides (`on` or `full`).
    pub fn yolo(self) -> bool {
        matches!(self, Mode::On | Mode::Full)
    }
}

/// Why YOLO is off no matter what the page says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Forced {
    /// `STEVE_YOLO=off`
    Env,
    /// `STEVE_NO_RESUME=1`: a Hub on a copy of the production database must not decide for real items.
    NoResume,
}

impl Forced {
    /// `STEVE_YOLO=off` wins over `STEVE_NO_RESUME=1` (it is the deliberate switch).
    pub fn from_env(steve_yolo: Option<&str>, no_resume: bool) -> Option<Forced> {
        if steve_yolo.is_some_and(|v| v.trim().eq_ignore_ascii_case("off")) {
            Some(Forced::Env)
        } else if no_resume {
            Some(Forced::NoResume)
        } else {
            None
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Forced::Env => "env",
            Forced::NoResume => "no_resume",
        }
    }
    /// The `409` text and the banner line.
    pub fn message(self) -> &'static str {
        match self {
            Forced::Env => "YOLO 被环境变量 STEVE_YOLO=off 关掉了",
            Forced::NoResume => "这个 Hub 开在生产库副本上（STEVE_NO_RESUME=1），YOLO 自带关闭",
        }
    }
}

/// The project layer of one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectLayer {
    /// The stored row; `None` = never set.
    pub mode: Option<Mode>,
    /// The project's cwd is the real directory (`kind=dir`, or git without worktrees): without a row it
    /// counts as `off`, so the user has to turn it on explicitly.
    pub real_dir: bool,
}

/// Everything [`resolve`] looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layers {
    pub forced: Option<Forced>,
    /// The breaker tripped.
    pub paused: bool,
    /// The workspace switch: `On`, `Full` or `Off`.
    pub workspace: Mode,
    /// `None` for a decision without an item, or an item without a project (scratch).
    pub project: Option<ProjectLayer>,
    /// `None` for a decision without an item; `Some(Inherit)` for an item without a row.
    pub item: Option<Mode>,
}

/// Which layer decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Forced,
    Paused,
    Item,
    Project,
    /// The project's default for a real directory.
    ProjectDefault,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Effective {
    /// YOLO decides for the user.
    pub on: bool,
    /// …and the agent runs with full access (implies `on`).
    pub full: bool,
    pub source: Source,
}

/// The effective setting for one decision.
pub fn resolve(l: &Layers) -> Effective {
    let eff = |m: Mode, source| Effective {
        on: m.yolo(),
        full: m == Mode::Full,
        source,
    };
    if l.forced.is_some() {
        return eff(Mode::Off, Source::Forced);
    }
    if l.paused {
        return eff(Mode::Off, Source::Paused);
    }
    if let Some(m) = l.item.filter(|m| *m != Mode::Inherit) {
        return eff(m, Source::Item);
    }
    if l.item.is_some()
        && let Some(p) = l.project
    {
        match p.mode {
            Some(m) if m != Mode::Inherit => return eff(m, Source::Project),
            None if p.real_dir => return eff(Mode::Off, Source::ProjectDefault),
            _ => {}
        }
    }
    eff(l.workspace, Source::Workspace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layers(ws: Mode, project: Option<ProjectLayer>, item: Option<Mode>) -> Layers {
        Layers {
            forced: None,
            paused: false,
            workspace: ws,
            project,
            item,
        }
    }
    fn proj(mode: Option<Mode>, real_dir: bool) -> Option<ProjectLayer> {
        Some(ProjectLayer { mode, real_dir })
    }

    #[test]
    fn item_beats_project_beats_workspace() {
        use Mode::*;
        // item set: it decides
        assert!(resolve(&layers(Off, proj(Some(Off), false), Some(On))).on);
        assert!(!resolve(&layers(On, proj(Some(On), false), Some(Off))).on);
        // item inherits: the project decides
        let e = resolve(&layers(Off, proj(Some(On), false), Some(Inherit)));
        assert_eq!((e.on, e.source), (true, Source::Project));
        assert!(!resolve(&layers(On, proj(Some(Off), false), Some(Inherit))).on);
        // both inherit: the workspace decides
        let e = resolve(&layers(On, proj(Some(Inherit), false), Some(Inherit)));
        assert_eq!((e.on, e.source), (true, Source::Workspace));
        assert!(resolve(&layers(On, proj(None, false), Some(Inherit))).on);
    }

    #[test]
    fn scratch_items_and_itemless_decisions_skip_the_project() {
        use Mode::*;
        // an item without a project: there is no project layer to look at
        let e = resolve(&layers(On, None, Some(Inherit)));
        assert_eq!((e.on, e.source), (true, Source::Workspace));
        // a decision without an item ignores even a project layer handed in
        let e = resolve(&layers(On, proj(Some(Off), false), None));
        assert_eq!((e.on, e.source), (true, Source::Workspace));
        let e = resolve(&layers(Off, proj(Some(On), false), None));
        assert!(!e.on);
    }

    #[test]
    fn real_directories_default_off_but_can_be_turned_on() {
        use Mode::*;
        let e = resolve(&layers(On, proj(None, true), Some(Inherit)));
        assert_eq!((e.on, e.source), (false, Source::ProjectDefault));
        assert!(resolve(&layers(On, proj(Some(On), true), Some(Inherit))).on);
        // an explicit `inherit` row follows the workspace
        assert!(resolve(&layers(On, proj(Some(Inherit), true), Some(Inherit))).on);
        assert!(resolve(&layers(Off, proj(None, true), Some(On))).on);
    }

    #[test]
    fn forced_and_paused_beat_everything() {
        use Mode::*;
        for forced in [Forced::Env, Forced::NoResume] {
            let mut l = layers(On, proj(Some(On), false), Some(On));
            l.forced = Some(forced);
            assert_eq!(
                resolve(&l),
                Effective {
                    on: false,
                    full: false,
                    source: Source::Forced
                }
            );
        }
        let mut l = layers(On, proj(Some(On), false), Some(On));
        l.paused = true;
        assert_eq!(
            resolve(&l),
            Effective {
                on: false,
                full: false,
                source: Source::Paused
            }
        );
    }

    #[test]
    fn full_access_is_yolo_and_follows_the_same_layers() {
        use Mode::*;
        let e = resolve(&layers(Full, proj(None, false), Some(Inherit)));
        assert_eq!((e.on, e.full, e.source), (true, true, Source::Workspace));
        // an item can step down to plain YOLO or off, or up to full on its own
        let e = resolve(&layers(Full, proj(None, false), Some(On)));
        assert_eq!((e.on, e.full), (true, false));
        assert!(!resolve(&layers(Full, proj(Some(Full), false), Some(Off))).on);
        let e = resolve(&layers(Off, proj(Some(Inherit), false), Some(Full)));
        assert_eq!((e.on, e.full, e.source), (true, true, Source::Item));
        let e = resolve(&layers(On, proj(Some(Full), false), Some(Inherit)));
        assert_eq!((e.on, e.full, e.source), (true, true, Source::Project));
        // a real directory without a setting stays off even under a full workspace
        let e = resolve(&layers(Full, proj(None, true), Some(Inherit)));
        assert_eq!((e.on, e.full), (false, false));
        // forced off and paused beat full
        for forced in [Forced::Env, Forced::NoResume] {
            let mut l = layers(Full, proj(Some(Full), false), Some(Full));
            l.forced = Some(forced);
            let e = resolve(&l);
            assert_eq!((e.on, e.full), (false, false));
        }
        let mut l = layers(Full, proj(Some(Full), false), Some(Full));
        l.paused = true;
        assert!(!resolve(&l).full);
        assert_eq!(Mode::parse("full"), Some(Full));
        assert_eq!(Full.as_str(), "full");
    }

    #[test]
    fn forced_from_env() {
        assert_eq!(Forced::from_env(Some("off"), false), Some(Forced::Env));
        assert_eq!(Forced::from_env(Some("OFF"), true), Some(Forced::Env));
        assert_eq!(Forced::from_env(None, true), Some(Forced::NoResume));
        assert_eq!(Forced::from_env(Some("on"), false), None);
        assert_eq!(Forced::from_env(None, false), None);
    }
}
