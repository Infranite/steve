//! Alert-type conversations (信息归类群, depth `alerts`): aggregated by rule, not read by a model.
//!
//! Each machine message is an alert of a category: its first line without the leading bracketed status
//! (`【P1 告警】`, `[FIRING]`), with numbers as `#` and long hex ids as `*`. A message whose first line says it
//! recovered (恢复, 已解决, resolved, recovered, 告警解除 …) recovers its category. A person who answers an alert
//! (replies to it, or writes in its thread) is its handler. The rollup per category (fired, recovered, still
//! open, handlers, last time) is kept in `alert_rollups` and written into the conversation's summary as its
//! rule part; only the anomalies — what people said, and alerts still open at the end of the window — go into
//! a packet for the digest Bot.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use steve_store::{StoreError, Tx};

use crate::conv::{ConvRef, Msg};

type Result<T> = std::result::Result<T, StoreError>;

static LEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:(?:【[^】]*】|\[[^\]]*\]|\([^)]*\)|（[^）]*）)\s*)+").expect("lead")
});
static HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9a-fA-F]{8,}\b").expect("hex"));
static DIGITS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[0-9]+(?:\.[0-9]+)?").expect("digits"));
static SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("space"));

const RECOVERED: [&str; 9] = [
    "已恢复",
    "恢复",
    "已解决",
    "告警解除",
    "已消除",
    "resolved",
    "recovered",
    "back to normal",
    "[ok]",
];

/// The category of an alert's text.
pub fn category(text: &str) -> String {
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let s = LEAD.replace(first, "");
    let s = HEX.replace_all(&s, "*");
    let s = DIGITS.replace_all(&s, "#");
    let s = SPACE.replace_all(s.trim(), " ");
    let s: String = s.chars().take(60).collect();
    if s.is_empty() { "?".into() } else { s }
}

/// Whether an alert's first line says it recovered.
pub fn recovered(text: &str) -> bool {
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_lowercase();
    RECOVERED.iter().any(|w| first.contains(w))
}

#[derive(Debug, Default)]
struct Cat {
    fired: i64,
    recovered: i64,
    open: bool,
    handlers: Vec<String>,
    first_at: i64,
    last_at: i64,
    last_id: String,
    last_fire: Option<usize>,
}

/// What a window of an alert conversation came to.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Indices (into the window's messages) of the anomalies, in order.
    pub anomalies: Vec<usize>,
    /// The conversation's rollup after the window, newest first (the summary's rule part).
    pub rollup: Value,
    pub alerts: i64,
}

