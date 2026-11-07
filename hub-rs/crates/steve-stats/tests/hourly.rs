//! “今天” per hour (ST-43): the hours of a day add up to the day, the drill-down of an hour lists exactly the
//! items its bar counts, hours not yet come are empty rather than 0, and the edges of the day stay on their
//! side of midnight. The workspace's `stats.tz` is Asia/Shanghai (UTC+8), so local midnight is 16:00 UTC.

use std::collections::BTreeMap;

use steve_stats::metrics::{self, Daily, Query, bounds, summary};
use steve_stats::views::{self, Bucket, Metric, Series};
use steve_stats::{refresh, tz};
use steve_store::{
    ModelUsage, NewBot, NewMark, NewUsage, NoPublisher, Store, Tokens, TurnBegin, TurnFinish, WS,
};

const DAY: &str = "2026-11-05";
const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;

fn zone() -> Option<chrono_tz::Tz> {
    tz(Some("Asia/Shanghai"))
}

/// local midnight starting [`DAY`], and the next one
fn edges() -> (i64, i64) {
    bounds(DAY, DAY, zone()).expect("day")
}

fn q(day: &str) -> Query {
    Query {
        workspace_id: WS.into(),
        from: day.into(),
        to: day.into(),
        ..Query::default()
    }
}

fn gateway(s: &Store, ts: i64, model: &str, t: Tokens) {
    s.stats()
        .usage_add(&NewUsage {
            workspace_id: WS.into(),
            ts,
            day: steve_stats::day(ts, zone()),
            source: "gateway".into(),
            purpose: "route".into(),
            harness: "claude-cli".into(),
            turn_id: None,
            session_id: None,
            item_id: None,
            bot_id: None,
            project_id: None,
            usage: ModelUsage {
                model: model.into(),
                tokens: t,
                ..ModelUsage::default()
            },
            duration_ms: None,
            ok: true,
            attempts: 1,
        })
        .expect("usage");
}

fn turn(s: &Store, started: i64, ended: i64, input: i64) {
    let id = s
        .stats()
        .turn_begin(&TurnBegin {
            workspace_id: WS.into(),
            session_id: "s1".into(),
            bot_id: Some("dev".into()),
            harness: "claude-code".into(),
            started,
            ..TurnBegin::default()
        })
        .expect("begin");
    s.stats()
        .turn_finish(&TurnFinish {
            turn_id: id,
            ended,
            day: steve_stats::day(ended, zone()),
            model: "claude-opus-5-5".into(),
            stop_reason: Some("end_turn".into()),
            tools: 2,
            usage: vec![ModelUsage {
                model: "claude-opus-5-5".into(),
                tokens: Tokens {
                    input: Some(input),
                    output: Some(20),
                    ..Tokens::default()
                },
                ..ModelUsage::default()
            }],
            ..TurnFinish::default()
        })
        .expect("finish");
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tmp");
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).expect("open");
    s.ensure_workspace().expect("ws");
    s.bots()
        .seed(
            "dev",
            false,
            &NewBot {
                name: "开发".into(),
                harness: "claude-code".into(),
                model: Some("claude-opus-5-5".into()),
                ..Default::default()
            },
            WS,
        )
        .expect("bot");
    let (lo, hi) = edges();
    // (no, created, done): the last millisecond of yesterday, the first of today, the last of today, the
    // first of tomorrow
    let items = [
        (1, lo - 1, Some(lo)),
        (2, lo, Some(lo + 13 * HOUR + 20 * MIN)),
        (3, lo + 13 * HOUR + 5 * MIN, Some(hi - 1)),
        (4, hi - 1, Some(hi)),
        (5, hi, None),
    ];
    let mut sql = String::from(
        "INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'stats.tz', '\"Asia/Shanghai\"', 0);
         INSERT INTO sessions (id, workspace_id, bot_id, harness, cwd, status, created, updated) VALUES ('s1', 'default', 'dev', 'claude-code', '/tmp', 'idle', 0, 0);\n",
    );
    for (no, created, done) in items {
        let status = if done.is_some() { "done" } else { "run" };
        sql.push_str(&format!(
            "INSERT INTO items (id, workspace_id, no, title, status, stage, bot_id, created, updated) VALUES ('ST-{no}', 'default', {no}, 't{no}', '{status}', '执行', 'dev', {created}, {created});\n"
        ));
    }
    s.with_conn(|c| Ok(c.execute_batch(&sql)?)).expect("seed");
    for (no, created, done) in items {
        let mark = |ts: i64, from: Option<&str>, to: &str| {
            s.stats()
                .mark_add(&NewMark {
                    workspace_id: WS.into(),
                    item_id: format!("ST-{no}"),
                    ts,
                    kind: "status".into(),
                    from_v: from.map(str::to_string),
                    to_v: Some(to.into()),
                    actor: "dev".into(),
                    ..NewMark::default()
                })
                .expect("mark");
        };
        mark(created, None, "run");
        if let Some(d) = done {
            mark(d, Some("run"), "done");
        }
    }
    let t = |input: Option<i64>, output: Option<i64>, cache_read: Option<i64>| Tokens {
        input,
        output,
        cache_read,
        ..Tokens::default()
    };
    gateway(&s, lo - 1, "claude-opus-5-5", t(Some(100), None, None));
    gateway(&s, lo, "claude-opus-5-5", t(Some(1_000), None, Some(3_000)));
    gateway(
        &s,
        lo + 13 * HOUR + 30 * MIN,
        "claude-opus-5-5",
        t(None, Some(500), None),
    );
    gateway(&s, hi - 1, "gpt-9", t(Some(7), None, None));
    gateway(&s, hi, "claude-opus-5-5", t(Some(9), None, None));
    // a turn across midnight belongs to the hour it ended in
    turn(&s, lo - 10 * MIN, lo + 10 * MIN, 50);
    turn(&s, lo + 8 * HOUR + 50 * MIN, lo + 9 * HOUR + 10 * MIN, 200);
    refresh::refresh_all(&s).expect("refresh");
    (dir, s)
}

