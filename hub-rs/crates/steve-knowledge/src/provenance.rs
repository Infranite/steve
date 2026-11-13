//! Where a memory came from and where it was used: the `origin` of every memory the knowledge page
//! shows (the conversation day, item, session, mail thread, event or bot behind it, with a link
//! there), how often retrieval handed it out, the recent uses of a set of objects (a subject's
//! memories and documents), and the memory providers the 来源 tab lists.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::{ApiResult, Caller, Modules};
use steve_store::Result;
use steve_store::base::mems::{self, Mem, MemFilter};

use crate::repo;

/// What kind of origin a memory has, as the page names it: `you` (written by you), `conversation`
/// (a day of the main conversation), `item` (an item's delivery or its agent), `agent` (an agent
/// session without an item), `mail`, `space` (a chat space), `event` (a correction of an event
/// rule), `plugin` (a plugin or a synced provider), `other`.
pub fn origin_kind(m: &Mem, cand_origin: Option<&str>) -> &'static str {
    match (m.source_type.as_str(), cand_origin) {
        ("me", _) => "you",
        (_, Some("mail")) => "mail",
        (_, Some("space")) => "space",
        ("message", _) => "conversation",
        ("item", _) => "item",
        ("steve", _) => "agent",
        ("event", _) => "event",
        ("plugin" | "sync", _) => "plugin",
        _ => "other",
    }
}

/// The `mem_candidates` row of a memory (kept after it was answered): `(origin, origin_id, evidence)`.
fn candidate_of(c: &Connection, id: &str) -> Result<Option<(String, String, String)>> {
    Ok(
        c.prepare_cached(
            "SELECT origin, origin_id, evidence FROM mem_candidates WHERE mem_id = ?",
        )?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?,
    )
}

/// Who wrote it, as shown: `None` for you, Steve, a bot's name, or a plugin's id.
pub fn writer_name(c: &Connection, writer: &str) -> Result<Option<String>> {
    Ok(match writer {
        "" | "me" => None,
        "steve" => Some("Steve".into()),
        w if w.starts_with("plugin:") => Some(w.trim_start_matches("plugin:").to_string()),
        w => c
            .prepare_cached("SELECT name FROM bots WHERE id = ?")?
            .query_row([w], |r| r.get::<_, String>(0))
            .optional()?
            .or_else(|| Some(w.to_string())),
    })
}

/// The link to where a memory came from (empty when there is nothing to open).
fn origin_href(c: &Connection, m: &Mem, cand: Option<&(String, String, String)>) -> Result<String> {
    let id = m.source_id.clone().unwrap_or_default();
    if let Some((origin, oid, _)) = cand {
        match origin.as_str() {
            "mail" if !oid.is_empty() => return Ok(format!("#/mail/t/{oid}")),
            "space" if !oid.is_empty() => return Ok(format!("#/channels/space/{oid}")),
            _ => {}
        }
    }
    if id.is_empty() {
        return Ok(String::new());
    }
    Ok(match m.source_type.as_str() {
        "item" => format!("#/items/{id}"),
        "message" if id.starts_with("chat:") => {
            match repo::source_of_kind(c, &m.workspace_id, crate::sources::chat::KIND)? {
                Some(s) => repo::doc_by_ref(c, &s.id, &id)?
                    .filter(|d| d.status != "deleted")
                    .map(|d| format!("#/knowledge/doc/{}", d.id))
                    .unwrap_or_default(),
                None => String::new(),
            }
        }
        "steve" | "message" if id.starts_with("ses_") => format!("#/sessions/{id}"),
        "event" => format!("#/signals?e={id}"),
        _ => String::new(),
    })
}

/// `Mem.origin`: `{kind, type, id, label, href, writer, writer_name, evidence}`. `label` is the
/// stored `source_label` in the request's language (“11-12 的对话”, “ST-12 交付时”, “你写的”),
/// `evidence` the words it was based on, when it was a candidate first.
pub fn origin(c: &Connection, m: &Mem) -> Result<Value> {
    let cand = candidate_of(c, &m.id)?;
    let href = origin_href(c, m, cand.as_ref())?;
    Ok(json!({
        "kind": origin_kind(m, cand.as_ref().map(|x| x.0.as_str())),
        "type": m.source_type,
        "id": m.source_id,
        "label": crate::sources::label_shown(&m.source_label),
        "href": href,
        "writer": m.writer,
        "writer_name": writer_name(c, &m.writer)?,
        "evidence": cand.map(|x| x.2).unwrap_or_default(),
    }))
}

/// How often retrieval handed out each object of one type, and when last: `obj_id → (count, last)`.
pub fn use_counts(c: &Connection, ws: &str, obj_type: &str) -> Result<HashMap<String, (i64, i64)>> {
    let mut st = c.prepare_cached(
        "SELECT json_extract(j.value, '$.obj_id'), COUNT(*), MAX(u.created)
           FROM kb_uses u, json_each(u.hits) j
          WHERE u.workspace_id = ? AND json_extract(j.value, '$.obj_type') = ?
          GROUP BY 1",
    )?;
    let rows = st.query_map(params![ws, obj_type], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get(1)?, r.get(2)?))
    })?;
    let mut out = HashMap::new();
    for r in rows {
        let (id, n, last) = r?;
        if let Some(id) = id {
            out.insert(id, (n, last));
        }
    }
    Ok(out)
}

