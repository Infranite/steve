//! The morning brief end to end on a migrated store: turned on by a member's first own todo, planned as
//! alarms on their clock, fired once per day, made by rule from their todos, items and 待处理, and kept in
//! their feed or sent under their policy. Synthetic data only.

#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use std::sync::Arc;

use chrono::{NaiveDate, TimeZone};
use steve_digest::brief::{self, BriefAlarms, BriefIntake};
use steve_digest::todo;
use steve_flow::alarms::{self, AlarmSources};
use steve_flow::schedules::{self, Fields, Mode};
use steve_modules::testkit::{ME, OTHER, WS};
use steve_store::outbox::Dispatcher;
use steve_store::{
    digest::{TodoFilter, Todos},
    testkit::digest as fixture,
};

const TZ: chrono_tz::Tz = chrono_tz::Asia::Taipei;
const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;

/// Unix ms of a local time in the workspace's default zone (Asia/Taipei).
fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
    TZ.from_local_datetime(
        &NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap(),
    )
    .single()
    .unwrap()
    .timestamp_millis()
}

/// Wednesday 2027-02-03 07:00 local.
fn now() -> i64 {
    local(2027, 2, 3, 7, 0)
}

struct Env {
    e: TestEnv,
    sources: AlarmSources,
    intake: Dispatcher,
}

fn env() -> Env {
    let e = TestEnv::new(|_| {});
    e.clock.set(now());
    e.store()
        .settings()
        .set(
            WS,
            &serde_json::Map::from_iter([("tz".into(), "Asia/Taipei".into())]),
            ME,
        )
        .unwrap();
    let mut sources = AlarmSources::default();
    sources.add(Arc::new(BriefAlarms));
    let intake = Dispatcher::new(vec![Arc::new(BriefIntake)]);
    intake.register(e.store()).unwrap();
    Env { e, sources, intake }
}

impl Env {
    fn add(&self, owner: &str, text: &str, f: impl FnOnce(&mut todo::NewTodo)) -> String {
        self.e
            .store()
            .write(|tx| {
                let mut t = todo::NewTodo {
                    ws: WS,
                    owner,
                    text,
                    source: "you",
                    writer: owner,
                    ..Default::default()
                };
                f(&mut t);
                todo::create(tx, &t, now())
            })
            .unwrap()
    }

    fn policy(&self, user: &str, tz: &str, quiet: Option<(u16, u16)>) {
        fixture::notification_policy(self.e.store(), WS, user, true, tz, quiet).unwrap();
    }

    fn brief(&self, user: &str) -> Option<schedules::Schedule> {
        self.e
            .store()
            .read(|c| brief::brief_of(c, WS, user))
            .unwrap()
    }

    /// One alarm clock round at `at`, then the outbox.
    fn round(&self, at: i64) -> alarms::Claimed {
        self.e.clock.set(at);
        alarms::replan_dirty(self.e.store(), &self.sources, at).unwrap();
        let c = alarms::claim_due(self.e.store(), &self.sources, at).unwrap();
        // the clock plans the subjects its round fired again in the next round
        alarms::replan_dirty(self.e.store(), &self.sources, at).unwrap();
        self.intake.pump(self.e.store(), at).unwrap();
        c
    }

    /// `(planned_for, state)` of a brief's alarms.
    fn alarms(&self, id: &str) -> Vec<(i64, String)> {
        self.e
            .store()
            .reader(|c| alarms::of_subject(c, WS, &format!("brief:{id}")))
            .unwrap()
            .into_iter()
            .map(|v| {
                (
                    v["planned_for"].as_i64().unwrap(),
                    v["state"].as_str().unwrap().into(),
                )
            })
            .collect()
    }

    fn planned(&self, id: &str) -> Vec<i64> {
        self.alarms(id)
            .into_iter()
            .filter(|(_, s)| s == "planned")
            .map(|(t, _)| t)
            .collect()
    }

    /// `(user, mode, state, body, feed kind)` of every brief notification.
    fn notes(&self) -> Vec<(String, String, String, String, String)> {
        fixture::notifications(self.e.store())
            .unwrap()
            .into_iter()
            .filter(|(_, topic, ..)| topic == "briefing.ready")
            .map(|(user, _, mode, state, body, kind)| (user, mode, state, body, kind))
            .collect()
    }

    /// `(trigger_kind, status, outcome_label)` of a schedule's runs, oldest first.
    fn runs(&self, id: &str) -> Vec<(String, String, String)> {
        fixture::brief_runs(self.e.store(), id).unwrap()
    }
}

