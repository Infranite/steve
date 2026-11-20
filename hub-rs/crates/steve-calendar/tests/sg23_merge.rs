//! SG23: grouping boundaries and stable identities before extracting merge helpers.
use std::collections::HashSet;
use steve_calendar::merge::{Item, meetings};

fn item(id: &str, calendar: &str) -> Item {
    Item {
        id: id.into(),
        calendar: calendar.into(),
        party: "owner".into(),
        ical_uid: None,
        original_start: None,
        organizer: None,
        start_ms: 0,
        title: Some("Weekly sync!".into()),
        can_modify: false,
        has_link: false,
        busy_only: false,
        meeting: None,
    }
}

#[test]
fn sg23_pair_matching_boundaries_table() {
    for (name, change, expected) in [
        ("uid", 0, 1),
        ("different party", 1, 2),
        ("same calendar", 2, 2),
        ("different original", 3, 2),
        ("original equals fallback", 4, 1),
        ("uid differs", 5, 2),
        ("empty uid", 6, 2),
        ("organizer exactly one minute", 7, 1),
        ("organizer over minute", 8, 2),
        ("organizer case is exact", 9, 2),
        ("missing title", 10, 2),
        ("punctuation only", 11, 2),
        ("negative starts", 12, 1),
        ("not same forward", 13, 2),
        ("not same backward", 14, 2),
    ] {
        let mut a = item("a", "one");
        let mut b = item("b", "two");
        a.ical_uid = Some("uid".into());
        b.ical_uid = a.ical_uid.clone();
        let mut apart = HashSet::new();
        match change {
            1 => b.party = "other".into(),
            2 => b.calendar = a.calendar.clone(),
            3 => b.original_start = Some("different".into()),
            4 => a.original_start = Some("0".into()),
            5 => b.ical_uid = Some("other".into()),
            6 => {
                a.ical_uid = Some(String::new());
                b.ical_uid = Some(String::new());
            }
            7..=12 => {
                a.ical_uid = None;
                b.ical_uid = None;
                a.organizer = Some("ada@example.com".into());
                b.organizer = a.organizer.clone();
                b.title = Some("weekly  SYNC".into());
                b.start_ms = 60_000;
                match change {
                    8 => b.start_ms += 1,
                    9 => b.organizer = Some("ADA@example.com".into()),
                    10 => b.title = None,
                    11 => {
                        a.title = Some("!?".into());
                        b.title = Some("!".into());
                    }
                    12 => {
                        a.start_ms = -60_000;
                        b.start_ms = -1;
                    }
                    _ => {}
                }
            }
            13 => {
                apart.insert((a.id.clone(), b.id.clone()));
            }
            14 => {
                apart.insert((b.id.clone(), a.id.clone()));
            }
            _ => {}
        }
        assert_eq!(meetings(&[a, b], &apart).len(), expected, "{name}");
    }
    assert!(meetings(&[], &HashSet::new()).is_empty());
}

#[test]
fn sg23_exclusions_are_direct_not_cluster_wide() {
    let mut a = item("a", "one");
    let mut b = item("b", "two");
    let mut c = item("c", "three");
    a.ical_uid = Some("uid".into());
    b.ical_uid = a.ical_uid.clone();
    b.organizer = Some("ada".into());
    c.organizer = b.organizer.clone();
    c.start_ms = 60_000;
    let apart = HashSet::from([("a".into(), "c".into())]);
    let out = meetings(&[a.clone(), b.clone(), c.clone()], &apart);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].events, ["a", "b", "c"]);
    // A same-calendar pair can also share a cluster through a third occurrence.
    c.calendar = a.calendar.clone();
    assert_eq!(meetings(&[a, b, c], &HashSet::new()).len(), 1);
}

#[test]
fn sg23_primary_selection_and_key_table() {
    for (modify, link, busy, winner) in [
        (false, false, false, "a"),
        (true, false, true, "z"),
        (false, true, true, "z"),
        (false, false, true, "a"),
    ] {
        let mut a = item("a", "one");
        let mut z = item("z", "two");
        a.ical_uid = Some("u".into());
        z.ical_uid = a.ical_uid.clone();
        z.can_modify = modify;
        z.has_link = link;
        z.busy_only = busy;
        let out = meetings(&[z, a], &HashSet::new());
        assert_eq!(out[0].primary, winner);
        assert_eq!(out[0].key, "uid:u:0");
        assert_eq!(out[0].events, ["a", "z"]);
    }
    let mut a = item("a", "one");
    let mut b = item("b", "two");
    a.ical_uid = Some(String::new());
    a.organizer = Some("ada".into());
    b.organizer = a.organizer.clone();
    assert_eq!(meetings(&[a, b], &HashSet::new())[0].key, "uid::0");
}

#[test]
fn sg23_stable_ids_prior_preference_collision_and_output_order() {
    let mut a = item("a", "one");
    let mut b = item("b", "two");
    a.ical_uid = Some("u".into());
    b.ical_uid = a.ical_uid.clone();
    a.meeting = Some("old-a".into());
    b.meeting = Some("old-b".into());
    b.can_modify = true;
    assert_eq!(
        meetings(&[a.clone(), b.clone()], &HashSet::new())[0].id,
        "old-b"
    );
    b.meeting = None;
    assert_eq!(
        meetings(&[b.clone(), a.clone()], &HashSet::new())[0].id,
        "old-a"
    );
    a.meeting = None;
    assert_eq!(
        meetings(&[a.clone(), b.clone()], &HashSet::new()),
        meetings(&[b, a], &HashSet::new())
    );

    let mut first = item("first", "one");
    let second = item("second", "two");
    let generated = format!("mtg_{}", steve_calendar::hash(&["meeting", "second"]));
    first.meeting = Some(generated.clone());
    let out = meetings(&[first, second], &HashSet::new());
    assert!(out.windows(2).all(|w| w[0].id < w[1].id));
    assert_eq!(
        out.iter().find(|m| m.primary == "second").unwrap().id,
        format!("mtg_{}", steve_calendar::hash(&[&generated]))
    );
}
