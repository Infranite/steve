//! The same meeting seen in several calendars of one person: which occurrences are one meeting, and
//! which of them is shown. Deterministic rules, no model:
//!
//! 1. Only occurrences in calendars of the same person, never two of the same calendar.
//! 2. The same `ical_uid` and original start (or start) are one meeting.
//! 3. Otherwise the same organizer, a start within a minute and the same normalized title.
//! 4. Two occurrences the owner said are not the same are never joined directly.
//!
//! The shown occurrence: one the owner may change, then one with a meeting link, then one with
//! details over a busy-only one, then the smallest id. A group keeps the meeting id its members had
//! (the shown one's first), so what hangs on a meeting stays when an occurrence joins or leaves;
//! a new group gets one from its occurrences' ids.

use std::collections::{BTreeMap, HashSet};

/// What matching needs of an occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub calendar: String,
    /// The person whose calendar it is.
    pub party: String,
    pub ical_uid: Option<String>,
    pub original_start: Option<String>,
    /// A contact id, else a lowercased email.
    pub organizer: Option<String>,
    pub start_ms: i64,
    pub title: Option<String>,
    pub can_modify: bool,
    pub has_link: bool,
    pub busy_only: bool,
    /// The meeting it belonged to so far.
    pub meeting: Option<String>,
}

/// One meeting: its id, the shown occurrence, the key it was matched by and its occurrences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meeting {
    pub id: String,
    pub primary: String,
    pub key: String,
    pub events: Vec<String>,
}