fn assert_plain(body: &str) {
    assert!(!body.contains("]("), "no markdown link: {body}");
    assert!(!body.contains("#/"), "no web fragment: {body}");
}

#[test]
fn a_members_first_own_todo_turns_their_brief_on_once_and_never_again_after_they_delete_it() {
    let e = env();
    // recorded by Steve from a message: not the member using their todos
    e.add(ME, "Steve 记的", |t| t.source = "steve");
    assert!(e.brief(ME).is_none());

    e.add(ME, "我自己记的", |_| {});
    let s = e.brief(ME).expect("turned on");
    assert_eq!(s.spec.mode, Mode::Brief);
    assert_eq!(s.spec.requester.as_deref(), Some(ME));
    assert_eq!(s.spec.cron, brief::DEFAULT_CRON);
    assert_eq!(s.spec.tz, "Asia/Taipei");
    assert!(s.enabled && s.spec.bot.is_empty());
    assert_eq!(s.name, "晨报");
    // its own: listed to nobody else
    assert!(s.visible_to(ME) && !s.visible_to(OTHER));
    // no flow trigger: the scheduler never runs it
    let (triggers, _) = fixture::schedule_counts(e.e.store(), &s.id).unwrap();
    assert_eq!(triggers, 0);

    e.add(ME, "第二个", |_| {});
    let (_, count) = fixture::schedule_counts(e.e.store(), &s.id).unwrap();
    assert_eq!(count, 1);

    e.e.store()
        .transaction(|tx| schedules::delete(tx, &s, now()))
        .unwrap();
    e.add(ME, "删了以后", |_| {});
    assert!(e.brief(ME).is_none(), "a deleted brief stays deleted");
}