fn hourly(s: &Store, day: &str, metric: Metric, group: &str, now: i64) -> Series {
    s.with_conn(|c| views::series(c, &q(day), metric, group, Bucket::Hour { now }))
        .expect("series")
}

/// a line's values, `None` as -1 so they compare easily
fn vals(s: &Series, line: usize) -> Vec<f64> {
    s.series[line]
        .values
        .iter()
        .map(|v| v.unwrap_or(-1.0))
        .collect()
}

fn at(pairs: &[(usize, f64)]) -> Vec<f64> {
    let mut v = vec![0.0; 24];
    for (i, x) in pairs {
        v[*i] = *x;
    }
    v
}

fn sum(s: &Series) -> f64 {
    s.series
        .iter()
        .flat_map(|l| l.values.iter().flatten())
        .sum()
}

/// the measures of a set of cube rows by everything but the day
fn totals(rows: Vec<Daily>) -> BTreeMap<String, [i64; 9]> {
    let mut m: BTreeMap<String, [i64; 9]> = BTreeMap::new();
    for d in rows {
        let k = format!(
            "{}|{}|{}|{}|{}|{}",
            d.source, d.purpose, d.harness, d.model, d.bot_id, d.project_id
        );
        let e = m.entry(k).or_default();
        for (i, v) in [
            d.input,
            d.output,
            d.cache_read,
            d.cache_write,
            d.hit_base,
            d.calls,
            d.turns,
            d.turn_ms,
            d.tools,
        ]
        .into_iter()
        .enumerate()
        {
            e[i] += v;
        }
    }
    m
}

