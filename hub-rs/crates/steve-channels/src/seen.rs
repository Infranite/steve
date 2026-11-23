//! Where contacts were seen in the channels ([`steve_modules::contacts::SpacePort::seen`]), so 需要你
//! 判断 can tell an unknown person apart without showing an id: the spaces they were in (as a
//! member, a sender or someone @-mentioned), their last
//! message, and whether every one of those spaces is one you muted (不提醒 / 屏蔽 — then they don't
//! need your judgement at all).

use std::collections::{HashMap, HashSet};

use steve_modules::contacts::{Seen, SeenMessage, SeenSpace};
use steve_store::channel_spaces::activity;
use steve_store::{Accounts, Read, Result};

use crate::spaces::Attention;

/// Spaces listed per contact at most.
const MAX_SPACES: usize = 5;
/// Characters of the last message shown.
const SNIPPET: usize = 80;

fn snippet(t: &str) -> String {
    let one: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > SNIPPET {
        let mut s: String = one.chars().take(SNIPPET).collect();
        s.push('…');
        s
    } else {
        one
    }
}

/// For each contact that was seen in a space of this workspace.
pub fn seen(r: &mut impl Read, ws: &str, parties: &[String]) -> Result<Vec<(String, Seen)>> {
    if parties.is_empty() {
        return Ok(Vec::new());
    }
    let accounts: HashMap<_, _> = Accounts(r)
        .owners_of_accounts(ws, parties)?
        .into_iter()
        .collect();
    let account_ids = accounts.keys().cloned().collect::<Vec<_>>();
    let mut appearances: HashMap<String, HashMap<String, i64>> = HashMap::new();
    let mut record = |party: String, space: String, at: i64| {
        appearances
            .entry(party)
            .or_default()
            .entry(space)
            .and_modify(|v| *v = (*v).max(at))
            .or_insert(at);
    };
    for row in activity::sent_spaces(r, ws, parties)? {
        record(row.subject, row.space, row.at);
    }
    if !account_ids.is_empty() {
        for row in activity::member_spaces(r, ws, &account_ids)?
            .into_iter()
            .chain(activity::mentioned_spaces(r, ws, &account_ids)?)
        {
            if let Some(party) = accounts.get(&row.subject) {
                record(party.clone(), row.space, row.at);
            }
        }
    }
    let ids = appearances
        .values()
        .flat_map(HashMap::keys)
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let spaces: HashMap<_, _> = activity::spaces(r, ws, &ids)?
        .into_iter()
        .map(|space| (space.id.clone(), space))
        .collect();
    let counts: HashMap<_, _> = activity::message_counts(r, ws, parties)?
        .into_iter()
        .collect();
    let mut out = Vec::new();
    for party in parties {
        let rows = appearances.get(party).into_iter().flatten();
        let mut rows: Vec<_> = rows
            .filter_map(|(id, at)| spaces.get(id).map(|s| (s, *at)))
            .collect();
        rows.sort_by(|(a, at), (b, bt)| bt.cmp(at).then_with(|| a.id.cmp(&b.id)));
        let Some((_, last_at)) = rows.first() else {
            continue;
        };
        let attention = |s: &activity::Space| {
            s.attention
                .as_deref()
                .and_then(Attention::parse)
                .unwrap_or(Attention::Realtime)
        };
        let quiet = rows
            .iter()
            .all(|(s, _)| attention(s) != Attention::Realtime);
        let spaces = rows
            .iter()
            .take(MAX_SPACES)
            .map(|(s, _)| SeenSpace {
                id: s.id.clone(),
                name: s.name.clone(),
                kind: s.kind.clone(),
                attention: attention(s).code().into(),
            })
            .collect();
        let last = activity::last_message(r, ws, party)?.map(|m| SeenMessage {
            text: snippet(&m.text),
            created: m.created,
            space_id: m.space_id,
            space_name: m.space_name,
        });
        out.push((
            party.clone(),
            Seen {
                last_at: Some(*last_at),
                spaces,
                last,
                messages: counts.get(party).copied().unwrap_or(0),
                quiet,
            },
        ));
    }
    Ok(out)
}
