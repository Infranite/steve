//! Keeping memories in the index: confirmed `mems` (one entry each, prefixed
//! with who or what they are about) and the old `bot_memories` (read only, “<Bot> 的记忆”).
//!
//! Event-driven ([`on_event`] on the bus's `mm` and `bot` events, wired by the integrator) with a
//! periodic [`sweep`] (`kb.sync_mems`, every 5 minutes) that catches anything an event missed.

use std::collections::HashMap;
use std::time::Duration;
use steve_store::knowledge::{BotMemories, Chunks, Documents, Memories};
use steve_store::{Read, References, Workspaces};

use steve_modules::Modules;
use steve_store::base::mems::Mem;
use steve_store::base::{Level, ObjRef};
use steve_store::{BusEvent, Result, StoreError, W, sha};

use crate::chunk::Chunk;
use crate::index::{self, ChunkMeta};
use crate::sources;
use steve_modules::contacts::memory::record_name;

/// How often the sweep runs.
pub const SWEEP_EVERY: Duration = Duration::from_secs(5 * 60);

/// Names of the objects a memory is about, “、”-joined; unknown objects are left out.
pub fn about_names(c: &mut impl Read, about: &[ObjRef]) -> Result<String> {
    joined_names(about, |a| References(c).name(a))
}

fn joined_names(
    about: &[ObjRef],
    mut name: impl FnMut(&ObjRef) -> Result<Option<String>>,
) -> Result<String> {
    let mut names = Vec::new();
    for a in about {
        if let Some(n) = name(a)?.filter(|n| !n.is_empty())
            && !names.contains(&n)
        {
            names.push(n);
        }
    }
    Ok(names.join("、"))
}

/// The citation label of a memory: what it is (长期记忆 or 知识条目, ST-41), then whom it is about.
pub fn mem_label(kind: &str, names: &str) -> String {
    let what = record_name(kind);
    if names.is_empty() {
        what.to_string()
    } else {
        format!("{what} · {names}")
    }
}

/// What a memory's citation label says after what it is: the names, or the label itself without any.
/// Labels written before ST-41 start with “记忆”.
pub fn mem_title(label: &str) -> &str {
    ["长期记忆 · ", "知识条目 · ", "记忆 · "]
        .iter()
        .find_map(|p| label.strip_prefix(p))
        .unwrap_or(label)
}

/// Does this memory belong in the index? Confirmed, not `never`, not expired.
fn usable(m: &Mem, now: i64) -> bool {
    m.status == "confirmed" && m.level != Level::Never && m.valid_until.is_none_or(|v| v > now)
}

/// Re-indexes one memory (or removes it: deleted, candidate, stale, `never`, expired).
pub fn sync_mem(tx: &mut W<'_>, id: &str, now: i64) -> Result<bool> {
    let Some(m) = Memories(tx).get(id)? else {
        index::remove(tx, "mem", id)?;
        return Ok(false);
    };
    if !usable(&m, now) {
        index::remove(tx, "mem", id)?;
        return Ok(false);
    }
    let names = joined_names(&m.about, |a| References(tx).name(a))?;
    let text = if names.is_empty() {
        m.text.clone()
    } else {
        format!("{names}：{}", m.text)
    };
    let label = mem_label(&m.kind, &names);
    let len = text.chars().count();
    let c = Chunk {
        seq: 0,
        heading: String::new(),
        text,
        start: 0,
        end: len,
    };
    index::replace(
        tx,
        &ChunkMeta {
            ws: &m.workspace_id,
            obj_type: "mem",
            obj_id: &m.id,
            title: "",
            visibility: &m.visibility,
            level: m.level,
            created_by: &m.created_by,
            source_kind: "mems",
            label: &label,
            updated: m.updated,
        },
        &[c],
    )?;
    Ok(true)
}

/// Memories of `ws` changed since the last sweep, plus index entries whose memory is gone or expired.
pub fn sweep_mems(tx: &mut W<'_>, ws: &str, now: i64) -> Result<usize> {
    let since: i64 = Documents(tx)
        .mark(ws, "sync:mems")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(-1);
    let changed = Memories(tx).updated_since(ws, since)?;
    let indexable: std::collections::HashSet<_> =
        Memories(tx).indexable(ws, now)?.into_iter().collect();
    let stale = Chunks(tx)
        .object_ids(ws, "mem")?
        .into_iter()
        .filter(|id| !indexable.contains(id))
        .collect::<Vec<_>>();
    let mut n = 0;
    let mut mark = since;
    for (id, updated) in &changed {
        sync_mem(tx, id.as_str(), now)?;
        mark = mark.max(*updated);
        n += 1;
    }
    for id in &stale {
        index::remove(tx, "mem", id)?;
        n += 1;
    }
    if mark != since {
        Documents(tx).set_mark(ws, "sync:mems", &mark.to_string(), now)?;
    }
    Ok(n)
}