#[test]
fn a_brief_fires_once_at_its_time_with_the_members_day_and_is_sent_under_their_policy() {
    let e = env();
    e.policy(ME, "Asia/Taipei", None);
    let today = "2027-02-03";
    e.add(ME, "交 Q4 复盘", |t| t.due_date = Some("2027-02-02"));
    e.add(ME, "回复陈默", |t| {
        t.due = Some(local(2027, 2, 3, 18, 0));
        t.star = true;
    });
    e.add(ME, "整理报销", |t| t.due_date = Some(today));
    e.add(ME, "推迟了的", |t| {
        t.due_date = Some(today);
        t.snooze_until = Some(local(2027, 2, 3, 15, 0));
    });
    e.add(ME, "读完 RFC", |t| t.star = true);
    e.add(ME, "下个月的", |t| t.due_date = Some("2027-03-01"));
    let goal = e.add(ME, "上线日历", |t| {
        t.kind = "goal";
        t.target_date = Some("2027-02-05");
    });
    e.e.store()
        .write(|tx| {
            todo::create(
                tx,
                &todo::NewTodo {
                    ws: WS,
                    owner: ME,
                    text: "写同步",
                    source: "you",
                    writer: ME,
                    parent: Some(&goal),
                    ..Default::default()
                },
                now(),
            )
        })
        .unwrap();
    e.add(OTHER, "别人的事", |t| t.due_date = Some(today));
    fixture::brief_items(e.e.store(), WS, ME, OTHER).unwrap();
    let s = e.brief(ME).unwrap();

    e.round(now());
    let at = local(2027, 2, 3, 8, 30);
    let planned = e.planned(&s.id);
    assert_eq!(planned.len(), brief::PLAN_AHEAD);
    assert_eq!(planned[0], at);
    // workdays only: Thursday, Friday, then Monday
    assert_eq!(planned[3], local(2027, 2, 8, 8, 30));

    assert_eq!(e.round(at - MIN).fired, 0);
    // theirs too: the other member's own todo turned their brief on
    assert_eq!(e.round(at).fired, 2);
    assert_eq!(e.round(at + HOUR).fired, 0);

    let notes = e.notes();
    assert_eq!(notes.len(), 2);
    let theirs = notes.iter().find(|n| n.0 == OTHER).unwrap();
    assert!(
        theirs.3.contains("别人的事") && theirs.3.contains("ST-3"),
        "{}",
        theirs.3
    );
    assert!(!theirs.3.contains("交 Q4 复盘"), "{}", theirs.3);
    let (user, mode, state, body, kind) = notes.iter().find(|n| n.0 == ME).unwrap();
    assert_eq!(user, ME);
    assert_eq!(kind, "briefing");
    // sent under the policy: waiting for the notification pump, which delivers through E1
    assert_eq!((mode.as_str(), state.as_str()), ("now", "waiting"));
    assert!(body.starts_with("晨报 · 02-03 周三\n"), "{body}");
    assert!(
        body.contains("今天到期 2 个待办，逾期 1 个，1 张待处理等你拍板，1 个事项卡住了。"),
        "{body}"
    );
    assert!(body.contains("逾期\n- 交 Q4 复盘 · 02-02"), "{body}");
    assert!(
        body.contains("今天到期\n- ★ 回复陈默 · 18:00\n- 整理报销"),
        "{body}"
    );
    assert!(body.contains("星标\n- ★ 读完 RFC"), "{body}");
    assert!(
        body.contains("本周目标\n- 上线日历 · 02-05 · 0/1"),
        "{body}"
    );
    assert!(body.contains("- ST-2 导出报表 · 失败"), "{body}");
    assert!(body.contains("进行中 1 个"), "{body}");
    for gone in ["推迟了的", "下个月的", "别人的事", "ST-3"] {
        assert!(!body.contains(gone), "{gone} is not in it: {body}");
    }
    assert_plain(body);

    // after it fired, it is planned again: today's stays fired, the rest of the week stays planned
    // (the day after the last one is added when tomorrow's fires)
    let planned = e.planned(&s.id);
    assert_eq!(planned.len(), brief::PLAN_AHEAD - 1);
    assert_eq!(planned[0], local(2027, 2, 4, 8, 30));
    assert_eq!(e.alarms(&s.id)[0], (at, "fired".into()));

    // its run on the schedule says what it did
    let runs = e.runs(&s.id);
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].0.as_str(), runs[0].1.as_str()), ("time", "done"));
    assert!(
        runs[0].2.starts_with("已发到私聊：今天到期 2 个待办"),
        "{}",
        runs[0].2
    );

    // the live view: the same until something changes
    let v =
        e.e.store()
            .read(|c| brief::today(c, WS, ME, at + HOUR))
            .unwrap();
    assert_eq!(v["sent"]["pushed"], true);
    assert_eq!(v["sent"]["changes"], 0);
    assert_eq!(v["schedule"]["id"], s.id.as_str());
    let first =
        e.e.store()
            .read(|r| {
                Ok(Todos(r)
                    .list(
                        WS,
                        ME,
                        &TodoFilter {
                            limit: 20,
                            ..Default::default()
                        },
                    )?
                    .into_iter()
                    .find(|todo| todo.fields.text == "整理报销")
                    .unwrap()
                    .id)
            })
            .unwrap();
    e.e.store()
        .write(|tx| todo::complete(tx, WS, &first, "done", None, at + HOUR))
        .unwrap();
    let v =
        e.e.store()
            .read(|c| brief::today(c, WS, ME, at + HOUR))
            .unwrap();
    assert_eq!(v["sent"]["changes"], 1);
}

#[test]
fn quiet_hours_hold_a_brief_and_after_noon_it_stays_in_the_feed() {
    let e = env();
    // quiet 08:00–13:00: the 08:30 brief waits until 13:00, past the noon push window
    e.policy(ME, "Asia/Taipei", Some((8 * 60, 13 * 60)));
    e.add(ME, "交周报", |t| t.due_date = Some("2027-02-03"));
    let s = e.brief(ME).unwrap();
    e.round(now());
    let at = local(2027, 2, 3, 8, 30);
    let c = e.round(at);
    assert_eq!((c.fired, c.held), (0, 1));
    assert!(e.notes().is_empty());
    let end = local(2027, 2, 3, 13, 0);
    assert_eq!(e.round(end).fired, 1);
    let notes = e.notes();
    assert_eq!(notes.len(), 1);
    assert_eq!(
        (notes[0].1.as_str(), notes[0].2.as_str()),
        ("log", "held"),
        "kept in the feed only"
    );
    let runs = e.runs(&s.id);
    assert_eq!(runs[0].0, "catch_up");
    assert_eq!(runs[0].2, "过了 12:00，只留在信息流");
}

#[test]
fn a_day_with_nothing_due_is_one_line_in_the_feed_and_not_sent() {
    let e = env();
    e.policy(ME, "Asia/Taipei", None);
    e.add(ME, "以后再说", |_| {});
    let s = e.brief(ME).unwrap();
    e.round(now());
    e.round(local(2027, 2, 3, 8, 30));
    let notes = e.notes();
    assert_eq!(notes.len(), 1);
    assert_eq!((notes[0].1.as_str(), notes[0].2.as_str()), ("log", "held"));
    assert_eq!(notes[0].3, "晨报 · 02-03 周三\n今天没有安排。");
    assert_eq!(e.runs(&s.id)[0].2, "今天没有安排，只留在信息流");
}

