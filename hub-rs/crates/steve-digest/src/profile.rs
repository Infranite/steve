//! A conversation's profile: its kind, its role, and how it is digested.
//!
//! Kinds (`conv_profiles.kind`): `project` 专项群 (short-lived, with a goal), `team` 团队群 (long-lived),
//! `feed` 信息归类群 (alerts, notifications, daily reports: mostly machine messages), `dm` 私聊, `mail` 邮件线程,
//! `pr`, `issue`. Until it is judged (by the digest Bot, `model`) or set by you (`you`), a rule gives the
//! first guess (`rule`): a DM is `dm`, a mail thread `mail`, a group where most recent messages come from bots
//! `feed`, any other group `team`. A rule guess follows the conversation as it changes; a judgement stays.
//!
//! Each setting is either yours or the kind's default ([`defaults`]): window size, quiet time, maximum wait,
//! depth, daily budget, priority and the notification level. Depths: `full` 摘要+关联+待办, `summary` 只摘要,
//! `alerts` 规则汇总+异常 (the default of `feed`), `record` 只记录 (nothing is queued; the cursor moves on),
//! `paused` 暂停 (nothing is queued; the messages wait). Notification levels: `now` 立即通知, `digest` 进摘要,
//! `record` 只记录; each is `default`, `learned` (Steve, from what you open and ignore — phase E) or `you`.
//! The role is the space's duty (`space_settings.duty`), edited where it always was.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::{StoreError, Tx};

use crate::conv::{self, ConvInfo, ConvRef};

type Result<T> = std::result::Result<T, StoreError>;

pub const KINDS: [&str; 7] = ["project", "team", "feed", "dm", "mail", "pr", "issue"];
pub const KIND_SOURCES: [&str; 3] = ["rule", "model", "you"];
pub const DEPTHS: [&str; 5] = ["full", "summary", "alerts", "record", "paused"];
pub const NOTIFY: [&str; 3] = ["now", "digest", "record"];
pub const NOTIFY_SOURCES: [&str; 3] = ["default", "learned", "you"];

pub const MINUTE: i64 = 60 * 1000;
pub const HOUR: i64 = 60 * MINUTE;

/// Bounds of what may be set.
pub const WINDOW_MIN: i64 = 5;
pub const WINDOW_MAX: i64 = 2000;
pub const QUIET_MAX: i64 = 24 * HOUR;
pub const WAIT_MAX: i64 = 7 * 24 * HOUR;
pub const BUDGET_MAX: i64 = 10_000_000;

/// The settings that decide how a conversation is digested.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub window: i64,
    pub quiet_ms: i64,
    pub max_wait_ms: i64,
    pub depth: String,
    /// Estimated tokens of packets per day; `0`: no limit.
    pub daily_budget: i64,
    pub priority: i64,
    pub notify: String,
}

/// The defaults of a kind.
pub fn defaults(kind: &str) -> Settings {
    let (window, depth, budget, priority, notify) = match kind {
        "dm" => (50, "full", 200_000, 2, "now"),
        "project" => (50, "full", 300_000, 2, "digest"),
        "feed" => (50, "alerts", 100_000, 0, "record"),
        "mail" => (50, "full", 200_000, 1, "digest"),
        "pr" | "issue" => (50, "summary", 100_000, 1, "digest"),
        _ => (50, "full", 300_000, 1, "digest"),
    };
    Settings {
        window,
        quiet_ms: 30 * MINUTE,
        max_wait_ms: 24 * HOUR,
        depth: depth.into(),
        daily_budget: budget,
        priority,
        notify: notify.into(),
    }
}

/// What is stored for a conversation (`None`: follows the kind).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    pub kind: Option<String>,
    pub kind_source: String,
    pub window: Option<i64>,
    pub quiet_ms: Option<i64>,
    pub max_wait_ms: Option<i64>,
    pub depth: Option<String>,
    pub daily_budget: Option<i64>,
    pub priority: Option<i64>,
    pub notify: Option<String>,
    pub notify_source: String,
    pub revision: i64,
}

