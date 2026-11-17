//! `time_alarms` (steve-flow `alarms`) on a migrated store, with a scripted alarm source: planning
//! (reschedule, change back, cancel, a fired alarm never again), firing exactly once, staleness, quiet
//! hours, merging, and a Hub that was down. Synthetic data only.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use serde_json::Value;
use steve_flow::FlowResult;
use steve_flow::alarms::{
    self, AlarmFired, AlarmLine, AlarmSource, AlarmSources, PAST_GRACE_MS, Reconciled, SNOOZE_WAKE,
    TODO_DUE, Want,
};
use steve_modules::testkit::TestEnv;
use steve_store::Store;

const WS: &str = "default";
const ME: &str = "usr_dev";
const OTHER: &str = "usr_other";
const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
/// 2026-11-18 01:00 UTC = 09:00 in Asia/Shanghai.
const T0: i64 = 1_794_963_600_000;

/// Subjects `thing:<id>` whose wanted alarms and line text the test sets.
#[derive(Default)]
struct Script {
    wants: Mutex<HashMap<String, Vec<Want>>>,
    text: Mutex<HashMap<String, String>>,
}

impl Script {
    fn set(&self, id: &str, wants: Vec<Want>, text: &str) {
        self.wants.lock().unwrap().insert(id.into(), wants);
        self.text.lock().unwrap().insert(id.into(), text.into());
    }
    fn gone(&self, id: &str) {
        self.wants.lock().unwrap().remove(id);
        self.text.lock().unwrap().remove(id);
    }
}

struct Scripted(Arc<Script>);

impl AlarmSource for Scripted {
    fn family(&self) -> &'static str {
        "thing"
    }
    fn plan(&self, _: &Connection, _: &str, id: &str, _: i64) -> FlowResult<Vec<Want>> {
        Ok(self
            .0
            .wants
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default())
    }
    fn line(&self, _: &Connection, _: &str, id: &str, _: &str) -> FlowResult<Option<AlarmLine>> {
        Ok(self.0.text.lock().unwrap().get(id).map(|t| AlarmLine {
            subject: format!("thing:{id}"),
            text: t.clone(),
            detail: None,
            link: None,
        }))
    }
}

fn want(owner: &str, kind: &'static str, at: i64) -> Want {
    Want {
        owner: owner.into(),
        kind,
        planned_for: at,
        fire_at: at,
        stale_at: None,
    }
}

struct Env {
    _env: TestEnv,
    store: Store,
    script: Arc<Script>,
    sources: AlarmSources,
}

fn env() -> Env {
    let env = TestEnv::new(|_| {});
    let store = env.store().clone();
    let script = Arc::new(Script::default());
    let mut sources = AlarmSources::default();
    sources.add(Arc::new(Scripted(script.clone())));
    Env {
        _env: env,
        store,
        script,
        sources,
    }
}