#[test]
fn hours_add_up_to_the_day() {
    let (_d, s) = store();
    let (_, hi) = edges();
    let later = hi + HOUR; // the whole day is past
    let sm = s
        .with_conn(|c| summary(c, &q(DAY)))
        .expect("summary")
        .expect("day");

    // the same measures, by hour, as the cube has for the day
    let cube = s.with_conn(|c| metrics::daily(c, &q(DAY))).expect("daily");
    let hours = s
        .with_conn(|c| metrics::hourly(c, &q(DAY)))
        .expect("hourly");
    assert!(hours.iter().all(|d| d.day.starts_with("2026-11-05T")));
    assert_eq!(totals(hours), totals(cube));

    let tokens = hourly(&s, DAY, Metric::Tokens, "none", later);
    assert_eq!(tokens.bucket, "hour");
    assert_eq!(tokens.buckets.len(), 24);
    assert_eq!(tokens.buckets[13], "2026-11-05T13:00");
    assert_eq!(tokens.current, None);
    // 00: the gateway call at midnight (1000 + 3000) and the turn that ended at 00:10 (50 + 20)
    assert_eq!(
        vals(&tokens, 0),
        at(&[(0, 4_070.0), (9, 220.0), (13, 500.0), (23, 7.0)])
    );
    assert_eq!(sum(&tokens), sm.usage.total as f64);

    let by_type = hourly(&s, DAY, Metric::Tokens, "token_type", later);
    assert_eq!(sum(&by_type), sm.usage.total as f64);
    let by_model = hourly(&s, DAY, Metric::Tokens, "model", later);
    assert_eq!(sum(&by_model), sm.usage.total as f64);
    let cost = hourly(&s, DAY, Metric::Cost, "none", later);
    assert!(
        (sum(&cost) - sm.usage.est).abs() < 1e-9,
        "{} vs {}",
        sum(&cost),
        sm.usage.est
    );
    let turns = hourly(&s, DAY, Metric::Turns, "none", later);
    assert_eq!(vals(&turns, 0), at(&[(0, 1.0), (9, 1.0)]));
    assert_eq!(sum(&turns), sm.agents.turns as f64);
    // a rate per hour: 3000 of 4000 at 00, nothing measurable elsewhere
    let hit = hourly(&s, DAY, Metric::HitRate, "none", later);
    assert_eq!(hit.series[0].values[0], Some(0.75));
    assert!(hit.series[0].values[1..].iter().all(Option::is_none));

    // the items: created and done at the first and last millisecond of the day count today
    let created = hourly(&s, DAY, Metric::Created, "none", later);
    let done = hourly(&s, DAY, Metric::Done, "none", later);
    assert_eq!(vals(&created, 0), at(&[(0, 1.0), (13, 1.0), (23, 1.0)]));
    assert_eq!(vals(&done, 0), at(&[(0, 1.0), (13, 1.0), (23, 1.0)]));
    assert_eq!(sum(&created), sm.throughput.created as f64);
    assert_eq!(sum(&done), sm.throughput.done as f64);

    // each hour's drill-down is exactly the items of its bar
    let ids = |only: &str, hour: Option<u32>| -> Vec<String> {
        let mut v: Vec<String> = s
            .with_conn(|c| views::items(c, &q(DAY), "est", Some(only), hour, 100, 0))
            .expect("items")
            .rows
            .into_iter()
            .map(|r| r.item_id)
            .collect();
        v.sort();
        v
    };
    for (only, line) in [("created", &created), ("done", &done)] {
        let mut all = vec![];
        for h in 0..24u32 {
            let xs = ids(only, Some(h));
            assert_eq!(
                xs.len() as f64,
                line.series[0].values[h as usize].unwrap_or(0.0),
                "{only} {h}"
            );
            all.extend(xs);
        }
        all.sort();
        assert_eq!(all, ids(only, None), "{only}");
    }
    assert_eq!(ids("done", Some(0)), ["ST-1"]);
    assert_eq!(ids("done", Some(23)), ["ST-3"]);
    assert_eq!(ids("created", Some(23)), ["ST-4"]);

    // the lead time of the hour ST-2 was done in: 13 h 20 min
    let lead = hourly(&s, DAY, Metric::Lead, "none", later);
    assert_eq!(
        lead.series[0].values[13],
        Some((13 * HOUR + 20 * MIN) as f64)
    );
    assert_eq!(lead.series[0].values[5], None);
}

#[test]
fn hours_not_yet_come_are_empty() {
    let (_d, s) = store();
    let (lo, _) = edges();
    let now = lo + 13 * HOUR + 45 * MIN;
    let tokens = hourly(&s, DAY, Metric::Tokens, "none", now);
    assert_eq!(tokens.current, Some(13));
    let spans = tokens.spans.clone().unwrap_or_default();
    assert_eq!(spans[13], [lo + 13 * HOUR, lo + 14 * HOUR]);
    // up to now as it stands, the current hour so far, nothing after it (not 0)
    assert_eq!(tokens.series[0].values[13], Some(500.0));
    assert_eq!(tokens.series[0].values[12], Some(0.0));
    assert!(tokens.series[0].values[14..].iter().all(Option::is_none));
    let wip = hourly(&s, DAY, Metric::Wip, "none", now);
    assert!(wip.series[0].values[..=13].iter().all(Option::is_some));
    assert!(wip.series[0].values[14..].iter().all(Option::is_none));
    // before the day starts, every hour is to come
    let early = hourly(&s, DAY, Metric::Done, "none", lo - 1);
    assert_eq!(early.current, None);
    assert!(early.series[0].values.iter().all(Option::is_none));
}

