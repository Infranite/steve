//! The calendar read foundation end to end, on synthetic data: the demo source, an ICS subscription
//! served on loopback, an uploaded .ics file, and a scripted source with cursor changes.

mod support;

use chrono::DateTime;
use serde_json::{Value, json};
use steve_calendar::profile::{
    Attendee, Changes, Person, RemoteCalendar, RemoteEvent, SourceError, When,
};
use steve_calendar::tools;
use steve_modules::contacts::mcp::McpCx;
use steve_modules::testkit::{ME, WS, caller};
use support::*;

fn at(s: &str) -> i64 {
    DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis()
}

fn titles(v: &Value) -> Vec<String> {
    v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap_or("").to_string())
        .collect()
}

fn find<'a>(v: &'a Value, title: &str) -> &'a Value {
    v["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["title"] == title)
        .unwrap_or_else(|| panic!("no {title} in {v}"))
}

fn cal_named<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["calendars"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no calendar {name} in {v}"))
}

fn ids_and_versions(h: &H) -> Vec<(String, i64)> {
    h.env
        .store()
        .reader(|c| {
            let mut st = c.prepare("SELECT id, version FROM cal_events ORDER BY id")?;
            let xs = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, steve_store::StoreError>(xs)
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn demo_calendar_mine_and_a_colleagues_free_busy() {
    let h = setup().await;
    // Wednesday morning in the reader's zone (the workspace default, UTC+8)
    h.env.clock.set(at("2026-11-18T08:00:00+08:00"));
    h.contact("ct_chen", "陈默", steve_calendar::sources::DEMO_COLLEAGUE);
    let conn = h.connect("cal_demo", &[("tz", "Asia/Shanghai")]).await;
    let r = h.cal.sync_connection(WS, &conn, false).await.unwrap();
    assert!(r.source && r.errors.is_empty(), "{r:?}");
    assert_eq!(r.calendars, 2);

    let (s, v) = h.call("GET", "/calendar/sources", None).await;
    assert_eq!(s, 200, "{v}");
    let src = &v[0];
    assert_eq!(src["adapter"], "cal_demo");
    let work = cal_named(src, "示例 · 工作");
    assert_eq!(
        (work["mine"].as_bool(), work["detail"].as_str()),
        (Some(true), Some("full"))
    );
    let coll = cal_named(src, "示例 · 同事");
    // the source names its owner: resolved to the contact, someone else's, times only
    assert_eq!(coll["party_id"], "ct_chen");
    assert_eq!(coll["party_name"], "陈默");
    assert_eq!(
        (coll["mine"].as_bool(), coll["detail"].as_str()),
        (Some(false), Some("busy"))
    );
    let coll_id = coll["id"].as_str().unwrap().to_string();
    let with_text: i64 = h.sql(
        "SELECT COUNT(*) FROM cal_events WHERE calendar_id = ? AND (title IS NOT NULL OR busy_only = 0)",
        &[&coll_id],
    );
    assert_eq!(
        with_text, 0,
        "nothing beyond free/busy is kept of someone else's calendar"
    );

    // tomorrow's agenda: the two overlapping meetings are marked, the colleague's blocks are not in it
    let (s, v) = h
        .call("GET", "/calendar/events?from=tomorrow&to=tomorrow", None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["range"]["from"], "2026-11-19 00:00");
    let t = titles(&v);
    assert!(
        t.contains(&"站会".to_string()) && t.contains(&"季度复盘".to_string()),
        "{t:?}"
    );
    let review = find(&v, "季度复盘");
    let design = find(&v, "评审会");
    assert_eq!(review["conflicts_with"], json!([design["id"]]));
    assert_eq!(review["start"], "2026-11-19 14:00");
    assert_eq!(review["meeting_url"], "https://meet.google.com/demo-review");
    assert_eq!(review["attendees"], 3);
    assert!(find(&v, "站会").get("conflicts_with").is_none());
    // the all-day event is on its own date only
    let (_, v) = h
        .call(
            "GET",
            "/calendar/events?from=2026-11-20&to=2026-11-20",
            None,
        )
        .await;
    let off = find(&v, "团建");
    assert_eq!(
        (
            off["all_day"].as_bool(),
            off["start"].as_str(),
            off["end"].as_str()
        ),
        (Some(true), Some("2026-11-20"), Some("2026-11-20"))
    );
    let (_, v) = h
        .call(
            "GET",
            "/calendar/events?from=2026-11-21&to=2026-11-21",
            None,
        )
        .await;
    assert!(!titles(&v).contains(&"团建".to_string()));

    // a resync changes nothing: the same ids, the same versions
    let before = ids_and_versions(&h);
    h.env.clock.advance(10 * 60_000);
    let r = h.cal.sync_connection(WS, &conn, false).await.unwrap();
    assert_eq!((r.changed, r.deleted), (0, 0), "{r:?}");
    assert_eq!(ids_and_versions(&h), before);
    // asked for by hand right after: the cache answers
    let (s, v) = h
        .call("POST", &format!("/calendar/sources/{conn}/sync"), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["report"]["skipped"], true);

    // free time with the colleague tomorrow: around the stand-up, their 10–12 and 14–15, our 14–15:30
    let (s, v) = h
        .call("POST", "/calendar/find-time", Some(json!({"people": ["陈默"], "duration_min": 60, "from": "2026-11-19T09:00", "to": "2026-11-19"})))
        .await;
    assert_eq!(s, 200, "{v}");
    let starts: Vec<&str> = v["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["start"].as_str().unwrap())
        .collect();
    assert_eq!(
        starts,
        ["2026-11-19 12:00", "2026-11-19 13:00", "2026-11-19 15:30"]
    );
    assert_eq!(v["checked"].as_array().unwrap().len(), 2);
    // someone without a calendar is named, not counted
    h.contact("ct_lin", "林舟", "lin@partner.example");
    let (_, v) = h
        .call("POST", "/calendar/find-time", Some(json!({"people": ["ct_lin"], "duration_min": 30, "from": "2026-11-19T09:00", "to": "2026-11-19"})))
        .await;
    assert_eq!(v["unknown"][0]["name"], "林舟", "{v}");
    assert_eq!(v["slots"][0]["start"], "2026-11-19 09:00");

    let (_, v) = h
        .call(
            "GET",
            "/calendar/freebusy?party=ct_chen&from=2026-11-19&to=2026-11-19",
            None,
        )
        .await;
    assert_eq!(v["known"], true);
    assert_eq!(v["busy"].as_array().unwrap().len(), 2, "{v}");
}

fn vcal(events: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Team calendar\r\nX-WR-TIMEZONE:Asia/Shanghai\r\n{events}END:VCALENDAR\r\n"
    )
}

const PLAN: &str = "BEGIN:VEVENT\r\nUID:plan-1\r\nSUMMARY:Planning\r\nDESCRIPTION:Agenda first\r\nLOCATION:Room 1\r\nDTSTART;TZID=Asia/Shanghai:20261120T100000\r\nDTEND;TZID=Asia/Shanghai:20261120T110000\r\nEND:VEVENT\r\n";
const WEEKLY: &str = "BEGIN:VEVENT\r\nUID:weekly-1\r\nSUMMARY:Weekly sync\r\nDTSTART;TZID=Asia/Shanghai:20261116T160000\r\nDURATION:PT30M\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nEND:VEVENT\r\n";

#[tokio::test(flavor = "multi_thread")]
async fn ics_subscription_versions_deletes_and_keeps_its_cache_on_failure() {
    let h = setup().await;
    h.env.clock.set(at("2026-11-18T08:00:00+08:00"));
    let srv = IcsServer::start(&vcal(&format!("{PLAN}{WEEKLY}"))).await;
    // an address that is no calendar is refused when connecting
    srv.set(200, "not a calendar");
    let f: std::collections::BTreeMap<String, zeroize::Zeroizing<String>> =
        [("url".to_string(), zeroize::Zeroizing::new(srv.url.clone()))].into();
    assert!(h.conns.create(&me(), "ics", &f, json!({})).await.is_err());
    srv.set(200, &vcal(&format!("{PLAN}{WEEKLY}")));
    let conn = h.connect("ics", &[("url", srv.url.as_str())]).await;
    let r = h.cal.sync_connection(WS, &conn, false).await.unwrap();
    assert!(r.errors.is_empty(), "{r:?}");
    let (_, v) = h
        .call(
            "GET",
            "/calendar/events?from=2026-11-16&to=2026-12-31",
            None,
        )
        .await;
    assert_eq!(
        titles(&v).iter().filter(|t| *t == "Weekly sync").count(),
        4,
        "{v}"
    );
    let plan = find(&v, "Planning");
    assert_eq!(plan["start"], "2026-11-20 10:00");
    assert_eq!(plan["calendar"]["name"], "Team calendar");
    let id = plan["id"].as_str().unwrap().to_string();
    let version = |h: &H| -> i64 { h.sql("SELECT version FROM cal_events WHERE id = ?", &[&id]) };

    // a new description: not a change that matters
    srv.set(
        200,
        &vcal(&format!(
            "{}{WEEKLY}",
            PLAN.replace("Agenda first", "Agenda, then notes")
        )),
    );
    h.env.clock.advance(6 * 60_000);
    h.cal.sync_connection(WS, &conn, false).await.unwrap();
    assert_eq!(version(&h), 1);
    let excerpt: String = h.sql("SELECT excerpt FROM cal_events WHERE id = ?", &[&id]);
    assert_eq!(excerpt, "Agenda, then notes");
    // a new time: version 2, the same id
    srv.set(
        200,
        &vcal(&format!(
            "{}{WEEKLY}",
            PLAN.replace("T100000", "T140000")
                .replace("T110000", "T150000")
        )),
    );
    h.env.clock.advance(6 * 60_000);
    let r = h.cal.sync_connection(WS, &conn, false).await.unwrap();
    assert_eq!(r.changed, 1, "{r:?}");
    assert_eq!(version(&h), 2);
    // the source fails: that calendar says so, its cache stays
    srv.set(500, "boom");
    h.env.clock.advance(6 * 60_000);
    let r = h.cal.sync_connection(WS, &conn, false).await.unwrap();
    assert_eq!(r.errors.len(), 1, "{r:?}");
    let (_, v) = h
        .call("GET", &format!("/calendar/sources/{conn}"), None)
        .await;
    assert_eq!(v["calendars"][0]["sync_state"], "error", "{v}");
    assert!(v["calendars"][0]["synced_at"].is_i64());
    let (_, v) = h
        .call(
            "GET",
            "/calendar/events?from=2026-11-20&to=2026-11-20",
            None,
        )
        .await;
    assert_eq!(find(&v, "Planning")["start"], "2026-11-20 14:00");
    // back, without the planning meeting: it is deleted, the row stays for links
    srv.set(200, &vcal(WEEKLY));
    h.env.clock.advance(6 * 60_000);
    let r = h.cal.sync_connection(WS, &conn, false).await.unwrap();
    assert_eq!(r.deleted, 1, "{r:?}");
    let (_, v) = h
        .call(
            "GET",
            "/calendar/events?from=2026-11-20&to=2026-11-20",
            None,
        )
        .await;
    assert!(!titles(&v).contains(&"Planning".to_string()));
    let (s, v) = h.call("GET", &format!("/calendar/events/{id}"), None).await;
    assert_eq!((s, v["deleted"].as_bool()), (200, Some(true)), "{v}");
    let (_, v) = h.call("GET", "/calendar/search?q=weekly", None).await;
    assert_eq!(titles(&v).len(), 4, "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ics_file_upload_and_only_the_owner_reads() {
    let h = setup().await;
    h.env.clock.set(at("2026-11-18T08:00:00+08:00"));
    let conn = h.connect("ics", &[("name", "Holidays")]).await;
    let (s, v) = h
        .call("GET", &format!("/calendar/sources/{conn}"), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["ics"]["subscribed"], false);
    assert_eq!(v["calendars"], json!([]));
    let (s, _) = h
        .call(
            "PUT",
            &format!("/calendar/sources/{conn}/ics"),
            Some(json!({"text": "hello"})),
        )
        .await;
    assert_eq!(s, 400);
    let body = vcal(
        "BEGIN:VEVENT\r\nUID:h1\r\nSUMMARY:Day off\r\nDTSTART;VALUE=DATE:20261123\r\nDTEND;VALUE=DATE:20261125\r\nEND:VEVENT\r\n",
    );
    let (s, v) = h
        .call(
            "PUT",
            &format!("/calendar/sources/{conn}/ics"),
            Some(json!({"name": "holidays.ics", "text": body})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["ics"]["file"], "holidays.ics");
    assert_eq!(v["calendars"][0]["name"], "Holidays");
    let (_, v) = h
        .call(
            "GET",
            "/calendar/events?from=2026-11-24&to=2026-11-24",
            None,
        )
        .await;
    let off = find(&v, "Day off");
    assert_eq!(
        (off["start"].as_str(), off["end"].as_str()),
        (Some("2026-11-23"), Some("2026-11-24"))
    );
    // another member sees none of it
    let other = caller(WS, "usr_other");
    let (s, _) = h
        .call_as(&other, "GET", &format!("/calendar/sources/{conn}"), None)
        .await;
    assert_eq!(s, 404);
    let (_, v) = h
        .call_as(
            &other,
            "GET",
            "/calendar/events?from=2026-11-24&to=2026-11-24",
            None,
        )
        .await;
    assert_eq!(v["events"], json!([]));
    let cal = v["calendars"].clone();
    assert_eq!(cal, json!([]));
    let id = off["id"].as_str().unwrap();
    let (s, _) = h
        .call_as(&other, "GET", &format!("/calendar/events/{id}"), None)
        .await;
    assert_eq!(s, 404);
    // turning a source's cal.read off makes it no source
    let (s, _) = h
        .call(
            "PATCH",
            &format!("/calendar/calendars/{}", "cal_nope"),
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(s, 404);
}

fn ev(id: &str, uid: &str, start: &str, mins: i64, title: &str) -> RemoteEvent {
    let s = at(start);
    RemoteEvent {
        id: id.into(),
        ical_uid: Some(uid.into()),
        title: Some(title.into()),
        start: When::at(s),
        end: When::at(s + mins * 60_000),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cursor_changes_one_meeting_in_two_calendars_settings_and_tools() {
    let h = setup().await;
    h.env.clock.set(at("2026-11-18T08:00:00+08:00"));
    h.contact("ct_ada", "Ada", "ada@example.com");
    *h.script.calendars.lock().unwrap() = vec![
        RemoteCalendar {
            id: "work".into(),
            name: "Work".into(),
            role: Some("owner".into()),
            ..Default::default()
        },
        RemoteCalendar {
            id: "inv".into(),
            name: "Invites".into(),
            role: Some("reader".into()),
            ..Default::default()
        },
    ];
    let mut shared = ev(
        "w-1",
        "u1",
        "2026-11-19T10:00:00+08:00",
        60,
        "Design review",
    );
    shared.can_modify = true;
    shared.organizer = Some(Person {
        email: Some("ada@example.com".into()),
        ..Default::default()
    });
    shared.attendees = vec![
        Attendee {
            who: Person {
                me: true,
                ..Default::default()
            },
            rsvp: Some("accepted".into()),
            ..Default::default()
        },
        Attendee {
            who: Person {
                email: Some("ADA@example.com".into()),
                name: Some("Ada".into()),
                ..Default::default()
            },
            organizer: true,
            ..Default::default()
        },
    ];
    let mut invite = ev(
        "i-9",
        "u1",
        "2026-11-19T10:00:00+08:00",
        60,
        "Design review",
    );
    invite.description = Some("Join https://acme.zoom.us/j/42 please".into());
    // calendars are synced by name: Invites, then Work
    *h.script.changes.lock().unwrap() = vec![
        Changes {
            events: vec![invite.clone()],
            cursor: Some("inv-1".into()),
            ..Default::default()
        },
        Changes {
            events: vec![
                shared.clone(),
                ev("w-2", "u2", "2026-11-19T15:00:00+08:00", 30, "Focus"),
            ],
            cursor: Some("work-1".into()),
            ..Default::default()
        },
    ];
    let conn = h.connect("cal_script", &[]).await;
    let r = h.cal.sync_connection(WS, &conn, false).await.unwrap();
    assert!(r.errors.is_empty(), "{r:?}");
    let (_, v) = h
        .call(
            "GET",
            "/calendar/events?from=2026-11-19&to=2026-11-19",
            None,
        )
        .await;
    assert_eq!(
        titles(&v),
        ["Design review", "Focus"],
        "one meeting, shown once: {v}"
    );
    let review = find(&v, "Design review");
    assert_eq!(
        review["calendar"]["name"], "Work",
        "the one the owner may change"
    );
    assert_eq!(review["also_in"], json!(["Invites"]));
    assert_eq!(review["organizer"]["party_id"], "ct_ada");
    assert_eq!(review["my_rsvp"], "accepted");
    let meeting = review["meeting_id"].as_str().unwrap().to_string();
    let (s, one) = h
        .call("GET", &format!("/calendar/events/{meeting}"), None)
        .await;
    assert_eq!(s, 200, "{one}");
    assert_eq!(one["attendees"][1]["party_id"], "ct_ada");
    assert_eq!(one["attendees"][0]["is_self"], true);

    // the next sync asks with the cursors; a delta deletes one and moves another
    *h.script.changes.lock().unwrap() = vec![
        Changes {
            deleted: vec!["i-9".into()],
            cursor: Some("inv-2".into()),
            ..Default::default()
        },
        Changes {
            events: vec![ev("w-2", "u2", "2026-11-19T16:00:00+08:00", 30, "Focus")],
            cursor: Some("work-2".into()),
            ..Default::default()
        },
    ];
    h.env.clock.advance(6 * 60_000);
    let r = h.cal.sync_connection(WS, &conn, false).await.unwrap();
    assert_eq!((r.changed, r.deleted), (1, 1), "{r:?}");
    assert_eq!(
        *h.script.cursors.lock().unwrap(),
        [None, None, Some("inv-1".into()), Some("work-1".into())]
    );
    let (_, v) = h
        .call(
            "GET",
            "/calendar/events?from=2026-11-19&to=2026-11-19",
            None,
        )
        .await;
    assert!(find(&v, "Design review").get("also_in").is_none());
    assert_eq!(find(&v, "Focus")["start"], "2026-11-19 16:00");
    assert_eq!(find(&v, "Focus")["version"], 2);
    // the source lost the cursor: everything comes again and replaces the window
    *h.script.changes.lock().unwrap() = vec![
        Changes {
            full: true,
            cursor: Some("inv-3".into()),
            ..Default::default()
        },
        Changes {
            full: true,
            events: vec![shared.clone()],
            cursor: Some("work-3".into()),
            ..Default::default()
        },
    ];
    h.env.clock.advance(6 * 60_000);
    let r = h.cal.sync_connection(WS, &conn, false).await.unwrap();
    assert_eq!(r.deleted, 1, "Focus is gone: {r:?}");

    // the tools read the same cache, as the session's own user
    let cx = McpCx {
        ws: WS.into(),
        user: ME.into(),
        bot_id: "agent".into(),
        item_id: None,
        session_id: Some("s1".into()),
    };
    let out: Value = serde_json::from_str(
        &tools::call(
            &h.cal,
            &cx,
            tools::AGENDA,
            &json!({"from": "2026-11-19", "to": "2026-11-19"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(titles(&out), ["Design review"]);
    assert_eq!(out["range"]["tz"], "Asia/Taipei");
    let out: Value = serde_json::from_str(
        &tools::call(&h.cal, &cx, tools::SEARCH, &json!({"query": "design"}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(titles(&out).len(), 1);
    assert!(
        tools::call(&h.cal, &cx, tools::AGENDA, &json!({"from": "next friday"}))
            .await
            .unwrap_err()
            .contains("看不懂")
    );
    // a live read takes the source's current version and its body
    let mut fresh = shared.clone();
    fresh.location = Some("Room 7".into());
    fresh.description = Some("Bring the numbers".into());
    *h.script.get.lock().unwrap() = Some(fresh);
    let out: Value = serde_json::from_str(
        &tools::call(&h.cal, &cx, tools::GET, &json!({"event": meeting}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(out["live"], true, "{out}");
    assert_eq!(
        (out["location"].as_str(), out["description"].as_str()),
        (Some("Room 7"), Some("Bring the numbers"))
    );
    let other = McpCx {
        user: "usr_other".into(),
        ..cx.clone()
    };
    assert!(
        tools::call(&h.cal, &other, tools::GET, &json!({"event": meeting}))
            .await
            .is_err()
    );
    let out: Value = serde_json::from_str(
        &tools::call(
            &h.cal,
            &cx,
            tools::FIND_TIME,
            &json!({"duration_min": 30, "from": "2026-11-19T09:00", "to": "2026-11-19"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(out["slots"][0]["start"], "2026-11-19 09:00", "{out}");

    // the Work calendar is Ada's after all: only its times are kept from now on
    let (_, v) = h
        .call("GET", &format!("/calendar/sources/{conn}"), None)
        .await;
    let work = cal_named(&v, "Work")["id"].as_str().unwrap().to_string();
    let (s, v) = h
        .call(
            "PATCH",
            &format!("/calendar/calendars/{work}"),
            Some(json!({"party_id": "ct_ada"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["detail"].as_str(),
            v["mine"].as_bool(),
            v["party_set"].as_bool()
        ),
        (Some("busy"), Some(false), Some(true))
    );
    let kept: i64 = h.sql(
        "SELECT COUNT(*) FROM cal_events WHERE calendar_id = ? AND title IS NOT NULL",
        &[&work],
    );
    assert_eq!(kept, 0);
    let (_, v) = h
        .call(
            "GET",
            "/calendar/freebusy?party=Ada&from=2026-11-19&to=2026-11-19",
            None,
        )
        .await;
    assert_eq!(v["busy"].as_array().map(Vec::len), Some(1), "{v}");
    // busy only now: a live read does not fetch details; gone at the source, it is deleted
    let out: Value = serde_json::from_str(
        &tools::call(&h.cal, &cx, tools::GET, &json!({"event": meeting}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        (out["live"].as_bool(), out["title"].is_null()),
        (Some(false), true),
        "{out}"
    );
    let (s, _) = h
        .call(
            "PATCH",
            &format!("/calendar/calendars/{work}"),
            Some(json!({"detail": "loud"})),
        )
        .await;
    assert_eq!(s, 400);
    let (s, _) = h
        .call_as(
            &caller(WS, "usr_other"),
            "PATCH",
            &format!("/calendar/calendars/{work}"),
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(s, 404);

    // a source that fails marks its calendars and keeps them
    *h.script.fail.lock().unwrap() = Some(SourceError::Transient("down".into()));
    h.env.clock.advance(6 * 60_000);
    let r = h.cal.sync_connection(WS, &conn, false).await.unwrap();
    assert_eq!(r.errors.len(), 1);
    let errs: i64 = h.sql(
        "SELECT COUNT(*) FROM cal_calendars WHERE connection_id = ? AND sync_state = 'error'",
        &[&conn],
    );
    assert_eq!(errs, 2);
    // disconnecting drops everything of it
    h.conns.disconnect(&me(), &conn).await.unwrap();
    let left: i64 = h.sql(
        "SELECT (SELECT COUNT(*) FROM cal_calendars) + (SELECT COUNT(*) FROM cal_events)",
        &[],
    );
    assert_eq!(left, 0);
}
