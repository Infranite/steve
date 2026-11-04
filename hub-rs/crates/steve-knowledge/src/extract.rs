//! Extracting candidate memories with the model gateway (knowledge.md §5.1).
//!
//! One call per origin (an item completion, a day of chat, a mail thread): the text, the confirmed
//! memories it resembles (so the model can say “duplicate of” or “replaces”) and the names it may use
//! for “关于”. The server then drops duplicates, resolves names only exactly (never guesses), takes
//! the stricter level and keeps at most 5. Other modules call [`submit`].

use std::time::Duration;

use rusqlite::Connection;
use serde_json::Value;
use steve_modules::contacts::people_repo;
use steve_modules::{ApiError, ApiResult, Modules, Tier};
use steve_store::base::mems::{self, Mem};
use steve_store::base::{Level, ObjRef, Viewer};
use tokio::sync::Semaphore;

use crate::candidates::{self, MAX_PER_ORIGIN, NewCandidate, Origin, OriginKind};
use crate::retrieve::{self, Purpose, Query};
use crate::state::{Knowledge, redact};

/// Model time limit (smart tier, §5.1).
pub const TIMEOUT: Duration = Duration::from_secs(90);
/// Source text given to the model.
pub const MAX_INPUT: usize = 12_000;
/// Names listed in the prompt.
pub const MAX_NAMES: usize = 50;
/// Similar confirmed memories listed in the prompt.
pub const SIMILAR_K: usize = 8;
/// What of the text the similar-memory search uses (the query builder ORs every bigram).
const SIMILAR_QUERY_CHARS: usize = 1000;
/// The first line of every extraction prompt (tests script the gateway on it).
pub const PROMPT_HEAD: &str = "你在帮用户整理以后还会用到的记忆。";
const KINDS: [&str; 4] = ["fact", "preference", "rule", "event"];

/// At most two extraction calls run at a time (§10 `kb.extract`).
static SLOTS: Semaphore = Semaphore::const_new(2);

// ---------------------------------------------------------------- names