#[test]
fn the_edges_of_the_day_stay_on_their_side() {
    let (_d, s) = store();
    let (_, hi) = edges();
    let later = hi + 2 * 86_400_000;
    let y = hourly(&s, "2026-11-04", Metric::Tokens, "none", later);
    assert_eq!(vals(&y, 0), at(&[(23, 100.0)]));
    let y = hourly(&s, "2026-11-04", Metric::Created, "none", later);
    assert_eq!(vals(&y, 0), at(&[(23, 1.0)]));
    let t = hourly(&s, "2026-11-06", Metric::Tokens, "none", later);
    assert_eq!(vals(&t, 0), at(&[(0, 9.0)]));
    let t = hourly(&s, "2026-11-06", Metric::Done, "none", later);
    assert_eq!(vals(&t, 0), at(&[(0, 1.0)]));

    // an hour only exists in a one-day range
    let week = Query {
        from: "2026-11-01".into(),
        ..q(DAY)
    };
    assert_eq!(views::window(&week, Some(3), zone()), None);
    assert_eq!(views::window(&q(DAY), Some(24), zone()), None);
    let (lo, _) = edges();
    assert_eq!(
        views::window(&q(DAY), Some(0), zone()),
        Some((lo, lo + HOUR))
    );
    assert_eq!(
        views::window(&q(DAY), Some(23), zone()),
        Some((hi - HOUR, hi))
    );
}

/// `(day, source, purpose, harness, model, bot, turns, turn_ms, interrupted, calls, input)` of a cell.
type CubeRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    i64,
);

/// `stat_daily` as the live writer left it, one [`CubeRow`] per cell.
fn cube(s: &Store) -> Vec<CubeRow> {
    s.with_conn(|c| {
        let mut st = c.prepare(
            "SELECT day, source, purpose, harness, model, bot_id, turns, turn_ms, interrupted, calls, input FROM stat_daily
              WHERE workspace_id = 'default' ORDER BY day, source, purpose, harness, model, bot_id, project_id",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .expect("cube")
}

#[test]
fn a_rebuild_books_every_turn_where_the_live_writer_did() {
    let (_d, s) = store();
    let (lo, _) = edges();
    // the runner reported another model than its first token record's
    let id = s
        .stats()
        .turn_begin(&TurnBegin {
            workspace_id: WS.into(),
            session_id: "s1".into(),
            bot_id: Some("dev".into()),
            harness: "claude-code".into(),
            started: lo + 2 * HOUR,
            ..TurnBegin::default()
        })
        .expect("begin");
    s.stats()
        .turn_finish(&TurnFinish {
            turn_id: id,
            ended: lo + 2 * HOUR + 5 * MIN,
            day: steve_stats::day(lo + 2 * HOUR + 5 * MIN, zone()),
            model: "claude-sonnet-5".into(),
            stop_reason: Some("end_turn".into()),
            usage: vec![ModelUsage {
                model: "claude-haiku-5".into(),
                tokens: Tokens {
                    input: Some(3),
                    ..Tokens::default()
                },
                ..ModelUsage::default()
            }],
            ..TurnFinish::default()
        })
        .expect("finish");
    // a turn the Hub's restart cut: the live writer books it under no model
    s.stats()
        .turn_begin(&TurnBegin {
            workspace_id: WS.into(),
            session_id: "s1".into(),
            bot_id: Some("dev".into()),
            harness: "claude-code".into(),
            started: lo + 3 * HOUR,
            ..TurnBegin::default()
        })
        .expect("begin");
    s.stats()
        .interrupt_open(lo + 3 * HOUR + MIN, |_, ts| steve_stats::day(ts, zone()))
        .expect("interrupt");
    let live = cube(&s);
    assert!(
        live.iter()
            .any(|r| r.1 == "agent" && r.4.is_empty() && r.8 == 1),
        "{live:?}"
    );
    assert!(
        live.iter()
            .any(|r| r.1 == "agent" && r.2 == "turn" && r.4 == "claude-sonnet-5" && r.6 == 1)
    );
    s.with_conn(|c| refresh::rebuild_in(c, WS))
        .expect("rebuild");
    assert_eq!(cube(&s), live, "the rebuild moved turns to other models");
    // and the hours of the day add up to the same cells
    let hours = s
        .with_conn(|c| metrics::hourly(c, &q(DAY)))
        .expect("hourly");
    let turns_by_model = |model: &str| -> i64 {
        hours
            .iter()
            .filter(|d| d.source == "agent" && d.purpose == "turn" && d.model == model)
            .map(|d| d.turns)
            .sum()
    };
    assert_eq!(turns_by_model("claude-sonnet-5"), 1);
    assert_eq!(turns_by_model(""), 1);
}
