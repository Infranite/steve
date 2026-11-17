//! A todo's reminder and snooze wake-up, end to end on a migrated store: the triggers mark it, the
//! `todo` alarm source plans it, the alarm clock fires it once, and the notification intake writes its
//! owner's notification and feed entry. Synthetic data only.

use std::sync::Arc;

use rusqlite::params;
use steve_digest::todo;
use steve_digest::todo_alarms::TodoAlarms;
use steve_flow::alarms::{self, AlarmSources};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS};
use steve_store::outbox::Dispatcher;

/// 2027-01-31 10:00 UTC
const NOW: i64 = 1_801_389_600_000;
const HOUR: i64 = 3_600_000;

struct Env {
    e: TestEnv,
    sources: AlarmSources,
    intake: Dispatcher,
}

fn env() -> Env {
    let e = TestEnv::new(|_| {});
    e.clock.set(NOW);
    let mut sources = AlarmSources::default();
    sources.add(Arc::new(TodoAlarms));
    let intake = Dispatcher::new(steve_channels::notifications::AlarmIntake::all());
    intake.register(e.store()).unwrap();
    Env { e, sources, intake }
}

impl Env {
    fn add(&self, owner: &str, text: &str, f: impl FnOnce(&mut todo::NewTodo)) -> String {
        self.e
            .store()
            .transaction(|tx| {
                let mut t = todo::NewTodo {
                    ws: WS,
                    owner,
                    text,
                    source: "you",
                    writer: owner,
                    ..Default::default()
                };
                f(&mut t);
                todo::create(tx, &t, NOW)
            })
            .unwrap()
    }

    fn edit(&self, id: &str, p: todo::Patch) {
        self.e
            .store()
            .transaction(|tx| todo::edit(tx, WS, id, &p, NOW))
            .unwrap();
    }

    /// One alarm clock round at `now`, then the outbox.
    fn round(&self, now: i64) -> alarms::Claimed {
        alarms::replan_dirty(self.e.store(), &self.sources, now).unwrap();
        let c = alarms::claim_due(self.e.store(), &self.sources, now).unwrap();
        self.intake.pump(self.e.store(), now).unwrap();
        c
    }

    fn states(&self, id: &str) -> Vec<(String, i64, String)> {
        self.e
            .store()
            .reader(|c| alarms::of_subject(c, WS, &format!("todo:{id}")))
            .unwrap()
            .into_iter()
            .map(|v| {
                (
                    v["kind"].as_str().unwrap().into(),
                    v["planned_for"].as_i64().unwrap(),
                    v["state"].as_str().unwrap().into(),
                )
            })
            .collect()
    }