/// `used: {count, last}` on a memory or document JSON.
pub fn add_used(v: &mut Value, counts: &HashMap<String, (i64, i64)>) {
    let id = v.get("id").and_then(Value::as_str).unwrap_or_default();
    let (n, last) = counts.get(id).copied().unwrap_or((0, 0));
    if let Some(o) = v.as_object_mut() {
        o.insert(
            "used".into(),
            json!({"count": n, "last": (n > 0).then_some(last)}),
        );
    }
}

/// Where a consumer of knowledge can be opened, and how it is named.
fn consumer_link(c: &Connection, ctype: &str, cid: &str, query: &str) -> Result<(String, String)> {
    Ok(match ctype {
        "session" => {
            let label: Option<(Option<String>, Option<String>)> = c
                .prepare_cached("SELECT item_id, bot_id FROM sessions WHERE id = ?")
                .and_then(|mut st| {
                    st.query_row([cid], |r| Ok((r.get(0)?, r.get(1)?)))
                        .optional()
                })
                .unwrap_or(None);
            let item = label.and_then(|x| x.0);
            (
                format!("#/sessions/{cid}"),
                item.map(|i| steve_i18n::t!("knowledge.use.session_of", item = i))
                    .unwrap_or_else(|| steve_i18n::t!("knowledge.use.session")),
            )
        }
        "chat" => (String::new(), steve_i18n::t!("knowledge.use.chat")),
        "search" => (
            format!("#/knowledge?q={}", urlencoding(query)),
            steve_i18n::t!("knowledge.use.search", q = query),
        ),
        "draft" => (String::new(), steve_i18n::t!("knowledge.use.draft")),
        "flow" => (String::new(), steve_i18n::t!("knowledge.use.flow")),
        _ => (String::new(), ctype.to_string()),
    })
}

fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The most recent uses of any of `objs` (`(obj_type, obj_id)`), newest first: who was given it
/// (`consumer_type`, a link and a name), the query, the object and when.
pub fn recent_uses(
    c: &Connection,
    ws: &str,
    objs: &[(String, String)],
    limit: usize,
) -> Result<Vec<Value>> {
    if objs.is_empty() {
        return Ok(Vec::new());
    }
    let want: std::collections::HashSet<(&str, &str)> =
        objs.iter().map(|(t, i)| (t.as_str(), i.as_str())).collect();
    let mut st = c.prepare_cached(
        "SELECT u.consumer_type, u.consumer_id, u.query, u.created,
                json_extract(j.value, '$.obj_type'), json_extract(j.value, '$.obj_id'),
                json_extract(j.value, '$.label')
           FROM kb_uses u, json_each(u.hits) j
          WHERE u.workspace_id = ? AND json_extract(j.value, '$.obj_type') IN ('mem', 'chunk')
          ORDER BY u.created DESC, u.rowid DESC",
    )?;
    let rows = st.query_map([ws], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        ))
    })?;
    let mut out = Vec::new();
    // one line per consumer and object: a session searched three times shows once
    let mut seen = std::collections::HashSet::new();
    for r in rows {
        let (ctype, cid, query, created, otype, oid, label) = r?;
        if !want.contains(&(otype.as_str(), oid.as_str()))
            || !seen.insert((cid.clone(), oid.clone()))
        {
            continue;
        }
        let (href, name) = consumer_link(c, &ctype, &cid, &query)?;
        out.push(json!({
            "consumer_type": ctype, "consumer_id": cid, "href": href, "name": name,
            "query": query, "created": created, "obj": {"type": otype, "id": oid, "label": label},
        }));
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// `GET /kb/providers`: who writes the memories, grouped by origin kind and writer — what the
/// 来源 tab shows next to the document sources: `[{kind, writer, writer_name, count, confirmed,
/// candidates, stale, last}]`, largest first; and the search store in use.
pub fn providers(m: &Modules, kb: &crate::state::Knowledge, who: &Caller) -> ApiResult<Value> {
    let viewer = m.viewer(who)?;
    let f = MemFilter {
        include_never: true,
        ..MemFilter::default()
    };
    let list = m.store.reader(|c| {
        let all = mems::all(c, &viewer, &f)?;
        let mut groups: Vec<(String, String, [i64; 4], i64)> = Vec::new();
        for x in &all {
            let cand = candidate_of(c, &x.id)?;
            let kind = origin_kind(x, cand.as_ref().map(|y| y.0.as_str())).to_string();
            let writer = if kind == "you" {
                String::new()
            } else {
                x.writer.clone()
            };
            let at = match groups.iter().position(|g| g.0 == kind && g.1 == writer) {
                Some(i) => i,
                None => {
                    groups.push((kind, writer, [0; 4], 0));
                    groups.len() - 1
                }
            };
            let g = &mut groups[at];
            g.2[0] += 1;
            match x.status.as_str() {
                "confirmed" => g.2[1] += 1,
                "candidate" => g.2[2] += 1,
                _ => g.2[3] += 1,
            }
            g.3 = g.3.max(x.created);
        }
        groups.sort_by(|a, b| b.2[0].cmp(&a.2[0]));
        let mut out = Vec::new();
        for (kind, writer, n, last) in groups {
            out.push(json!({
                "kind": kind, "writer": writer, "writer_name": writer_name(c, &writer)?,
                "count": n[0], "confirmed": n[1], "candidates": n[2], "stale": n[3], "last": last,
            }));
        }
        Ok::<_, steve_store::StoreError>(out)
    })?;
    Ok(json!({ "memory": list, "store": kb.store().id() }))
}