pub fn stored(c: &Connection, ws: &str, conv: &ConvRef) -> Result<Option<Stored>> {
    Ok(c
        .prepare_cached(
            "SELECT kind, kind_source, window_size, quiet_ms, max_wait_ms, depth, daily_budget, priority, notify, notify_source, revision
               FROM conv_profiles WHERE workspace_id = ? AND conv = ?",
        )?
        .query_row(params![ws, conv.key()], |r| {
            Ok(Stored {
                kind: r.get(0)?,
                kind_source: r.get(1)?,
                window: r.get(2)?,
                quiet_ms: r.get(3)?,
                max_wait_ms: r.get(4)?,
                depth: r.get(5)?,
                daily_budget: r.get(6)?,
                priority: r.get(7)?,
                notify: r.get(8)?,
                notify_source: r.get(9)?,
                revision: r.get(10)?,
            })
        })
        .optional()?)
}

/// The rule's guess of a conversation's kind.
pub fn guess(c: &Connection, ws: &str, info: &ConvInfo) -> Result<&'static str> {
    Ok(match info.source_kind.as_str() {
        "dm" => "dm",
        "mail_thread" => "mail",
        _ => match conv::bot_share(c, ws, &info.conv, 200)? {
            Some(s) if s >= 0.6 => "feed",
            _ => "team",
        },
    })
}

/// A conversation's profile as the page and the module read it.
#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub conv: ConvRef,
    pub kind: String,
    pub kind_source: String,
    /// Each setting in effect.
    pub settings: Settings,
    /// The settings you set (the others follow the kind).
    pub own: Vec<&'static str>,
    pub notify_source: String,
    pub revision: i64,
}

/// The profile of `info`'s conversation: what is stored, else the rule's guess and the kind's defaults.
pub fn profile(c: &Connection, ws: &str, info: &ConvInfo) -> Result<Profile> {
    let s = stored(c, ws, &info.conv)?.unwrap_or_else(|| Stored {
        kind_source: "rule".into(),
        notify_source: "default".into(),
        ..Default::default()
    });
    let (kind, kind_source) = match (&s.kind, s.kind_source.as_str()) {
        (Some(k), src) if src != "rule" => (k.clone(), src.to_string()),
        _ => (guess(c, ws, info)?.to_string(), "rule".to_string()),
    };
    let d = defaults(&kind);
    let mut own = Vec::new();
    let mut pick = |name: &'static str, v: Option<i64>, d: i64| match v {
        Some(v) => {
            own.push(name);
            v
        }
        None => d,
    };
    let window = pick("window", s.window, d.window);
    let quiet_ms = pick("quiet_ms", s.quiet_ms, d.quiet_ms);
    let max_wait_ms = pick("max_wait_ms", s.max_wait_ms, d.max_wait_ms);
    let daily_budget = pick("daily_budget", s.daily_budget, d.daily_budget);
    let priority = pick("priority", s.priority, d.priority);
    if s.depth.is_some() {
        own.push("depth");
    }
    // a muted space (只记录) is recorded, never digested, whatever its depth says
    let depth = if info.muted || info.attention == "blocked" {
        "record".to_string()
    } else {
        s.depth.clone().unwrap_or(d.depth)
    };
    let notify = match &s.notify {
        Some(n) => {
            own.push("notify");
            n.clone()
        }
        None => d.notify,
    };
    Ok(Profile {
        conv: info.conv.clone(),
        kind,
        kind_source,
        settings: Settings {
            window,
            quiet_ms,
            max_wait_ms,
            depth,
            daily_budget,
            priority,
            notify,
        },
        own,
        notify_source: if s.notify.is_some() {
            s.notify_source
        } else {
            "default".into()
        },
        revision: s.revision,
    })
}

/// A change of a profile: each field `Some(Value::Null)` clears it (follows the kind again).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    #[serde(default, deserialize_with = "present")]
    pub kind: Option<Value>,
    /// Who sets the kind: `you` from the page (the default), `model` from the digest Bot.
    #[serde(default)]
    pub kind_source: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub window: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub quiet_ms: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub max_wait_ms: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub depth: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub daily_budget: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub priority: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub notify: Option<Value>,
    /// `you` from the page (the default), `learned` from Steve.
    #[serde(default)]
    pub notify_source: Option<String>,
    /// The revision the change was made on; `409` when it moved.
    #[serde(default)]
    pub revision: Option<i64>,
}

fn present<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

