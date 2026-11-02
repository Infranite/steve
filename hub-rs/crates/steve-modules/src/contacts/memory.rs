//! Memories about contacts, organizations and teams (slice P3, contacts.md §2.6 and the memory rows of
//! §3.2): `GET/POST /parties/:id/memories`, `PATCH/DELETE /mems/:id`, and the candidate an agent leaves
//! with `steve_contacts_remember`.
//!
//! What you write is `confirmed` at once. What an agent writes is a `candidate` with a `confirm` decision
//! (`subject_type = "mem"`); answering it is the knowledge module's handler (K3, modules/build-plan.md
//! §1 point 1), so the candidate is written the way K3 writes its own: a `mem_candidates` row
//! (`origin = tool`) and at most 20 open memory decisions per workspace.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::Tx;
use steve_store::base::mems::{self, Mem, MemFilter, MemPatch, NewMem};
use steve_store::base::{self, Level, ObjRef, Viewer, Visibility};

use super::people_repo::{self as prepo, NewCandidate};
use super::repo::{PARTY, Party};
use super::service::{check_visibility, own_party, start};
use crate::{ApiError, ApiResult, Caller, Check, Modules, decisions};

/// Kinds a memory about a party may have (§3.2).
const KINDS: [&str; 4] = ["fact", "preference", "rule", "event"];
/// Open memory decisions per workspace before new candidates stop opening one (knowledge.md §5.2).
pub const MAX_OPEN_CONFIRMS: i64 = 20;

/// The `Mem` shape of §3.1 (also what the knowledge module's `Candidate.mem` embeds).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemOut {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub source_label: String,
    pub level: Level,
    pub status: String,
    pub visibility: Visibility,
    pub about: Vec<ObjRef>,
    pub created: i64,
}

impl From<&Mem> for MemOut {
    fn from(m: &Mem) -> Self {
        Self {
            id: m.id.clone(),
            kind: m.kind.clone(),
            text: m.text.clone(),
            source_label: m.source_label.clone(),
            level: m.level,
            status: m.status.clone(),
            visibility: m.visibility.clone(),
            about: m.about.clone(),
            created: m.created,
        }
    }
}

/// Any active contact, organization or team the viewer owns; `me` = “我”. `404 没有这个联系人`.
fn own_any(c: &rusqlite::Connection, viewer: &Viewer, id: &str, me: &Party) -> ApiResult<Party> {
    let id = if id == "me" { me.id.as_str() } else { id };
    let kind = super::repo::get(c, id)?.map(|p| p.kind).unwrap_or_default();
    let (kind, noun) = match kind.as_str() {
        "org" => ("org", "组织"),
        "team" => ("team", "团队"),
        _ => ("contact", "联系人"),
    };
    own_party(c, viewer, id, kind, noun, false)
}