/// Makes the `bot_mem` entries of `ws` match `bot_memories` (the old table has no `updated` and is
/// small, so it is compared whole; a hash in `kb_marks` skips the work when nothing changed).
pub fn sync_bot_mems(tx: &mut W<'_>, ws: &str, actor: &str, now: i64) -> Result<usize> {
    let src = sources::builtin(tx, ws, "bot_mems", actor, now)?;
    let facts = BotMemories(tx).in_workspace(ws)?;
    let bot_ids: Vec<_> = facts.iter().map(|(_, bot, _)| bot.as_str()).collect();
    let names = References(tx).bot_names(&bot_ids)?;
    let rows: Vec<_> = facts
        .into_iter()
        .map(|(id, bot, text)| {
            let name = names.get(&bot).cloned().unwrap_or_default();
            (id, bot, text, name)
        })
        .collect();
    let mut all = format!("{}|{}\n", src.visibility.code(), src.level.code());
    for (id, bot, text, name) in &rows {
        all.push_str(&format!("{id}\t{bot}\t{name}\t{text}\n"));
    }
    let hash = sha(&all);
    if Documents(tx).mark(ws, "sync:bot_mems")?.as_deref() == Some(hash.as_str()) {
        return Ok(0);
    }
    let have: HashMap<_, _> = Chunks(tx).texts(ws, "bot_mem")?.into_iter().collect();
    let mut n = 0;
    for (id, _bot, text, name) in &rows {
        let label = format!("{name} 的记忆");
        if have.get(id).is_some_and(|(t, l)| t == text && *l == label) {
            continue;
        }
        let len = text.chars().count();
        index::replace(
            tx,
            &ChunkMeta {
                ws,
                obj_type: "bot_mem",
                obj_id: id,
                title: name,
                visibility: &src.visibility,
                level: src.level,
                created_by: &src.created_by,
                source_kind: "bot_mems",
                label: &label,
                updated: now,
            },
            &[Chunk {
                seq: 0,
                heading: String::new(),
                text: text.clone(),
                start: 0,
                end: len,
            }],
        )?;
        n += 1;
    }
    for id in have.keys() {
        if !rows.iter().any(|r| &r.0 == id) {
            index::remove(tx, "bot_mem", id)?;
            n += 1;
        }
    }
    // Scope changes of the source reach entries whose text did not change.
    index::rescope_source(tx, &src)?;
    Documents(tx).set_mark(ws, "sync:bot_mems", &hash, now)?;
    Ok(n)
}

fn workspaces(c: &mut impl Read) -> Result<Vec<String>> {
    let mut ids = Workspaces(c).ids()?;
    ids.sort();
    Ok(ids)
}

/// Reacts to a bus event: `mm` re-indexes that memory, `bot` re-compares the old Bot memories of
/// every workspace, preserving the full memory reconciliation.
pub fn on_event(m: &Modules, e: &BusEvent) -> Result<()> {
    let now = m.clock.now();
    match e {
        steve_types::BusEvent::Domain(steve_types::DomainEvent::Config(
            steve_types::ConfigEvent::Mm { id, .. },
        )) => {
            m.store.write(|tx| sync_mem(tx, id.as_str(), now))?;
        }
        steve_types::BusEvent::Domain(steve_types::DomainEvent::Config(
            steve_types::ConfigEvent::Bot { .. },
        )) => {
            let wss = m.store.read(|r| workspaces(r))?;
            m.store.write(|tx| {
                for ws in &wss {
                    sync_bot_mems(tx, ws, "steve", now)?;
                }
                Ok::<_, StoreError>(())
            })?;
        }
        _ => {}
    }
    Ok(())
}

/// One sweep over every workspace: built-in sources, memories, Bot memories.
pub fn sweep(m: &Modules) -> Result<usize> {
    let now = m.clock.now();
    let wss = m.store.read(|r| workspaces(r))?;
    m.store.write(|tx| {
        let mut n = 0;
        for ws in &wss {
            sources::ensure_builtin(tx, ws, "steve", now)?;
            n += sweep_mems(tx, ws, now)?;
            n += sync_bot_mems(tx, ws, "steve", now)?;
        }
        Ok(n)
    })
}

/// The `kb.sync_mems` job: a sweep now, then every [`SWEEP_EVERY`].
pub async fn sweep_job(m: Modules) {
    steve_runtime::every(
        "kb.sync_mems",
        m.clock.clone(),
        SWEEP_EVERY,
        steve_runtime::FirstRun::Immediate,
        move || {
            let m = m.clone();
            async move {
                let mm = m.clone();
                match tokio::task::spawn_blocking(move || sweep(&mm)).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => tracing::warn!("kb.sync_mems: {e}"),
                    Err(e) => tracing::warn!("kb.sync_mems: task failed: {e}"),
                }
            }
        },
    )
    .await;
}