/// Aggregates a window of `conv` into its rollup and picks the anomalies.
pub fn aggregate(tx: &Tx, ws: &str, conv: &ConvRef, msgs: &[Msg]) -> Result<Outcome> {
    let key = conv.key();
    let mut cats: BTreeMap<String, Cat> = BTreeMap::new();
    let mut cat_of: HashMap<&str, String> = HashMap::new();
    let mut thread_cat: HashMap<String, String> = HashMap::new();
    let mut anomalies: Vec<usize> = Vec::new();
    let mut alerts = 0;
    for (i, m) in msgs.iter().enumerate() {
        if m.sender.bot {
            alerts += 1;
            let k = category(&m.text);
            let e = cats.entry(k.clone()).or_default();
            if e.first_at == 0 {
                e.first_at = m.at;
            }
            e.last_at = m.at;
            e.last_id = m.id.clone();
            if recovered(&m.text) {
                e.recovered += 1;
                e.open = false;
                e.last_fire = None;
            } else {
                e.fired += 1;
                e.open = true;
                e.last_fire = Some(i);
            }
            cat_of.insert(m.id.as_str(), k.clone());
            if let Some(t) = &m.thread {
                thread_cat.entry(t.clone()).or_insert(k);
            }
        } else {
            // a person: discussion, and the handler of the alert answered
            anomalies.push(i);
            let k = m
                .reply_to
                .as_deref()
                .and_then(|r| cat_of.get(r).cloned())
                .or_else(|| m.thread.as_ref().and_then(|t| thread_cat.get(t).cloned()));
            if let Some(k) = k {
                if let Some(r) = m.reply_to.as_deref()
                    && let Some(j) = msgs.iter().position(|x| x.id == r)
                {
                    anomalies.push(j);
                }
                if let (Some(e), Some(p)) = (cats.get_mut(&k), &m.sender.party)
                    && !e.handlers.contains(p)
                {
                    e.handlers.push(p.clone());
                }
            }
        }
    }
    for e in cats.values() {
        if e.open
            && let Some(i) = e.last_fire
        {
            anomalies.push(i);
        }
    }
    anomalies.sort_unstable();
    anomalies.dedup();

    for (k, e) in &cats {
        let old: Option<(i64, i64, String, i64)> = tx
            .prepare_cached(
                "SELECT fired, recovered, handlers, first_at FROM alert_rollups WHERE workspace_id = ? AND conv = ? AND category = ?",
            )?
            .query_row(params![ws, key, k], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .optional()?;
        let (fired, rec, mut handlers, first) = match old {
            Some((f, r, h, first)) => (
                f + e.fired,
                r + e.recovered,
                serde_json::from_str::<Vec<String>>(&h).unwrap_or_default(),
                first,
            ),
            None => (e.fired, e.recovered, Vec::new(), e.first_at),
        };
        for h in &e.handlers {
            if !handlers.contains(h) {
                handlers.push(h.clone());
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO alert_rollups (workspace_id, conv, category, fired, recovered, open, handlers, first_at, last_at, last_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                ws,
                key,
                k,
                fired,
                rec,
                e.open as i64,
                serde_json::to_string(&handlers).unwrap_or_default(),
                first,
                e.last_at,
                e.last_id
            ],
        )?;
    }
    Ok(Outcome {
        anomalies,
        rollup: rollup(tx, ws, conv)?,
        alerts,
    })
}

/// The rollup of `conv`, newest category first (at most 30).
pub fn rollup(c: &rusqlite::Connection, ws: &str, conv: &ConvRef) -> Result<Value> {
    let mut st = c.prepare_cached(
        "SELECT r.category, r.fired, r.recovered, r.open, r.handlers, r.first_at, r.last_at, r.last_id
           FROM alert_rollups r WHERE r.workspace_id = ? AND r.conv = ? ORDER BY r.last_at DESC LIMIT 30",
    )?;
    let rows: Vec<Value> = st
        .query_map(params![ws, conv.key()], |r| {
            let handlers: Vec<String> =
                serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default();
            Ok(json!({
                "category": r.get::<_, String>(0)?, "fired": r.get::<_, i64>(1)?, "recovered": r.get::<_, i64>(2)?,
                "open": r.get::<_, i64>(3)? != 0, "handlers": handlers,
                "first_at": r.get::<_, i64>(5)?, "last_at": r.get::<_, i64>(6)?, "last_id": r.get::<_, String>(7)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut rows = rows;
    for r in &mut rows {
        let names: Vec<String> = r["handlers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|p| {
                c.query_row("SELECT name FROM parties WHERE id = ?", [p], |x| {
                    x.get::<_, String>(0)
                })
                .ok()
            })
            .collect();
        r["handler_names"] = json!(names);
    }
    Ok(Value::Array(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_ignore_status_and_numbers() {
        assert_eq!(
            category("【P1 告警】订单服务 CPU 使用率 92%\n详情…"),
            "订单服务 CPU 使用率 #%"
        );
        assert_eq!(
            category("【已恢复】订单服务 CPU 使用率 45.5%"),
            "订单服务 CPU 使用率 #%"
        );
        assert_eq!(
            category("[FIRING] disk full on host-3f9a2b7c1d"),
            "disk full on host-*"
        );
        assert!(recovered("【已恢复】订单服务"));
        assert!(recovered("[RESOLVED] disk"));
        assert!(!recovered("【P1 告警】订单服务"));
    }
}
