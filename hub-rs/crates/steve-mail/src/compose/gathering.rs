//! Context assembled inside the existing gather reader, in its original order.
use super::*;

pub(super) struct Personal {
    pub anchors: Vec<ObjRef>,
    pub cards_text: String,
    pub items: Vec<(String, String)>,
}
pub(super) fn personal(
    c: &rusqlite::Connection,
    viewer: &steve_store::base::Viewer,
    who: &Caller,
    orig: &MessageRow,
    q: &QuickDraft,
) -> MailResult<Personal> {
    // layer 6: the sender's card, their open items, rules about them
    let mut anchors: Vec<ObjRef> = Vec::new();
    let mut cards_text =
        steve_modules::contacts::profile::address(c, &who.ws, &who.me, true, None, None)?
            .map(|name| format!("向收件人介绍用户时使用：{name}。不要使用内部昵称。"))
            .unwrap_or_default();
    let mut items = Vec::new();
    if let Some(p) = orig
        .from_party_id
        .as_ref()
        .filter(|_| orig.direction == "in")
    {
        let cs = cards::contact_cards(c, &viewer, std::slice::from_ref(p))?;
        cards_text.push_str(&format!("\n{}", cards::render(&cs)));
        anchors.push(ObjRef::new("party", p));
        let links = people_repo::item_links_to(c, &who.ws, std::slice::from_ref(p), true)?;
        let mut ids: Vec<String> = Vec::new();
        for l in links {
            if !ids.contains(&l.item_id) {
                ids.push(l.item_id);
            }
        }
        items = repo::open_items(c, &ids)?
            .into_iter()
            .take(5)
            .map(|(_, t, s)| (t, s))
            .collect();
    }
    if let Some(i) = &q.item_id {
        anchors.push(ObjRef::new("item", i));
    }
    Ok(Personal {
        anchors,
        cards_text,
        items,
    })
}

pub(super) struct Related {
    pub hits: Vec<Hit>,
    pub query: String,
    pub semantic_down: bool,
}
pub(super) fn related(
    c: &mut steve_store::R<'_>,
    viewer: &steve_store::base::Viewer,
    store: &dyn steve_knowledge::port::KnowledgeStore,
    orig: &MessageRow,
    anchors: &[ObjRef],
    now: i64,
) -> MailResult<Related> {
    let mut hits = anchor_mems(c, viewer, anchors, ANCHOR_MEMS, now)?;
    // layer 7: retrieve(subject + new text, k=8, Draft)
    let query = steve_knowledge::context::query_text(&format!(
        "{} {}",
        orig.subject,
        orig.body_new.as_deref().unwrap_or(&orig.snippet)
    ));
    let mut rq = Query::new(store, &viewer, &query, Purpose::Draft);
    rq.anchors = &anchors;
    rq.k = DRAFT_K;
    rq.now = now;
    let found = retrieve_all(c, &rq)?;
    let semantic_down = found.semantic_down;
    for h in found.hits {
        if !hits
            .iter()
            .any(|x| x.obj_type == h.obj_type && x.obj_id == h.obj_id && x.seq == h.seq)
        {
            hits.push(h);
        }
    }
    for (i, h) in hits.iter_mut().enumerate() {
        h.n = i + 1;
    }

    Ok(Related {
        hits,
        query,
        semantic_down,
    })
}

pub(super) fn thread(c: &rusqlite::Connection, thread_id: &str) -> MailResult<String> {
    // the thread: the last five messages' new text, ≤12000 characters
    let msgs = repo::thread_messages(c, thread_id)?;
    let start = msgs.len().saturating_sub(CONTEXT_MESSAGES);
    let mut blocks: Vec<String> = Vec::new();
    let mut left = CONTEXT_MAX;
    for m in msgs[start..].iter().rev() {
        if left == 0 {
            break;
        }
        let text = m
            .body_new
            .clone()
            .unwrap_or_else(|| format!("（正文已过保留期，摘要：{}）", m.snippet));
        let text = cut_chars(&text, left);
        left = left.saturating_sub(text.chars().count());
        blocks.push(format!("{}\n\n{}", line_of(m), text));
    }
    blocks.reverse();
    Ok(blocks.join("\n\n---\n\n"))
}