/// A field that does not fit (its name).
#[derive(Debug)]
pub struct Bad(pub &'static str);

fn int(v: &Value, name: &'static str, lo: i64, hi: i64) -> std::result::Result<Option<i64>, Bad> {
    match v {
        Value::Null => Ok(None),
        v => v
            .as_i64()
            .filter(|n| (lo..=hi).contains(n))
            .map(Some)
            .ok_or(Bad(name)),
    }
}

fn word(v: &Value, name: &'static str, of: &[&str]) -> std::result::Result<Option<String>, Bad> {
    match v {
        Value::Null => Ok(None),
        v => v
            .as_str()
            .filter(|s| of.contains(s))
            .map(|s| Some(s.to_string()))
            .ok_or(Bad(name)),
    }
}

/// Applies `p` (validated) on top of what is stored; returns the new revision. `Err(Ok(name))` for a field
/// that does not fit, `Err(Err(_))` for a store error; `Ok(None)` when the revision moved.
pub fn apply(
    tx: &Tx,
    ws: &str,
    conv: &ConvRef,
    p: &Patch,
    now: i64,
) -> std::result::Result<Option<i64>, std::result::Result<Bad, StoreError>> {
    let mut s = stored(tx, ws, conv)
        .map_err(Err)?
        .unwrap_or_else(|| Stored {
            kind_source: "rule".into(),
            notify_source: "default".into(),
            ..Default::default()
        });
    if let Some(r) = p.revision
        && r != s.revision
    {
        return Ok(None);
    }
    let check = |r: std::result::Result<(), Bad>| r.map_err(Ok);
    check((|| {
        if let Some(v) = &p.kind {
            s.kind = word(v, "kind", &KINDS)?;
            s.kind_source = match (&s.kind, p.kind_source.as_deref()) {
                (None, _) => "rule".into(),
                (Some(_), Some(src @ ("model" | "you"))) => src.into(),
                (Some(_), None) => "you".into(),
                (Some(_), Some(_)) => return Err(Bad("kind_source")),
            };
        }
        if let Some(v) = &p.window {
            s.window = int(v, "window", WINDOW_MIN, WINDOW_MAX)?;
        }
        if let Some(v) = &p.quiet_ms {
            s.quiet_ms = int(v, "quiet_ms", MINUTE, QUIET_MAX)?;
        }
        if let Some(v) = &p.max_wait_ms {
            s.max_wait_ms = int(v, "max_wait_ms", 10 * MINUTE, WAIT_MAX)?;
        }
        if let Some(v) = &p.depth {
            s.depth = word(v, "depth", &DEPTHS)?;
        }
        if let Some(v) = &p.daily_budget {
            s.daily_budget = int(v, "daily_budget", 0, BUDGET_MAX)?;
        }
        if let Some(v) = &p.priority {
            s.priority = int(v, "priority", 0, 3)?;
        }
        if let Some(v) = &p.notify {
            s.notify = word(v, "notify", &NOTIFY)?;
            s.notify_source = match (&s.notify, p.notify_source.as_deref()) {
                (None, _) => "default".into(),
                (Some(_), Some(src @ ("learned" | "you"))) => src.into(),
                (Some(_), None) => "you".into(),
                (Some(_), Some(_)) => return Err(Bad("notify_source")),
            };
        }
        Ok(())
    })())?;
    let rev = s.revision + 1;
    tx.execute(
        "INSERT INTO conv_profiles (workspace_id, conv, kind, kind_source, window_size, quiet_ms, max_wait_ms, depth,
            daily_budget, priority, notify, notify_source, revision, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (workspace_id, conv) DO UPDATE SET kind = excluded.kind, kind_source = excluded.kind_source,
            window_size = excluded.window_size, quiet_ms = excluded.quiet_ms, max_wait_ms = excluded.max_wait_ms,
            depth = excluded.depth, daily_budget = excluded.daily_budget, priority = excluded.priority,
            notify = excluded.notify, notify_source = excluded.notify_source, revision = excluded.revision,
            updated = excluded.updated",
        params![
            ws,
            conv.key(),
            s.kind,
            s.kind_source,
            s.window,
            s.quiet_ms,
            s.max_wait_ms,
            s.depth,
            s.daily_budget,
            s.priority,
            s.notify,
            s.notify_source,
            rev,
            now
        ],
    )
    .map_err(|e| Err(e.into()))?;
    crate::event(tx, ws, &conv.key());
    Ok(Some(rev))
}

/// The profile as JSON, with every kind's defaults so the page can say what “跟随类型” means.
pub fn json(p: &Profile) -> Value {
    json!({
        "conv": p.conv.key(), "kind": p.kind, "kind_source": p.kind_source,
        "settings": p.settings, "own": p.own, "notify_source": p.notify_source,
        "revision": p.revision, "defaults": defaults(&p.kind),
    })
}
