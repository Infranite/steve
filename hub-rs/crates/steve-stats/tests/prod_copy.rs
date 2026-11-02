//! The backfill on a backup of the production database. Skipped unless `STEVE_STATS_PROD_COPY` points at one:
//!
//!   sqlite3 -readonly ~/.steve/data/steve.sqlite3 ".backup '/tmp/x/steve.sqlite3'"
//!   STEVE_STATS_PROD_COPY=/tmp/x/steve.sqlite3 cargo test -p steve-stats --test prod_copy -- --nocapture
//!
//! The test backs the given copy up again into its own temp dir, so the given file is never written.

use std::path::Path;
use std::process::Command;

use steve_stats::metrics::{Query, summary};
use steve_store::{NoPublisher, Store, now};

#[test]
fn backfill_on_a_prod_copy() {
    let Some(src) = std::env::var_os("STEVE_STATS_PROD_COPY") else {
        return;
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let real = std::fs::canonicalize(&src).unwrap();
    assert!(
        !real.starts_with(Path::new(&home).join(".steve")),
        "point STEVE_STATS_PROD_COPY at a backup, never at ~/.steve"
    );
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("steve.sqlite3");
    let st = Command::new("sqlite3")
        .arg(&real)
        .arg(format!(".backup '{}'", dst.display()))
        .status()
        .unwrap();
    assert!(st.success());

    let store = Store::open(&dst, NoPublisher).unwrap();
    let first = steve_stats::backfill::run(&store, false).unwrap();
    println!("backfill: {first:?}");
    let snapshot = |s: &Store| {
        s.with_conn(|c| {
            let q = |sql: &str| -> String {
                c.prepare(sql)
                    .unwrap()
                    .query_map([], |r| {
                        let n = r.as_ref().column_count();
                        Ok((0..n)
                            .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                            .collect::<Vec<_>>()
                            .join("|"))
                    })
                    .unwrap()
                    .map(Result::unwrap)
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            // an open item's wait grows with the clock between the two runs, so it is left out
            Ok(format!(
                "{}\n{}\n{}\n{}",
                q("SELECT session_id, user_seq, attempt, started, ended, stop_reason, tools, tool_errors, tool_kinds, permissions FROM agent_turns ORDER BY session_id, started, attempt"),
                q("SELECT item_id, ts, kind, from_v, to_v, actor, ref, detail FROM item_marks ORDER BY item_id, ts, kind"),
                q("SELECT * FROM stat_daily ORDER BY 1, 2, 3, 4, 5, 6, 7, 8"),
                q("SELECT item_id, done_at, lead_ms, cycle_ms, CASE WHEN done_at IS NULL AND dropped_at IS NULL THEN NULL ELSE wait_ms END, agent_ms, turns, delivers, reworks, first_pass, fails, releases, release_fails FROM item_stats ORDER BY item_id"),
            ))
        })
        .unwrap()
    };
    let a = snapshot(&store);
    // a second, forced run gives the same rows
    steve_stats::backfill::run(&store, true).unwrap();
    assert_eq!(a, snapshot(&store));
    // the migrated (v12 → current) and backfilled copy is still sound
    store
        .with_conn(|c| {
            let v: i64 = c.query_row("SELECT v FROM schema_version", [], |r| r.get(0))?;
            assert_eq!(v, steve_store::db::SCHEMA_VERSION);
            let ok: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            assert_eq!(ok, "ok");
            let fk: i64 =
                c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get(0)
                })?;
            assert_eq!(fk, 0, "foreign_key_check");
            Ok(())
        })
        .unwrap();

    let tz = store
        .with_conn(|c| steve_stats::refresh::tz_of(c, "default"))
        .unwrap();
    let first_day: String = store
        .with_conn(|c| {
            Ok(c.query_row("SELECT MIN(created) FROM items", [], |r| r.get::<_, i64>(0))?)
        })
        .map(|t| steve_stats::day(t, tz))
        .unwrap();
    let q = Query {
        workspace_id: "default".into(),
        from: first_day,
        to: steve_stats::day(now(), tz),
        ..Query::default()
    };
    let s = store.with_conn(|c| summary(c, &q)).unwrap().unwrap();
    println!("{}", serde_json::to_string_pretty(&s).unwrap());
    store
        .with_conn(|c| {
            let mut st = c.prepare("SELECT item_id, done_at, lead_ms, cycle_ms, wait_ms, agent_ms, turns, delivers, reworks, first_pass, fails, stalls, releases, release_fails FROM item_stats ORDER BY created")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                println!(
                    "{:?}",
                    (0..14).map(|i| format!("{:?}", r.get_ref(i).unwrap())).collect::<Vec<_>>()
                );
            }
            Ok(())
        })
        .unwrap();
}
