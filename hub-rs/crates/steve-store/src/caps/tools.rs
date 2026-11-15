//! Tool search (migration 69): what a Bot keeps loaded in every session (`bot_tool_pins`) and how often
//! each tool came up in its searches and was called (`tool_usage`). Steve's core tools are always
//! loaded; the rest are found with `steve_tools_search` and run with `steve_tool_call`.

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::Result;

/// The kinds of pin: a tool by name, every tool of a library MCP server, a remote agent's tool.
pub const PIN_KINDS: &[&str] = &["tool", "mcp", "a2a"];

/// Days a usage row is kept.
pub const KEEP_DAYS: i64 = 90;

/// One pin of a Bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pin {
    pub kind: String,
    pub target: String,
}

/// A Bot's pins, oldest first.
pub fn pins(c: &Connection, bot: &str) -> Result<Vec<Pin>> {
    let mut st = c.prepare_cached(
        "SELECT kind, target FROM bot_tool_pins WHERE bot_id = ? ORDER BY created, kind, target",
    )?;
    let v = st
        .query_map([bot], |r| {
            Ok(Pin {
                kind: r.get(0)?,
                target: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Pins (`on`) or unpins one; whether anything changed.
pub fn set_pin(
    c: &Connection,
    bot: &str,
    kind: &str,
    target: &str,
    on: bool,
    now: i64,
) -> Result<bool> {
    let n = if on {
        c.execute(
            "INSERT OR IGNORE INTO bot_tool_pins (bot_id, kind, target, created) VALUES (?1, ?2, ?3, ?4)",
            params![bot, kind, target, now],
        )?
    } else {
        c.execute(
            "DELETE FROM bot_tool_pins WHERE bot_id = ? AND kind = ? AND target = ?",
            [bot, kind, target],
        )?
    };
    Ok(n > 0)
}

/// A library entry is gone: no Bot pins it any more.
pub fn drop_target(c: &Connection, kind: &str, target: &str) -> Result<usize> {
    Ok(c.execute(
        "DELETE FROM bot_tool_pins WHERE kind = ? AND target = ?",
        [kind, target],
    )?)
}

/// The day (since the Unix epoch, UTC) of a millisecond timestamp.
pub fn day_of(ms: i64) -> i64 {
    ms.div_euclid(86_400_000)
}

/// Counts `searched` appearances in a search and `called` calls of each tool for `bot` on `day`;
/// rows older than [`KEEP_DAYS`] go.
pub fn record(
    c: &Connection,
    bot: &str,
    day: i64,
    tools: &[&str],
    searched: i64,
    called: i64,
) -> Result<()> {
    let mut st = c.prepare_cached(
        "INSERT INTO tool_usage (bot_id, tool, day, searched, called) VALUES (?1, ?2, ?3, ?4, ?5)
           ON CONFLICT (bot_id, tool, day) DO UPDATE SET searched = searched + excluded.searched,
             called = called + excluded.called",
    )?;
    for t in tools {
        st.execute(params![bot, t, day, searched, called])?;
    }
    c.execute("DELETE FROM tool_usage WHERE day < ?", [day - KEEP_DAYS])?;
    Ok(())
}

/// One tool's use by a Bot over some days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub tool: String,
    pub searched: i64,
    pub called: i64,
}

/// `bot`'s use of each tool on the `days` days up to and including `today`, most called first.
pub fn usage(c: &Connection, bot: &str, today: i64, days: i64) -> Result<Vec<Usage>> {
    let mut st = c.prepare_cached(
        "SELECT tool, SUM(searched), SUM(called) FROM tool_usage WHERE bot_id = ? AND day > ?
          GROUP BY tool ORDER BY SUM(called) DESC, SUM(searched) DESC, tool",
    )?;
    let v = st
        .query_map(params![bot, today - days], |r| {
            Ok(Usage {
                tool: r.get(0)?,
                searched: r.get(1)?,
                called: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}