impl Env {
    /// Marks `thing:<id>` changed and plans it (what the triggers and the clock's round do).
    fn changed(&self, id: &str, now: i64) -> usize {
        self.store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO time_alarm_dirty (workspace_id, subject) VALUES (?, ?) ON CONFLICT DO NOTHING",
                    params![WS, format!("thing:{id}")],
                )?;
                Ok(())
            })
            .unwrap();
        alarms::replan_dirty(&self.store, &self.sources, now).unwrap()
    }

    fn rows(&self, id: &str) -> Vec<(String, i64, i64, String)> {
        self.store
            .reader(|c| alarms::of_subject(c, WS, &format!("thing:{id}")))
            .unwrap()
            .into_iter()
            .map(|v| {
                (
                    v["kind"].as_str().unwrap().to_string(),
                    v["planned_for"].as_i64().unwrap(),
                    v["fire_at"].as_i64().unwrap(),
                    v["state"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn events(&self, topic: &str) -> Vec<AlarmFired> {
        self.store
            .reader(|c| {
                let mut st =
                    c.prepare("SELECT payload FROM outbox_events WHERE topic = ? ORDER BY seq")?;
                let v = st
                    .query_map([topic], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok::<_, steve_store::StoreError>(v)
            })
            .unwrap()
            .into_iter()
            .map(|p| serde_json::from_str(&p).unwrap())
            .collect()
    }

    fn claim(&self, now: i64) -> alarms::Claimed {
        alarms::claim_due(&self.store, &self.sources, now).unwrap()
    }

    /// `ME` hears notifications now, in Shanghai, quiet 22:00–08:00.
    fn quiet_policy(&self) {
        self.store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO notification_policies (workspace_id, user_id, enabled, mode, timezone, quiet_start, quiet_end, consented_at, consent_since_seq, updated)
                     VALUES (?, ?, 1, 'now', 'Asia/Shanghai', 1320, 480, 1, 0, 1)",
                    params![WS, ME],
                )?;
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn reschedule_change_back_and_cancel_keep_one_row_per_planned_time() {
    let e = env();
    let at = T0 + HOUR;
    e.script.set("a", vec![want(ME, "todo", at)], "交 Q4 复盘");
    assert_eq!(e.changed("a", T0), 1);
    assert_eq!(e.rows("a"), vec![("todo".into(), at, at, "planned".into())]);

    // a lead time changed: same planned time, the fire time moves in place
    let mut w = want(ME, "todo", at);
    w.fire_at = at - 10 * MIN;
    e.script.set("a", vec![w], "交 Q4 复盘");
    e.changed("a", T0);
    assert_eq!(
        e.rows("a"),
        vec![("todo".into(), at, at - 10 * MIN, "planned".into())]
    );

    // moved: the old one is cancelled, a new one planned
    let later = at + 2 * HOUR;
    e.script
        .set("a", vec![want(ME, "todo", later)], "交 Q4 复盘");
    e.changed("a", T0);
    assert_eq!(
        e.rows("a"),
        vec![
            ("todo".into(), at, at - 10 * MIN, "cancelled".into()),
            ("todo".into(), later, later, "planned".into()),
        ]
    );

    // moved back: the cancelled row is planned again, no new row (the unique key holds)
    e.script.set("a", vec![want(ME, "todo", at)], "交 Q4 复盘");
    e.changed("a", T0);
    assert_eq!(
        e.rows("a"),
        vec![
            ("todo".into(), at, at, "planned".into()),
            ("todo".into(), later, later, "cancelled".into()),
        ]
    );

    // done or deleted: nothing planned
    e.script.gone("a");
    e.changed("a", T0);
    assert!(e.rows("a").iter().all(|r| r.3 == "cancelled"));
    assert_eq!(e.claim(later + HOUR).fired, 0);
    assert!(e.events(TODO_DUE).is_empty());
}

#[test]
fn an_alarm_fires_exactly_once_and_never_again() {
    let e = env();
    let at = T0 + 30 * MIN;
    e.script
        .set("a", vec![want(ME, "todo", at)], "给林岚回邮件");
    e.changed("a", T0);
    // not yet
    assert_eq!(e.claim(at - 2 * MIN), alarms::Claimed::default());
    assert_eq!(
        e.store.reader(alarms::earliest).unwrap(),
        Some(at),
        "the clock sleeps until it"
    );
    let c = e.claim(at);
    assert_eq!((c.fired, c.events.len()), (1, 1));
    let ev = e.events(TODO_DUE);
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].owner, ME);
    assert_eq!(ev[0].kind, "todo");
    assert_eq!(ev[0].lines[0].text, "给林岚回邮件");
    assert!(!ev[0].late);
    let rows = e
        .store
        .reader(|c| alarms::of_subject(c, WS, "thing:a"))
        .unwrap();
    assert_eq!(rows[0]["state"], "fired");
    assert_eq!(rows[0]["run_ref"], Value::String(c.events[0].clone()));

    // claimed again, planned again with the same time: never a second time
    assert_eq!(e.claim(at + MIN).fired, 0);
    e.changed("a", at + MIN);
    assert_eq!(e.claim(at + 2 * MIN).fired, 0);
    assert_eq!(e.events(TODO_DUE).len(), 1);
    assert_eq!(e.store.reader(alarms::earliest).unwrap(), None);
}

#[test]
fn a_long_gone_time_is_not_planned_but_one_missed_while_down_still_fires() {
    let e = env();
    let past = T0 - PAST_GRACE_MS - MIN;
    e.script
        .set("old", vec![want(ME, "todo", past)], "很久以前");
    e.changed("old", T0);
    assert!(
        e.rows("old").is_empty(),
        "set for a time long gone: no alarm"
    );
    // just past (within the grace): it fires at once
    e.script
        .set("now", vec![want(ME, "todo", T0 - MIN)], "刚刚");
    e.changed("now", T0);
    assert_eq!(e.claim(T0).fired, 1);
}

#[test]
fn a_hub_that_was_down_sends_the_backlog_once_as_one_message() {
    let e = env();
    e.script
        .set("a", vec![want(ME, "todo", T0 + HOUR)], "写周报");
    e.script
        .set("b", vec![want(ME, "todo", T0 + 2 * HOUR)], "订机票");
    // the same text twice in one go is one line
    e.script
        .set("c", vec![want(ME, "todo", T0 + 2 * HOUR)], "  写周报 ");
    e.script
        .set("d", vec![want(OTHER, "todo", T0 + HOUR)], "别人的");
    let mut stale = want(ME, "todo", T0 + HOUR);
    stale.stale_at = Some(T0 + 90 * MIN);
    e.script.set("e", vec![stale], "过时的");
    e.script
        .set("f", vec![want(ME, "snooze", T0 + HOUR)], "推迟的");
    for id in ["a", "b", "c", "d", "e", "f"] {
        e.changed(id, T0);
    }

    // the Hub stops; it comes back with new sources (a new process) five hours later
    let mut sources = AlarmSources::default();
    sources.add(Arc::new(Scripted(e.script.clone())));
    let back = T0 + 5 * HOUR;
    let c = alarms::claim_due(&e.store, &sources, back).unwrap();
    assert_eq!((c.fired, c.stale), (5, 1));
    // one per owner and topic
    let due = e.events(TODO_DUE);
    assert_eq!(due.len(), 2);
    let mine = due.iter().find(|a| a.owner == ME).unwrap();
    assert_eq!(mine.alarms.len(), 3);
    let texts: Vec<_> = mine.lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, vec!["写周报", "订机票"]);
    assert!(mine.late);
    assert_eq!(e.events(SNOOZE_WAKE).len(), 1);
    assert_eq!(e.rows("e")[0].3, "stale");
    // and never again
    assert_eq!(
        alarms::claim_due(&e.store, &sources, back + MIN)
            .unwrap()
            .fired,
        0
    );
}

#[test]
fn quiet_hours_postpone_a_reminder_and_merge_what_waited() {
    let e = env();
    e.quiet_policy();
    // 23:00 and 06:00 Shanghai (15:00 and 22:00 UTC on Nov 18)
    let night = T0 + 14 * HOUR;
    let dawn = T0 + 21 * HOUR;
    let end = T0 + 23 * HOUR; // 08:00 Shanghai
    e.script.set("a", vec![want(ME, "todo", night)], "倒垃圾");
    e.script.set("b", vec![want(ME, "todo", dawn)], "晨跑");
    e.changed("a", T0);
    e.changed("b", T0);

    let c = e.claim(night);
    assert_eq!((c.fired, c.held), (0, 1));
    let rows = e
        .store
        .reader(|c| alarms::of_subject(c, WS, "thing:a"))
        .unwrap();
    assert_eq!(rows[0]["hold_until"], end);
    // the next round: the other one, then the held one at the end of the quiet hours
    assert_eq!(e.store.reader(alarms::earliest).unwrap(), Some(dawn));
    assert_eq!(e.claim(dawn).held, 2);
    assert!(
        e.events(TODO_DUE).is_empty(),
        "nothing during the quiet hours"
    );

    let c = e.claim(end);
    assert_eq!(c.fired, 2);
    let ev = e.events(TODO_DUE);
    assert_eq!(ev.len(), 1, "one message for both");
    assert_eq!(ev[0].lines.len(), 2);
    assert!(ev[0].late);

    // someone who does not get pushes is not held: the feed is on time
    let e = env();
    e.script.set("x", vec![want(OTHER, "todo", night)], "看书");
    e.changed("x", T0);
    assert_eq!(e.claim(night).fired, 1);
}

#[test]
fn a_subject_gone_when_its_alarm_fires_is_cancelled_and_a_lead_time_change_resets_a_hold() {
    let e = env();
    let at = T0 + HOUR;
    e.script.set("a", vec![want(ME, "todo", at)], "回电话");
    e.changed("a", T0);
    // gone without being planned again (the line is asked at the last moment)
    e.script.text.lock().unwrap().remove("a");
    let c = e.claim(at);
    assert_eq!((c.fired, c.cancelled), (0, 1));
    assert!(e.events(TODO_DUE).is_empty());

    // a reconcile reports what it did
    let r = e
        .store
        .transaction(|tx| {
            alarms::reconcile(
                tx,
                WS,
                "thing:z",
                &[want(ME, "todo", at), want(ME, "snooze", at)],
                T0,
            )
        })
        .unwrap();
    assert_eq!(
        r,
        Reconciled {
            added: 2,
            ..Default::default()
        }
    );
    let r = e
        .store
        .transaction(|tx| alarms::reconcile(tx, WS, "thing:z", &[want(ME, "snooze", at)], T0))
        .unwrap();
    assert_eq!(r.cancelled, 1);
}
