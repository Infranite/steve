//! What a window says for certain, and its text with position numbers.
//!
//! Positions: the window's messages are `[1]`…`[n]`; the messages it carries from before for context are
//! `[-k]`…`[-1]` (read only, never written back). Every fact names Steve's own ids: the conversation, each
//! message, each party (contact), each reply chain or topic, each referenced item. The digest Bot only ever
//! writes positions and ids it read here or in a tool's answer; the Hub checks them.

use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use steve_store::StoreError;

use crate::conv::{self, ConvInfo, Msg};
use crate::normalize::{self, Ref};
use crate::profile::Profile;

type Result<T> = std::result::Result<T, StoreError>;

/// How much of a quoted message outside the window is shown.
const QUOTE_CHARS: usize = 60;

/// One position of a window: its rendered text and what chunking needs.
#[derive(Debug, Clone, Serialize)]
pub struct Line {
    pub pos: i64,
    pub id: String,
    pub at: i64,
    /// The reply chain or topic it belongs to.
    pub chain: Option<String>,
    pub text: String,
    /// The message's own words (no header, no placeholders).
    #[serde(skip)]
    pub body: String,
    pub tokens: i64,
}

/// A party of a window.
#[derive(Debug, Clone, Serialize)]
pub struct Party {
    pub id: String,
    pub name: String,
    pub bot: bool,
    pub mine: bool,
    pub messages: i64,
    pub mentioned: i64,
    pub last_at: i64,
}

/// A window, built.
#[derive(Debug, Clone)]
pub struct Built {
    pub facts: Value,
    pub lines: Vec<Line>,
    pub refs: Vec<Ref>,
    pub parties: Vec<Party>,
    /// chain id → positions in the window
    pub chains: BTreeMap<String, Vec<i64>>,
    /// Items the window names (checked to exist).
    pub items: Vec<String>,
    pub mentions_me: bool,
    pub my_messages: i64,
}