/// Lowercase, punctuation and spacing dropped: "Weekly sync!" and "weekly  SYNC" are one title.
pub fn norm_title(t: &str) -> String {
    t.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn find(p: &mut [usize], i: usize) -> usize {
    let mut r = i;
    while p[r] != r {
        r = p[r];
    }
    let mut i = i;
    while p[i] != r {
        let n = p[i];
        p[i] = r;
        i = n;
    }
    r
}

/// Groups `items` into meetings. `not_same` holds pairs of event ids (either order).
pub fn meetings(items: &[Item], not_same: &HashSet<(String, String)>) -> Vec<Meeting> {
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let apart = |a: &Item, b: &Item| {
        a.calendar == b.calendar
            || a.party != b.party
            || not_same.contains(&(a.id.clone(), b.id.clone()))
            || not_same.contains(&(b.id.clone(), a.id.clone()))
    };
    let join = |p: &mut Vec<usize>, a: usize, b: usize| {
        if !apart(&items[a], &items[b]) {
            let (ra, rb) = (find(p, a), find(p, b));
            if ra != rb {
                p[ra.max(rb)] = ra.min(rb);
            }
        }
    };
    join_uid_matches(items, &mut parent, &join);
    join_organizer_matches(items, &mut parent, &join);
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut taken: HashSet<String> = HashSet::new();
    let mut out: Vec<Meeting> = groups
        .into_values()
        .map(|g| meeting_of_group(items, g, &mut taken))
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn join_uid_matches(
    items: &[Item],
    parent: &mut Vec<usize>,
    join: &impl Fn(&mut Vec<usize>, usize, usize),
) {
    // rule 2: the same uid and original start
    let mut by_uid: BTreeMap<(String, String, String), Vec<usize>> = BTreeMap::new();
    for (i, it) in items.iter().enumerate() {
        if let Some(uid) = it.ical_uid.as_deref().filter(|u| !u.is_empty()) {
            let at = it
                .original_start
                .clone()
                .unwrap_or_else(|| it.start_ms.to_string());
            by_uid
                .entry((it.party.clone(), uid.to_string(), at))
                .or_default()
                .push(i);
        }
    }
    for xs in by_uid.values() {
        for a in 0..xs.len() {
            for b in a + 1..xs.len() {
                join(parent, xs[a], xs[b]);
            }
        }
    }
}

fn join_organizer_matches(
    items: &[Item],
    parent: &mut Vec<usize>,
    join: &impl Fn(&mut Vec<usize>, usize, usize),
) {
    // rule 3: organizer, a minute, title
    let mut by_org: BTreeMap<(String, String, String), Vec<usize>> = BTreeMap::new();
    for (i, it) in items.iter().enumerate() {
        let (Some(org), Some(title)) = (&it.organizer, it.title.as_deref().map(norm_title)) else {
            continue;
        };
        if title.is_empty() {
            continue;
        }
        by_org
            .entry((it.party.clone(), org.clone(), title))
            .or_default()
            .push(i);
    }
    for xs in by_org.values_mut() {
        xs.sort_by_key(|i| items[*i].start_ms);
        for a in 0..xs.len() {
            for b in a + 1..xs.len() {
                if items[xs[b]].start_ms - items[xs[a]].start_ms > 60_000 {
                    break;
                }
                join(parent, xs[a], xs[b]);
            }
        }
    }
}

fn meeting_of_group(items: &[Item], g: Vec<usize>, taken: &mut HashSet<String>) -> Meeting {
    let primary = g
        .iter()
        .copied()
        .min_by(|a, b| {
            let (x, y) = (&items[*a], &items[*b]);
            (!x.can_modify, !x.has_link, x.busy_only, &x.id).cmp(&(
                !y.can_modify,
                !y.has_link,
                y.busy_only,
                &y.id,
            ))
        })
        .expect("a group has a member");
    let mut events: Vec<String> = g.iter().map(|i| items[*i].id.clone()).collect();
    events.sort();
    let p = &items[primary];
    let key = match &p.ical_uid {
        Some(uid) => format!(
            "uid:{uid}:{}",
            p.original_start
                .clone()
                .unwrap_or_else(|| p.start_ms.to_string())
        ),
        None => format!("event:{}", p.id),
    };
    let mut prior: Vec<&String> = std::iter::once(primary)
        .chain(g.iter().copied())
        .filter_map(|i| items[i].meeting.as_ref())
        .collect();
    prior.dedup();
    let id = prior
        .into_iter()
        .find(|m| !taken.contains(*m))
        .cloned()
        .unwrap_or_else(|| {
            let mut parts: Vec<&str> = vec!["meeting"];
            parts.extend(events.iter().map(String::as_str));
            let mut id = format!("mtg_{}", crate::hash(&parts));
            while taken.contains(&id) {
                id = format!("mtg_{}", crate::hash(&[&id]));
            }
            id
        });
    taken.insert(id.clone());
    Meeting {
        id,
        primary: p.id.clone(),
        key,
        events,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(
        id: &str,
        cal: &str,
        uid: Option<&str>,
        org: Option<&str>,
        start: i64,
        title: &str,
    ) -> Item {
        Item {
            id: id.into(),
            calendar: cal.into(),
            party: "me".into(),
            ical_uid: uid.map(str::to_string),
            original_start: None,
            organizer: org.map(str::to_string),
            start_ms: start,
            title: Some(title.into()),
            can_modify: false,
            has_link: false,
            busy_only: false,
            meeting: None,
        }
    }

    #[test]
    fn same_uid_or_organizer_minute_and_title_join_across_calendars() {
        let mut a = it("e1", "work", Some("u1"), None, 0, "Review");
        let b = it("e2", "mail", Some("u1"), None, 0, "Review (invite)");
        a.can_modify = true;
        let c = it("e3", "work", None, Some("ada@x"), 3_600_000, "Weekly sync!");
        let mut d = it("e4", "mail", None, Some("ada@x"), 3_630_000, "weekly  SYNC");
        d.has_link = true;
        let ms = meetings(&[a, b, c, d], &HashSet::new());
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[0].events.len() + ms[1].events.len(), 4);
        let uid = ms
            .iter()
            .find(|m| m.events.contains(&"e1".to_string()))
            .unwrap();
        assert_eq!(uid.primary, "e1", "the one the owner may change");
        let org = ms
            .iter()
            .find(|m| m.events.contains(&"e3".to_string()))
            .unwrap();
        assert_eq!(org.primary, "e4", "the one with a meeting link");
    }

    #[test]
    fn weekly_meetings_with_one_title_stay_apart() {
        // the same weekly title, a week apart, and two series at one time by different organizers
        let a = it("e1", "work", None, Some("ada@x"), 0, "Weekly");
        let b = it("e2", "mail", None, Some("ada@x"), 7 * 86_400_000, "Weekly");
        let c = it("e3", "mail", None, Some("bo@x"), 0, "Weekly");
        // two occurrences of one calendar never join
        let d = it("e4", "work", None, Some("ada@x"), 30_000, "Weekly");
        let ms = meetings(&[a, b, c, d], &HashSet::new());
        assert_eq!(ms.len(), 4, "{ms:?}");
    }

    #[test]
    fn a_meeting_keeps_its_id_when_an_occurrence_leaves() {
        let mut a = it("e9", "work", Some("u1"), None, 0, "Review");
        let mut b = it("e1", "mail", Some("u1"), None, 0, "Review");
        let first = meetings(&[a.clone(), b.clone()], &HashSet::new());
        assert_eq!(first.len(), 1);
        a.meeting = Some(first[0].id.clone());
        b.meeting = Some(first[0].id.clone());
        // e1 (the smallest id) goes: e9 keeps the meeting
        assert_eq!(meetings(&[a.clone()], &HashSet::new())[0].id, first[0].id);
        // both stay but are split: one keeps it, the other gets a new one
        let no: HashSet<_> = [("e1".to_string(), "e9".to_string())].into();
        let split = meetings(&[a, b], &no);
        assert_eq!(split.iter().filter(|m| m.id == first[0].id).count(), 1);
        assert_ne!(split[0].id, split[1].id);
    }

    #[test]
    fn not_same_and_other_people_stay_apart() {
        let a = it("e1", "work", Some("u1"), None, 0, "Review");
        let b = it("e2", "mail", Some("u1"), None, 0, "Review");
        let mut c = it("e3", "colleague", Some("u1"), None, 0, "Review");
        c.party = "bo".into();
        let no: HashSet<_> = [("e2".to_string(), "e1".to_string())].into();
        assert_eq!(meetings(&[a, b, c], &no).len(), 3);
    }
}