/// `GET /parties/:id/memories`: every memory about the party the viewer can see, any status, `never`
/// included (this is the owner's own page; prompts and MCP never get them), newest first.
pub fn list(m: &Modules, who: &Caller, id: &str) -> ApiResult<Vec<MemOut>> {
    let (viewer, me) = start(m, who)?;
    m.store.reader(|c| {
        let p = own_any(c, &viewer, id, &me)?;
        let f = MemFilter {
            include_never: true,
            ..Default::default()
        };
        Ok(mems::about(c, &viewer, &ObjRef::new(PARTY, &p.id), &f)?
            .iter()
            .map(MemOut::from)
            .collect())
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewMemBody {
    pub text: String,
    pub kind: Option<String>,
    pub level: Option<Level>,
}

/// `POST /parties/:id/memories`: what you say is `confirmed`, `source_type = me`, private by default.
pub fn create(m: &Modules, who: &Caller, id: &str, b: NewMemBody) -> ApiResult<MemOut> {
    let (viewer, me) = start(m, who)?;
    let text = b.text.trim().to_string();
    let mut ck = Check::new();
    ck.len("text", &text, 1, 2000);
    if let Some(k) = &b.kind {
        ck.one_of("kind", k, &KINDS);
    }
    ck.done()?;
    let mem = m.store.transaction(|tx| {
        let p = own_any(tx, &viewer, id, &me)?;
        let mem = mems::create(
            tx,
            &who.ws,
            &NewMem {
                kind: b.kind.clone().unwrap_or_else(|| "fact".into()),
                text: text.clone(),
                source_type: "me".into(),
                source_label: "你写的".into(),
                level: b.level.unwrap_or(Level::Conclusion),
                writer: "me".into(),
                created_by: who.me.clone(),
                about: vec![ObjRef::new(PARTY, &p.id)],
                ..Default::default()
            },
        )?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "mm.create",
            &mem.id,
            &json!({ "about": p.id, "kind": mem.kind }),
        )?;
        Ok::<_, ApiError>(mem)
    })?;
    Ok(MemOut::from(&mem))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MemPatchBody {
    pub text: Option<String>,
    pub level: Option<Level>,
    pub status: Option<String>,
    pub visibility: Option<Visibility>,
}

fn own_mem(tx: &rusqlite::Connection, viewer: &Viewer, id: &str) -> ApiResult<Mem> {
    mems::visible(tx, viewer, id)?.ok_or_else(|| ApiError::not_found("记忆"))
}

/// `PATCH /mems/:id`.
pub fn patch(m: &Modules, who: &Caller, id: &str, b: MemPatchBody) -> ApiResult<MemOut> {
    let (viewer, _) = start(m, who)?;
    let text = b.text.as_deref().map(str::trim).map(String::from);
    let mut ck = Check::new();
    ck.opt_len("text", text.as_deref(), 1, 2000);
    if let Some(s) = &b.status {
        ck.one_of("status", s, &["confirmed", "stale"]);
    }
    ck.done()?;
    let mem = m.store.transaction(|tx| {
        let old = own_mem(tx, &viewer, id)?;
        if let Some(v) = &b.visibility {
            check_visibility(tx, &who.ws, v)?;
        }
        let p = MemPatch {
            text,
            level: b.level,
            status: b.status.clone(),
            visibility: b.visibility.clone(),
            ..Default::default()
        };
        let mem = mems::patch(tx, &old.id, &p)?.ok_or_else(|| ApiError::not_found("记忆"))?;
        // confirming or retiring a candidate by hand answers its decision the same way
        if old.status == "candidate" && mem.status != "candidate" {
            let choice = if mem.status == "confirmed" {
                "yes"
            } else {
                "no"
            };
            settle_decision(
                tx,
                &old,
                json!({ "choice": choice, "by": "edit" }),
                "resolved",
            )?;
            prepo::candidate_delete(tx, &old.id)?;
        }
        let mut changed: Vec<&str> = Vec::new();
        for (k, on) in [
            ("text", p.text.is_some()),
            ("level", p.level.is_some()),
            ("status", p.status.is_some()),
            ("visibility", p.visibility.is_some()),
        ] {
            if on {
                changed.push(k);
            }
        }
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "mm.patch",
            &mem.id,
            &json!({ "fields": changed }),
        )?;
        Ok::<_, ApiError>(mem)
    })?;
    Ok(MemOut::from(&mem))
}

/// Settles the memory's open decision, if it has one.
fn settle_decision(tx: &Tx, mem: &Mem, result: Value, status: &str) -> ApiResult<()> {
    if let Some(d) = &mem.decision_id {
        base::decisions::settle(tx, d, &result, status, &Value::Null)?;
    }
    Ok(())
}

/// Deletes a memory with what hangs off it: its candidate row and its open decision (expired, so the
/// card leaves the inbox). Returns whether it existed.
pub fn delete_in(tx: &Tx, id: &str) -> ApiResult<bool> {
    let Some(old) = mems::get(tx, id)? else {
        return Ok(false);
    };
    settle_decision(tx, &old, json!({ "deleted": true }), "expired")?;
    prepo::candidate_delete(tx, id)?;
    Ok(mems::delete(tx, id)?)
}

/// The memories about one party and nothing else, deleted physically (deleting a contact, §2.2, §8).
/// Returns how many.
pub fn delete_only_about(tx: &Tx, ws: &str, party_id: &str) -> ApiResult<usize> {
    let ids = prepo::mems_only_about(tx, ws, PARTY, party_id)?;
    for id in &ids {
        delete_in(tx, id)?;
    }
    Ok(ids.len())
}

/// `DELETE /mems/:id`.
pub fn delete(m: &Modules, who: &Caller, id: &str) -> ApiResult<()> {
    let (viewer, _) = start(m, who)?;
    m.store.transaction(|tx| {
        let old = own_mem(tx, &viewer, id)?;
        delete_in(tx, &old.id)?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "mm.delete",
            &old.id,
            &json!({ "kind": old.kind }),
        )?;
        Ok(())
    })
}

// ---------------------------------------------------------------- candidates from agents

/// A memory an agent proposes about a party (`steve_contacts_remember`).
#[derive(Debug, Clone)]
pub struct Proposal {
    pub party_id: String,
    /// `fact` | `preference` | `event`
    pub kind: String,
    pub text: String,
    /// The bot that wrote it (`writer`, `requested_by`).
    pub bot_id: String,
    /// Where it came from: the agent's item, if any (`source_type = item`, `mem_candidates.origin_id`).
    pub item_id: Option<String>,
    /// The session, when there is no item.
    pub session_id: Option<String>,
    /// “ST-12 的会话” — shown on the card and the knowledge “待确认” page.
    pub origin_label: String,
}

/// Writes `p` as a private `candidate` of `viewer.user` plus its `confirm` decision (`subject_type =
/// "mem"`, question `记住这条吗？<text>`), unless 20 memory decisions are already open; then it waits on
/// the knowledge page without a decision. Returns the memory.
pub fn propose(tx: &Tx, viewer: &Viewer, p: &Proposal) -> ApiResult<Mem> {
    let ws = viewer.ws.as_str();
    let (source_type, source_id) = match &p.item_id {
        Some(i) => ("item", Some(i.clone())),
        None => ("steve", p.session_id.clone()),
    };
    let mem = mems::create(
        tx,
        ws,
        &NewMem {
            kind: p.kind.clone(),
            text: p.text.clone(),
            source_type: source_type.into(),
            source_id,
            source_label: format!("{} · Agent 记下", p.origin_label),
            visibility: Visibility::Private,
            level: Level::Conclusion,
            status: "candidate".into(),
            writer: p.bot_id.clone(),
            created_by: viewer.user.clone(),
            about: vec![ObjRef::new(PARTY, &p.party_id)],
            ..Default::default()
        },
    )?;
    let origin_id = p
        .item_id
        .clone()
        .or_else(|| p.session_id.clone())
        .unwrap_or_default();
    prepo::candidate_insert(
        tx,
        ws,
        &NewCandidate {
            mem_id: &mem.id,
            origin: "tool",
            origin_id: &origin_id,
            origin_label: &p.origin_label,
            evidence: "",
        },
    )?;
    let mut mem = mem;
    if prepo::open_mem_confirms(tx, ws)? < MAX_OPEN_CONFIRMS {
        let mut nd = decisions::confirm(
            ws,
            "mem",
            &mem.id,
            &format!("记住这条吗？{}", p.text),
            &p.bot_id,
        );
        nd.draft = Some(format!("来源：{}", p.origin_label));
        nd.session_id = p.session_id.clone();
        let d = base::decisions::open(tx, &nd)?;
        mem = mems::patch(
            tx,
            &mem.id,
            &MemPatch {
                decision_id: Some(Some(d.id)),
                ..Default::default()
            },
        )?
        .ok_or_else(|| ApiError::not_found("记忆"))?;
    }
    base::audit(
        tx,
        ws,
        &p.bot_id,
        "mm.propose",
        &mem.id,
        &json!({ "about": p.party_id, "kind": p.kind }),
    )?;
    Ok(mem)
}