/// Task IDs already are `<workspace prefix>-<number>`; validate the exact stored id.
fn item_title(c: &Connection, ws: &str, id: &str) -> Result<Option<(String, String)>> {
    Ok(
        c.prepare_cached("SELECT id, title FROM items WHERE workspace_id = ? AND id = ?")?
            .query_row(params![ws, id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?,
    )
}

fn display_name(m: &Msg) -> String {
    let n = if m.sender.name.trim().is_empty() {
        "?".to_string()
    } else {
        m.sender.name.trim().to_string()
    };
    if m.sender.mine {
        format!("我（{n}）")
    } else if m.sender.bot {
        format!("{n}〔机器人〕")
    } else {
        n
    }
}

/// Follow stored reply ids to a stable root even when the chain crosses windows.
fn reply_chain(c: &Connection, ws: &str, conv: &conv::ConvRef, id: &str) -> Result<String> {
    let mut root = id.to_string();
    let mut seen = std::collections::HashSet::new();
    while seen.insert(root.clone()) {
        let Some(m) = conv::by_ids(c, ws, conv, &[root.clone()])?.pop() else {
            break;
        };
        if let Some(topic) = m.thread {
            return Ok(format!("t:{topic}"));
        }
        match m.reply_to {
            Some(parent) => root = parent,
            None => break,
        }
    }
    Ok(format!("r:{root}"))
}

type Positioned<'a> = Vec<(i64, &'a Msg)>;
type Forward = (String, String, Option<i64>, String);

fn chain_roots<'a>(
    c: &Connection,
    ws: &str,
    conv: &conv::ConvRef,
    all: &[(i64, &'a Msg)],
) -> Result<HashMap<&'a str, String>> {
    // reply chains and topics: a topic key, else the chain of the message replied to
    let mut chain_of: HashMap<&str, String> = HashMap::new();
    for (_, m) in all {
        let chain = if let Some(t) = &m.thread {
            Some(format!("t:{t}"))
        } else if let Some(r) = &m.reply_to {
            Some(match chain_of.get(r.as_str()) {
                Some(ch) => ch.clone(),
                None => reply_chain(c, ws, conv, r)?,
            })
        } else {
            None
        };
        if let Some(ch) = chain {
            // the message replied to starts the chain too
            if let Some(r) = &m.reply_to {
                chain_of.entry(r.as_str()).or_insert_with(|| ch.clone());
            }
            chain_of.insert(m.id.as_str(), ch);
        }
    }
    Ok(chain_of)
}

fn chain_positions(
    all: &[(i64, &Msg)],
    chain_of: &HashMap<&str, String>,
) -> BTreeMap<String, Vec<i64>> {
    // Keep singleton chains: a later window can continue the same deterministic topic.
    let mut chains: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for (p, m) in all {
        if let Some(ch) = chain_of.get(m.id.as_str()) {
            chains.entry(ch.clone()).or_default().push(*p);
        }
    }

    chains
}

fn quoted_messages(
    c: &Connection,
    ws: &str,
    conv: &conv::ConvRef,
    all: &[(i64, &Msg)],
    pos_of: &HashMap<&str, i64>,
) -> Result<HashMap<String, Msg>> {
    // quoted messages outside the window
    let outside: Vec<String> = all
        .iter()
        .filter_map(|(_, m)| m.reply_to.clone())
        .filter(|r| !pos_of.contains_key(r.as_str()))
        .collect();
    let quoted: HashMap<String, Msg> = conv::by_ids(c, ws, conv, &outside)?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();

    Ok(quoted)
}

/// Reference numbers follow attachment, forwarded-message, URL and stored-item order.
fn collect_references(
    c: &Connection,
    ws: &str,
    p: i64,
    m: &Msg,
    refs: &mut Vec<Ref>,
    items: &mut Vec<String>,
) -> Result<(Vec<usize>, Vec<Forward>)> {
    let mut my_refs = Vec::new();
    let push_ref = |refs: &mut Vec<Ref>, kind: String, title: String, at: Value| -> usize {
        let n = refs.len() + 1;
        refs.push(Ref {
            n,
            kind,
            title,
            pos: p,
            message: m.id.clone(),
            at,
        });
        n
    };
    for a in &m.attachments {
        if let Some((kind, title, at)) = normalize::attachment_ref(a) {
            my_refs.push(push_ref(refs, kind, title, at));
        }
    }
    let mut forwards = Vec::new();
    for a in &m.attachments {
        let f = normalize::forwarded(a);
        if !f.is_empty() {
            let ids: Vec<&str> = f.iter().map(|x| x.0.as_str()).collect();
            let n = push_ref(
                refs,
                "forward".into(),
                format!("{} 条", f.len()),
                json!({"forwarded": ids}),
            );
            my_refs.push(n);
            forwards.extend(f);
        }
    }
    for u in normalize::urls(&m.text) {
        let (kind, at) = normalize::classify_url(&u);
        my_refs.push(push_ref(refs, kind, String::new(), at));
    }
    for no in normalize::item_numbers(&m.text) {
        if let Some((id, title)) = item_title(c, ws, &no)? {
            my_refs.push(push_ref(
                refs,
                "item".into(),
                title,
                json!({"item": id, "number": no}),
            ));
            if p > 0 && !items.contains(&id) {
                items.push(id);
            }
        }
    }

    Ok((my_refs, forwards))
}

struct Rendering<'a, 'm> {
    info: &'a ConvInfo,
    zone: steve_modules::zone::Zone,
    pos_of: &'a HashMap<&'m str, i64>,
    chain_of: &'a HashMap<&'m str, String>,
    quoted: &'a HashMap<String, Msg>,
}

impl Rendering<'_, '_> {
    fn short_time(&self, ms: i64) -> String {
        self.zone.format(ms, "%m-%d %H:%M")
    }

    fn head(&self, p: i64, m: &Msg) -> String {
        let mut head = format!("[{p}] {} {}", self.short_time(m.at), display_name(m));
        match &m.reply_to {
            Some(r) if self.pos_of.contains_key(r.as_str()) => {
                head.push_str(&format!("（回复 [{}]）", self.pos_of[r.as_str()]))
            }
            Some(r) => match self.quoted.get(r) {
                Some(q) => head.push_str(&format!(
                    "（回复 上文 {} {}：「{}」）",
                    q.id,
                    display_name(q),
                    crate::cut(&normalize::tidy(&q.text).replace('\n', " "), QUOTE_CHARS)
                )),
                None => head.push_str(&format!("（回复 {r}）")),
            },
            None => {}
        }
        if self.info.conv.kind == "mail_thread" && !m.mentions.is_empty() {
            let to: Vec<String> = m.mentions.iter().map(|x| x.name.clone()).collect();
            head.push_str(&format!(" → {}", to.join("、")));
        }
        head
    }

    fn line(
        &self,
        p: i64,
        m: &Msg,
        my_refs: &[usize],
        refs: &[Ref],
        forwards: &[Forward],
        subject: &mut Option<String>,
    ) -> Line {
        let head = self.head(p, m);
        let mut text = String::new();
        if let Some(sj) = &m.subject
            && subject.as_ref() != Some(sj)
        {
            text.push_str(&format!("主题：{sj}\n"));
            *subject = Some(sj.clone());
        }
        text.push_str(&normalize::tidy(&m.text));
        let shown: Vec<String> = my_refs
            .iter()
            .filter_map(|n| refs.get(n - 1))
            .filter(|r| matches!(r.kind.as_str(), "image" | "file" | "card" | "forward"))
            .map(normalize::placeholder)
            .collect();
        let mut line = format!("{head}：{text}");
        if !shown.is_empty() {
            line.push_str(&format!("\n    {}", shown.join(" ")));
        }
        for (id, sender, at, t) in forwards {
            line.push_str(&format!(
                "\n    ↳ 转发 {}{}（原消息 {}）：{}",
                if sender.is_empty() { "?" } else { sender },
                at.map(|a| format!(" {}", self.short_time(a)))
                    .unwrap_or_default(),
                if id.is_empty() { "-" } else { id },
                crate::cut(&normalize::tidy(t).replace('\n', " "), 400)
            ));
        }
        Line {
            pos: p,
            id: m.id.clone(),
            at: m.at,
            chain: self.chain_of.get(m.id.as_str()).cloned(),
            tokens: normalize::tokens(&line) + 2,
            body: normalize::tidy(&m.text),
            text: line,
        }
    }
}

#[derive(Default)]
struct Participation {
    parties: BTreeMap<String, Party>,
    mentions_me: bool,
    my_messages: i64,
}

impl Participation {
    /// Context lines do not call this: only positive positions affect participation.
    fn observe(&mut self, m: &Msg, me: &Option<String>) {
        if m.sender.mine {
            self.my_messages += 1;
        }
        if m.mentions
            .iter()
            .any(|x| x.mine || (x.party.is_some() && x.party == *me))
        {
            self.mentions_me = true;
        }
        if let Some(pid) = &m.sender.party {
            let e = self.parties.entry(pid.clone()).or_insert_with(|| Party {
                id: pid.clone(),
                name: m.sender.name.clone(),
                bot: m.sender.bot,
                mine: m.sender.mine,
                messages: 0,
                mentioned: 0,
                last_at: 0,
            });
            e.messages += 1;
            e.last_at = e.last_at.max(m.at);
        }
        for x in &m.mentions {
            if let Some(pid) = &x.party {
                let e = self.parties.entry(pid.clone()).or_insert_with(|| Party {
                    id: pid.clone(),
                    name: x.name.clone(),
                    bot: x.bot,
                    mine: x.mine,
                    messages: 0,
                    mentioned: 0,
                    last_at: 0,
                });
                e.mentioned += 1;
                e.last_at = e.last_at.max(m.at);
            }
        }
    }
}

fn message_fact(
    p: i64,
    m: &Msg,
    my_refs: &[usize],
    pos_of: &HashMap<&str, i64>,
    chain_of: &HashMap<&str, String>,
    chains: &BTreeMap<String, Vec<i64>>,
) -> Value {
    json!({
        "pos": p, "id": m.id, "ext_id": m.ext_id, "at": m.at,
        "sender": m.sender.party, "name": m.sender.name, "bot": m.sender.bot, "mine": m.sender.mine,
        "mentions": m.mentions.iter().filter_map(|x| x.party.clone()).collect::<Vec<_>>(),
        "reply_to": m.reply_to.as_ref().map(|r| pos_of.get(r.as_str()).map(|p| json!(p)).unwrap_or_else(|| json!(r))),
        "chain": chain_of.get(m.id.as_str()).filter(|c| chains.contains_key(*c)),
        "refs": my_refs,
    })
}

/// Builds the facts and lines of a window: `context` (from before, may be empty) then `msgs`.
#[allow(clippy::too_many_arguments)]
pub fn build(
    c: &Connection,
    ws: &str,
    info: &ConvInfo,
    profile: &Profile,
    seq: i64,
    trigger: &str,
    context: &[Msg],
    msgs: &[Msg],
) -> Result<Built> {
    let all: Positioned<'_> = context
        .iter()
        .enumerate()
        .map(|(i, m)| (i as i64 - context.len() as i64, m))
        .chain(msgs.iter().enumerate().map(|(i, m)| (i as i64 + 1, m)))
        .collect();
    let pos_of: HashMap<&str, i64> = all.iter().map(|(p, m)| (m.id.as_str(), *p)).collect();
    let me = conv::my_party(c, ws)?;
    // times on the clock of the person the digest is for
    let zone = steve_modules::zone::Zone::of_owner(c, ws)?;

    let chain_of = chain_roots(c, ws, &info.conv, &all)?;
    let chains = chain_positions(&all, &chain_of);
    let quoted = quoted_messages(c, ws, &info.conv, &all, &pos_of)?;
    let rendering = Rendering {
        info,
        zone,
        pos_of: &pos_of,
        chain_of: &chain_of,
        quoted: &quoted,
    };
    let mut refs = Vec::new();
    let mut items = Vec::new();
    let mut participation = Participation::default();
    let mut lines = Vec::new();
    let mut facts_msgs = Vec::new();
    let mut subject = None;
    for (p, m) in &all {
        let (my_refs, forwards) = collect_references(c, ws, *p, m, &mut refs, &mut items)?;
        lines.push(rendering.line(*p, m, &my_refs, &refs, &forwards, &mut subject));
        if *p <= 0 {
            continue;
        }
        participation.observe(m, &me);
        facts_msgs.push(message_fact(*p, m, &my_refs, &pos_of, &chain_of, &chains));
    }
    let Participation {
        parties,
        mentions_me,
        my_messages,
    } = participation;
    let wrote: Vec<i64> = all
        .iter()
        .filter(|(p, m)| *p > 0 && m.sender.mine)
        .map(|(p, _)| *p)
        .collect();
    let mentioned: Vec<i64> = all
        .iter()
        .filter(|(p, m)| *p > 0 && m.mentions.iter().any(|x| x.mine))
        .map(|(p, _)| *p)
        .collect();
    let (first, last) = (msgs.first(), msgs.last());
    let parties: Vec<Party> = parties.into_values().collect();
    let facts = json!({
        "conv": {
            "key": info.conv.key(), "type": info.conv.kind, "id": info.conv.id, "name": info.name,
            "source_kind": info.source_kind, "adapter": info.adapter, "members": info.members,
            "has_external": info.has_external, "kind": profile.kind, "kind_source": profile.kind_source,
            "role": info.role, "depth": profile.settings.depth,
        },
        "window": {
            "seq": seq, "trigger": trigger, "count": msgs.len(), "context": context.len(),
            "first_id": first.map(|m| m.id.clone()), "last_id": last.map(|m| m.id.clone()),
            "first_at": first.map(|m| m.at), "last_at": last.map(|m| m.at),
        },
        "messages": facts_msgs,
        "parties": parties,
        "chains": chains.iter().map(|(k, v)| json!({"id": k, "positions": v})).collect::<Vec<_>>(),
        "me": { "party": me, "participated": !wrote.is_empty(), "wrote": wrote, "mentioned": mentioned },
        "refs": refs,
        "items": items,
    });
    Ok(Built {
        facts,
        lines,
        refs,
        parties,
        chains,
        items,
        mentions_me,
        my_messages,
    })
}