#[test]
fn without_a_policy_the_brief_is_in_the_feed_only() {
    let e = env();
    e.add(ME, "交周报", |t| t.due_date = Some("2027-02-03"));
    e.round(now());
    e.round(local(2027, 2, 3, 8, 30));
    let notes = e.notes();
    assert_eq!(notes.len(), 1);
    assert_eq!((notes[0].1.as_str(), notes[0].2.as_str()), ("log", "held"));
    assert!(notes[0].3.contains("今天到期 1 个待办"), "{}", notes[0].3);
}

#[test]
fn a_hub_down_for_days_sends_only_the_days_brief_once() {
    let e = env();
    e.policy(ME, "Asia/Taipei", None);
    e.add(ME, "交周报", |t| t.due_date = Some("2027-02-05"));
    let s = e.brief(ME).unwrap();
    e.round(now());
    // down from Wednesday 07:00 to Friday 09:00: Wednesday's and Thursday's are dropped, Friday's is late
    let back = local(2027, 2, 5, 9, 0);
    let c = e.round(back);
    assert_eq!((c.stale, c.fired), (2, 1));
    let notes = e.notes();
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].3.starts_with("晨报 · 02-05 周五"),
        "{}",
        notes[0].3
    );
    // before noon: still sent
    assert_eq!(notes[0].2, "waiting");
    assert_eq!(e.runs(&s.id)[0].0, "catch_up");
    // the next round does not send it again
    assert_eq!(e.round(back + MIN).fired, 0);
    assert_eq!(e.notes().len(), 1);
}

#[test]
fn a_moved_rezoned_or_switched_off_brief_is_planned_again() {
    let e = env();
    e.add(ME, "x", |_| {});
    let s = e.brief(ME).unwrap();
    e.round(now());
    assert_eq!(e.planned(&s.id)[0], local(2027, 2, 3, 8, 30));

    // moved to 09:15
    let mut spec = s.spec.clone();
    spec.cron = "15 9 * * 1-5".into();
    e.e.store()
        .transaction(|tx| {
            schedules::update(
                tx,
                &s,
                &Fields {
                    name: &s.name,
                    spec: &spec,
                    enabled: true,
                },
                ME,
                "",
                now(),
            )
        })
        .unwrap();
    e.round(now());
    assert_eq!(e.planned(&s.id)[0], local(2027, 2, 3, 9, 15));
    assert!(
        e.alarms(&s.id)
            .iter()
            .any(|(t, st)| *t == local(2027, 2, 3, 8, 30) && st == "cancelled")
    );

    // its owner's clock moves to Tokyo (an hour ahead): 09:15 there
    e.policy(ME, "Asia/Tokyo", None);
    e.round(now());
    assert_eq!(e.planned(&s.id)[0], local(2027, 2, 3, 9, 15) - HOUR);

    // a once-a-day rule: an hourly brief is refused
    let mut hourly = spec.clone();
    hourly.cron = "0 * * * *".into();
    let refused =
        e.e.store()
            .reader(|c| schedules::check_refs(c, WS, &hourly));
    assert!(refused.is_err());

    // switched off: nothing planned
    let s = e.brief(ME).unwrap();
    e.e.store()
        .transaction(|tx| {
            schedules::update(
                tx,
                &s,
                &Fields {
                    name: &s.name,
                    spec: &s.spec,
                    enabled: false,
                },
                ME,
                "",
                now(),
            )
        })
        .unwrap();
    e.round(now());
    assert!(e.planned(&s.id).is_empty());
}

#[test]
fn one_brief_per_member_and_day() {
    let e = env();
    e.add(ME, "交周报", |t| t.due_date = Some("2027-02-03"));
    let s = e.brief(ME).unwrap();
    // a second brief schedule of the same member, a little later
    let mut spec = s.spec.clone();
    spec.cron = "45 8 * * 1-5".into();
    e.e.store()
        .transaction(|tx| {
            schedules::insert(
                tx,
                WS,
                ME,
                &Fields {
                    name: "另一个",
                    spec: &spec,
                    enabled: true,
                },
                &schedules::Origin::default(),
                now(),
            )
        })
        .unwrap();
    e.round(now());
    e.round(local(2027, 2, 3, 8, 30));
    e.round(local(2027, 2, 3, 8, 45));
    assert_eq!(e.notes().len(), 1);
}