    /// `(user, topic, mode, state, body, feed kind)` of every personal notification.
    fn notes(&self) -> Vec<(String, String, String, String, String, String)> {
        self.e
            .store()
            .reader(|c| {
                let mut st = c.prepare(
                    "SELECT n.user_id, n.topic, n.mode, n.state, n.body, f.kind FROM personal_notifications n
                       JOIN feed_entries f ON f.notification_id = n.id ORDER BY n.created, n.id",
                )?;
                let v = st
                    .query_map([], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok::<_, steve_store::StoreError>(v)
            })
            .unwrap()
    }
}

/// The stored body is what the feed shows and what the Bot DM sends as a Feishu `text` message: plain
/// text, no markdown link and no relative `#/…` fragment (neither can be opened from a DM).
fn assert_plain(body: &str) {
    assert!(!body.contains("]("), "no markdown link: {body}");
    assert!(!body.contains("#/"), "no web fragment: {body}");
}

#[test]
fn a_reminder_is_planned_moved_and_rung_once_into_its_owners_feed() {
    let e = env();
    let id = e.add(ME, "交 Q4 复盘", |t| {
        t.remind_at = Some(NOW + HOUR);
        t.due_date = Some("2027-02-05");
    });
    e.round(NOW);
    assert_eq!(
        e.states(&id),
        vec![("todo".into(), NOW + HOUR, "planned".into())]
    );

    // moved by the person: the old time is cancelled
    e.edit(
        &id,
        todo::Patch {
            remind_at: Some(Some(NOW + 2 * HOUR)),
            ..Default::default()
        },
    );
    e.round(NOW);
    assert_eq!(
        e.states(&id),
        vec![
            ("todo".into(), NOW + HOUR, "cancelled".into()),
            ("todo".into(), NOW + 2 * HOUR, "planned".into()),
        ]
    );
    assert_eq!(e.round(NOW + HOUR).fired, 0);
    assert_eq!(e.round(NOW + 2 * HOUR).fired, 1);
    assert_eq!(e.round(NOW + 3 * HOUR).fired, 0);

    let notes = e.notes();
    assert_eq!(notes.len(), 1);
    let (user, topic, mode, state, body, kind) = &notes[0];
    assert_eq!((user.as_str(), topic.as_str()), (ME, alarms::TODO_DUE));
    // no notification policy: kept in the feed, not sent
    assert_eq!((mode.as_str(), state.as_str()), ("log", "held"));
    assert_eq!(kind, "reminder");
    assert!(body.starts_with("待办提醒：交 Q4 复盘"), "{body}");
    assert!(body.contains("截止 2027-02-05"), "{body}");
    assert_plain(body);
}

#[test]
fn done_dropped_or_handed_todos_lose_their_alarms_and_a_snooze_wakes_its_owner() {
    let e = env();
    let a = e.add(ME, "打电话", |t| t.remind_at = Some(NOW + HOUR));
    let b = e.add(ME, "看合同", |t| t.snooze_until = Some(NOW + HOUR));
    let c = e.add(OTHER, "别人的事", |t| t.snooze_until = Some(NOW + HOUR));
    e.round(NOW);
    e.e.store()
        .transaction(|tx| todo::complete(tx, WS, &a, "done", None, NOW))
        .unwrap();
    e.round(NOW);
    assert_eq!(e.states(&a)[0].2, "cancelled");

    // reopened before its time (an undo): planned again
    e.e.store()
        .transaction(|tx| todo::set_state(tx, WS, &a, "open", None, NOW))
        .unwrap();
    e.round(NOW);
    assert_eq!(e.states(&a)[0].2, "planned");

    let fired = e.round(NOW + HOUR);
    assert_eq!(fired.fired, 3);
    assert_eq!(
        e.states(&b),
        vec![("snooze".into(), NOW + HOUR, "fired".into())]
    );
    let notes = e.notes();
    let mine: Vec<_> = notes.iter().filter(|n| n.0 == ME).collect();
    assert_eq!(mine.len(), 2, "a reminder and a wake-up: two topics");
    assert!(
        mine.iter()
            .any(|n| n.1 == alarms::SNOOZE_WAKE && n.4.starts_with("推迟的待办回来了：看合同"))
    );
    let theirs: Vec<_> = notes.iter().filter(|n| n.0 == OTHER).collect();
    assert_eq!(theirs.len(), 1);
    assert!(theirs[0].4.contains("别人的事"));
    assert!(!mine.iter().any(|n| n.4.contains("别人的事")));
    let _ = c;

    // deleting a todo whose alarm is planned cancels it
    let d = e.add(ME, "删掉的", |t| t.remind_at = Some(NOW + 5 * HOUR));
    e.round(NOW + HOUR);
    e.e.store()
        .with_conn(|c| Ok(c.execute("DELETE FROM todos WHERE id = ?", [&d])?))
        .unwrap();
    e.round(NOW + HOUR);
    assert_eq!(e.states(&d)[0].2, "cancelled");
}

#[test]
fn a_recurring_todos_next_one_gets_its_reminder_and_a_policy_that_sends_waits_to_send() {
    let e = env();
    e.e.store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO notification_policies (workspace_id, user_id, enabled, mode, timezone, consented_at, consent_since_seq, updated)
                 VALUES (?, ?, 1, 'now', 'UTC', 1, 0, 1)",
                params![WS, ME],
            )?;
            Ok(())
        })
        .unwrap();
    let id = e.add(ME, "交周报", |t| {
        t.due = Some(NOW + 2 * HOUR);
        t.remind_at = Some(NOW + HOUR);
        t.recur = Some("FREQ=WEEKLY");
    });
    e.round(NOW);
    let next =
        e.e.store()
            .transaction(|tx| todo::complete(tx, WS, &id, "done", None, NOW))
            .unwrap()
            .expect("the next one");
    e.round(NOW);
    assert_eq!(e.states(&id)[0].2, "cancelled");
    let week = 7 * 24 * HOUR;
    assert_eq!(
        e.states(&next),
        vec![("todo".into(), NOW + HOUR + week, "planned".into())]
    );
    e.round(NOW + HOUR + week);
    let notes = e.notes();
    assert_eq!(notes.len(), 1);
    // sent under the policy: waiting for the notification pump, which delivers through E1
    assert_eq!(
        (notes[0].2.as_str(), notes[0].3.as_str()),
        ("now", "waiting")
    );
    assert!(notes[0].4.contains("截止 02-07 12:00"), "{}", notes[0].4);
    assert_plain(&notes[0].4);
}

#[test]
fn reminders_due_together_are_sent_as_one_plain_message() {
    let e = env();
    e.e.store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO notification_policies (workspace_id, user_id, enabled, mode, timezone, consented_at, consent_since_seq, updated)
                 VALUES (?, ?, 1, 'now', 'UTC', 1, 0, 1)",
                params![WS, ME],
            )?;
            Ok(())
        })
        .unwrap();
    e.add(ME, "回邮件", |t| t.remind_at = Some(NOW + HOUR));
    e.add(ME, "订机票", |t| {
        t.remind_at = Some(NOW + HOUR);
        t.due_date = Some("2027-02-03");
    });
    e.round(NOW);
    assert_eq!(e.round(NOW + HOUR).fired, 2);
    let notes = e.notes();
    assert_eq!(notes.len(), 1, "one message for the two: {notes:?}");
    let body = &notes[0].4;
    assert_eq!(
        (notes[0].2.as_str(), notes[0].3.as_str()),
        ("now", "waiting")
    );
    assert!(
        body.contains("- 回邮件") && body.contains("- 订机票"),
        "{body}"
    );
    assert!(body.contains("截止 2027-02-03"), "{body}");
    assert_plain(body);
}