/// The objects “关于” may name: contacts, organizations, teams (names and aliases), projects and bots
/// of the workspace that the user can see; “我” is the user's own contact.
#[derive(Debug, Clone, Default)]
pub struct Names {
    /// `(display name, label, ref)`, in prompt order (item people first).
    entries: Vec<(String, &'static str, ObjRef)>,
    /// `(lowercased name or alias, ref)`.
    keys: Vec<(String, ObjRef)>,
}

impl Names {
    /// Loads the names for `viewer`; the parties linked to `item` come first in the prompt list.
    pub fn load(c: &Connection, viewer: &Viewer, item: Option<&str>) -> ApiResult<Self> {
        let mut n = Self::default();
        let mut parties = people_repo::parties(c, viewer, &["contact", "org", "team"], false)?;
        if let Some(item) = item {
            let linked: Vec<String> = c
                .prepare_cached(
                    "SELECT object_id FROM links WHERE workspace_id = ? AND subject_type = 'item' AND subject_id = ? AND object_type = 'party' ORDER BY created",
                )?
                .query_map([&viewer.ws, &item.to_string()], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            parties.sort_by_key(|p| !linked.contains(&p.id));
        }
        for p in &parties {
            let r = ObjRef::new("party", &p.id);
            let label = match p.kind.as_str() {
                "org" => "组织",
                "team" => "团队",
                _ => "联系人",
            };
            if p.user_id.as_deref() == Some(viewer.user.as_str()) {
                n.keys.push(("我".into(), r.clone()));
            }
            n.add(&p.name, label, &r);
            for a in &p.aliases {
                n.keys.push((a.trim().to_lowercase(), r.clone()));
            }
        }
        for (sql, kind, label) in [
            (
                "SELECT id, name FROM projects WHERE workspace_id = ? ORDER BY name, id",
                "project",
                "项目",
            ),
            (
                "SELECT b.id, b.name FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id WHERE w.workspace_id = ? ORDER BY b.name, b.id",
                "bot",
                "Bot",
            ),
        ] {
            let rows: Vec<(String, String)> = c
                .prepare_cached(sql)?
                .query_map([&viewer.ws], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (id, name) in rows {
                n.add(&name, label, &ObjRef::new(kind, &id));
            }
        }
        Ok(n)
    }

    fn add(&mut self, name: &str, label: &'static str, r: &ObjRef) {
        self.entries.push((name.to_string(), label, r.clone()));
        self.keys.push((name.trim().to_lowercase(), r.clone()));
    }

    /// The object `name` names: an id of a listed object, or the single object whose name or alias
    /// equals it (case-insensitive). None or several → `None`: never a guess.
    pub fn resolve(&self, name: &str) -> Option<ObjRef> {
        let w = name.trim();
        if w.is_empty() {
            return None;
        }
        if let Some((_, _, r)) = self.entries.iter().find(|(_, _, r)| r.id == w) {
            return Some(r.clone());
        }
        let wl = w.to_lowercase();
        let mut hits: Vec<&ObjRef> = self
            .keys
            .iter()
            .filter(|(k, _)| *k == wl)
            .map(|(_, r)| r)
            .collect();
        hits.dedup();
        hits.sort();
        hits.dedup();
        match hits.as_slice() {
            [one] => Some((*one).clone()),
            _ => None,
        }
    }

    /// The prompt lines, at most [`MAX_NAMES`]: “- Sarah Chen（联系人；别名 Sarah）”.
    fn prompt_lines(&self) -> String {
        self.entries
            .iter()
            .take(MAX_NAMES)
            .map(|(name, label, _)| format!("- {name}（{label}）"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ---------------------------------------------------------------- prompt and answer

/// A confirmed memory shown to the model, by id.
#[derive(Debug, Clone, PartialEq)]
pub struct Similar {
    pub id: String,
    pub text: String,
}

/// The extraction prompt.
pub fn prompt(label: &str, text: &str, similar: &[Similar], names: &Names) -> String {
    let names = names.prompt_lines();
    let similar = if similar.is_empty() {
        "（没有）".to_string()
    } else {
        similar
            .iter()
            .map(|s| format!("- [{}] {}", s.id, s.text))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "{PROMPT_HEAD}下面是{label}的原文。只抽以后还会用到的事实、偏好、规则（用户说的、用户决定的、对方明确说的），不抽这次任务的过程细节；最多 {MAX_PER_ORIGIN} 条，没有就返回空列表。\n\n\
可识别的名字（about 只能用这些名字，认不出就留空，不要猜）：\n{names}\n\n\
已确认的相近记忆（和某条重复就在 dup_of 填它的 id；和某条冲突、应该替换它就在 conflicts 填它的 id）：\n{similar}\n\n\
原文：\n<<<\n{text}\n>>>\n\n\
只返回 JSON：{{\"items\":[{{\"kind\":\"fact|preference|rule|event\",\"text\":\"…\",\"about\":[\"名字\"],\"level\":\"public|conclusion|reasoning|never\",\"evidence\":\"原话摘录\",\"dup_of\":null,\"conflicts\":null,\"why\":\"…\"}}]}}"
    )
}

/// One item of the model's answer, as given.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Raw {
    pub kind: String,
    pub text: String,
    pub about: Vec<String>,
    pub level: Option<Level>,
    pub evidence: String,
    pub dup_of: Option<String>,
    pub conflicts: Option<String>,
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Parses the answer: the first JSON object in it (code fences and chatter around it are tolerated).
/// Anything unreadable gives no items.
pub fn parse(out: &str) -> Vec<Raw> {
    let (Some(a), Some(b)) = (out.find('{'), out.rfind('}')) else {
        return Vec::new();
    };
    let Some(v) = out
        .get(a..=b)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
    else {
        return Vec::new();
    };
    let Some(items) = v.get("items").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|x| x.is_object())
        .map(|x| Raw {
            kind: opt_str(x.get("kind")).unwrap_or_default(),
            text: opt_str(x.get("text")).unwrap_or_default(),
            about: match x.get("about") {
                Some(Value::Array(a)) => a.iter().filter_map(|n| opt_str(Some(n))).collect(),
                Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
                _ => Vec::new(),
            },
            level: x
                .get("level")
                .and_then(Value::as_str)
                .and_then(Level::parse),
            evidence: opt_str(x.get("evidence")).unwrap_or_default(),
            dup_of: opt_str(x.get("dup_of")),
            conflicts: opt_str(x.get("conflicts")),
        })
        .collect()
}

/// The server's rules over the model's items (§5.1): duplicates dropped; unknown kinds and empty or
/// overlong texts dropped; `about` names resolved exactly, unknown ones dropped — and a candidate whose
/// named objects are all unknown is dropped (it would be about the wrong thing, or nothing); the
/// stricter of the model's and the origin's level; `conflicts` kept only if `valid_conflict` accepts
/// it; at most 5.
pub fn select(
    raw: Vec<Raw>,
    names: &Names,
    default_level: Level,
    valid_conflict: impl Fn(&str) -> bool,
) -> Vec<NewCandidate> {
    let mut out: Vec<NewCandidate> = Vec::new();
    for r in raw {
        if out.len() >= MAX_PER_ORIGIN {
            break;
        }
        if r.dup_of.is_some() || !KINDS.contains(&r.kind.as_str()) {
            continue;
        }
        let n = r.text.chars().count();
        if n == 0 || n > 2000 {
            continue;
        }
        let mut about: Vec<ObjRef> = Vec::new();
        for name in &r.about {
            if let Some(x) = names.resolve(name)
                && !about.contains(&x)
            {
                about.push(x);
            }
        }
        if !r.about.is_empty() && about.is_empty() {
            continue;
        }
        if out.iter().any(|c| c.text == r.text) {
            continue;
        }
        out.push(NewCandidate {
            kind: r.kind,
            text: r.text,
            about,
            level: r.level.unwrap_or(default_level).max(default_level),
            evidence: r.evidence,
            supersedes: r.conflicts.filter(|id| valid_conflict(id)),
        });
    }
    out
}

/// Cuts to `n` characters.
pub fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---------------------------------------------------------------- running

/// Runs one extraction for `origin` over `text` and writes the candidates. `item` puts the item's
/// people first in the name list. Returns the candidates written (possibly none).
pub async fn run(
    m: &Modules,
    kb: &Knowledge,
    origin: &Origin,
    text: &str,
    item: Option<&str>,
) -> ApiResult<Vec<Mem>> {
    let text = redact(kb.guard.as_ref(), &clip(text, MAX_INPUT));
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let now = m.clock.now();
    let (names, similar) = m.store.reader(|c| {
        let viewer = Viewer::load(c, &origin.ws, &origin.user)?;
        let names = Names::load(c, &viewer, item)?;
        let q_text = clip(&text, SIMILAR_QUERY_CHARS);
        let kinds = ["mems"];
        let mut q = Query::new(kb.store(), &viewer, &q_text, Purpose::Agent);
        q.kinds = Some(&kinds);
        q.k = SIMILAR_K;
        q.now = now;
        let similar: Vec<Similar> = retrieve::retrieve(c, &q)?
            .into_iter()
            .filter(|h| h.obj_type == "mem")
            .map(|h| Similar {
                id: h.obj_id,
                text: h.text,
            })
            .collect();
        Ok::<_, ApiError>((names, similar))
    })?;
    let p = prompt(&origin.label, &text, &similar, &names);
    let answer = {
        let _slot = SLOTS
            .acquire()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        steve_modules::gateway::tagged(
            "extract",
            &origin.ws,
            m.gateway.run(Tier::Smart, &p, TIMEOUT),
        )
        .await
    };
    let answer = answer.map_err(|e| {
        tracing::warn!(origin = %origin.id, "kb.extract: gateway failed: {e}");
        ApiError::Internal(format!("gateway: {e}"))
    })?;
    let raw = parse(&answer.text);
    let ws = origin.ws.clone();
    let user = origin.user.clone();
    let chosen = m.store.reader(|c| {
        let viewer = Viewer::load(c, &ws, &user)?;
        Ok::<_, ApiError>(select(raw, &names, origin.default_level, |id| {
            mems::visible(c, &viewer, id)
                .ok()
                .flatten()
                .is_some_and(|x| x.status == "confirmed")
        }))
    })?;
    if chosen.is_empty() {
        return Ok(Vec::new());
    }
    let written = m.store.transaction(|tx| {
        chosen
            .iter()
            .map(|c| {
                let mut c = c.clone();
                c.evidence = redact(kb.guard.as_ref(), &c.evidence);
                candidates::propose(tx, origin, &c)
            })
            .collect::<ApiResult<Vec<_>>>()
    })?;
    tracing::info!(origin = %origin.id, n = written.len(), "kb.extract: candidates");
    Ok(written)
}

/// The entry point for other modules (§5.1 “其他模块”): mail threads (`OriginKind::Mail`, default
/// `reasoning`), channel spaces (`OriginKind::Space`). `user` = the workspace user the text is for;
/// `None` = the workspace owner.
#[allow(clippy::too_many_arguments)]
pub async fn submit(
    m: &Modules,
    kb: &Knowledge,
    ws: &str,
    origin: OriginKind,
    origin_id: &str,
    label: &str,
    text: &str,
    default_level: Level,
    user: Option<&str>,
) -> ApiResult<Vec<Mem>> {
    let user = match user {
        Some(u) => u.to_string(),
        None => m.store.reader(|c| owner(c, ws))?,
    };
    let o = Origin {
        ws: ws.into(),
        kind: origin,
        id: origin_id.into(),
        label: label.into(),
        user,
        writer: "steve".into(),
        default_level,
        item_id: None,
        session_id: None,
    };
    run(m, kb, &o, text, None).await
}

/// The workspace's owner (else its first member): whom workspace-wide origins (main thread chat)
/// write their candidates for.
pub fn owner(c: &Connection, ws: &str) -> ApiResult<String> {
    use rusqlite::OptionalExtension;
    c.prepare_cached(
        "SELECT user_id FROM workspace_members WHERE workspace_id = ? ORDER BY role = 'owner' DESC, created, user_id LIMIT 1",
    )?
    .query_row([ws], |r| r.get(0))
    .optional()?
    .ok_or_else(|| ApiError::not_found("工作空间成员"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        let mut n = Names::default();
        n.add("Sarah Chen", "联系人", &ObjRef::new("party", "ct_1"));
        n.keys.push(("sarah".into(), ObjRef::new("party", "ct_1")));
        n.add("Acme", "组织", &ObjRef::new("party", "org_1"));
        n.add("李雷", "联系人", &ObjRef::new("party", "ct_2"));
        n.add("李雷", "联系人", &ObjRef::new("party", "ct_3"));
        n
    }

    #[test]
    fn resolves_only_exact_and_unique_names() {
        let n = names();
        assert_eq!(n.resolve("sarah chen"), Some(ObjRef::new("party", "ct_1")));
        assert_eq!(n.resolve(" Sarah "), Some(ObjRef::new("party", "ct_1")));
        assert_eq!(n.resolve("org_1"), Some(ObjRef::new("party", "org_1")));
        assert_eq!(n.resolve("Sarah C"), None);
        assert_eq!(n.resolve("李雷"), None, "two people of that name: no guess");
        assert_eq!(n.resolve(""), None);
    }

    #[test]
    fn parses_fenced_answers_and_ignores_garbage() {
        let out = "好的：\n```json\n{\"items\":[{\"kind\":\"fact\",\"text\":\" A \",\"about\":\"Acme\",\"level\":\"reasoning\",\"dup_of\":\"\",\"conflicts\":null}]}\n```";
        let r = parse(out);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].text, "A");
        assert_eq!(r[0].about, vec!["Acme".to_string()]);
        assert_eq!(r[0].level, Some(Level::Reasoning));
        assert_eq!(r[0].dup_of, None);
        assert!(parse("没有").is_empty());
        assert!(parse("{\"items\": 3}").is_empty());
        assert!(parse("{broken").is_empty());
    }

    fn raw(text: &str) -> Raw {
        Raw {
            kind: "fact".into(),
            text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn select_applies_the_server_rules() {
        let n = names();
        let mut items = vec![
            Raw {
                dup_of: Some("mm_1".into()),
                ..raw("重复的")
            },
            Raw {
                about: vec!["王五".into()],
                ..raw("认不出的人")
            },
            Raw {
                about: vec!["Acme".into(), "王五".into()],
                level: Some(Level::Public),
                conflicts: Some("mm_old".into()),
                ..raw("部分认出")
            },
            Raw {
                kind: "summary".into(),
                ..raw("种类不对")
            },
            Raw {
                level: Some(Level::Never),
                conflicts: Some("mm_bogus".into()),
                ..raw("更严的级别")
            },
        ];
        for i in 0..10 {
            items.push(raw(&format!("第 {i} 条")));
        }
        let got = select(items, &n, Level::Conclusion, |id| id == "mm_old");
        assert_eq!(got.len(), MAX_PER_ORIGIN);
        assert_eq!(got[0].text, "部分认出");
        assert_eq!(got[0].about, vec![ObjRef::new("party", "org_1")]);
        assert_eq!(got[0].level, Level::Conclusion, "public < origin default");
        assert_eq!(got[0].supersedes.as_deref(), Some("mm_old"));
        assert_eq!(got[1].text, "更严的级别");
        assert_eq!(got[1].level, Level::Never);
        assert_eq!(got[1].supersedes, None);
        assert!(
            got.iter()
                .all(|c| c.text != "重复的" && c.text != "认不出的人")
        );
    }
}
