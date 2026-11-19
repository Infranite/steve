use rusqlite::{Connection, params};
use steve_storage_api::retention::OpName;
use steve_store_sqlite::retention::{
    self as r, Action, Budget, Cell, KeepFrom, Policy, RetentionPolicy, Row, Select, Window,
};

struct FixturePolicy {
    definition: RetentionPolicy,
    protect: Vec<i64>,
    fail: Option<i64>,
}
impl FixturePolicy {
    fn age() -> Self {
        Self {
            definition: RetentionPolicy {
                entity: "t".into(),
                select: Select::MaxAge {
                    field: "at".into(),
                    keep_ms: 10,
                },
                keep_from: KeepFrom::Fixed { millis: 10 },
                before: None,
                protect: None,
                action: Action::Delete,
                budget: Budget {
                    rows: 500,
                    target_ms: 20,
                },
            },
            protect: vec![],
            fail: None,
        }
    }
}
fn id(row: &Row) -> i64 {
    r::policies::integer(row, "id").unwrap()
}
impl Policy for FixturePolicy {
    type Cursor = i64;
    fn definition(&self) -> &RetentionPolicy {
        &self.definition
    }
    fn window(
        &self,
        c: &Connection,
        after: Option<&i64>,
        limit: u32,
    ) -> rusqlite::Result<Window<i64>> {
        let sql = if after.is_some() {
            "SELECT id,at,state,payload FROM t WHERE id>?1 ORDER BY id LIMIT ?2"
        } else {
            "SELECT id,at,state,payload FROM t WHERE ?1 IS NULL ORDER BY id LIMIT ?2"
        };
        let rows = c
            .prepare(sql)?
            .query_map(params![after, limit], |row| {
                let id: i64 = row.get(0)?;
                let data: Option<String> = row.get(3)?;
                Ok((
                    Row(vec![
                        ("id".into(), Cell::Int(id)),
                        ("at".into(), Cell::Millis(row.get(1)?)),
                        ("state".into(), Cell::Text(row.get(2)?)),
                        ("payload".into(), data.map(Cell::Text).unwrap_or(Cell::Null)),
                    ]),
                    id,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows
            .last()
            .map(|(_, last)| {
                c.query_row("SELECT EXISTS(SELECT 1 FROM t WHERE id>?)", [last], |r| {
                    r.get(0)
                })
            })
            .transpose()?
            .unwrap_or(false);
        Ok(Window { rows, more })
    }
    fn protect(&self, _: &Connection, row: &Row) -> rusqlite::Result<bool> {
        Ok(self.protect.contains(&id(row)))
    }
    fn apply(&self, c: &Connection, row: &Row) -> rusqlite::Result<u64> {
        if self.fail == Some(id(row)) {
            c.execute("UPDATE t SET payload='partial' WHERE id=?", [id(row)])?;
            return Err(r::invalid("poison"));
        }
        let sql = if matches!(self.definition.action, Action::Delete) {
            "DELETE FROM t WHERE id=?"
        } else {
            "UPDATE t SET payload=NULL WHERE id=?"
        };
        Ok(c.execute(sql, [id(row)])? as u64)
    }
}
fn database() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY,at INTEGER,state TEXT,payload TEXT);")
        .unwrap();
    c
}
fn insert(c: &Connection, id: i64, at: i64, state: &str, payload: Option<&str>) {
    c.execute(
        "INSERT INTO t VALUES(?,?,?,?)",
        params![id, at, state, payload],
    )
    .unwrap();
}
fn count(c: &Connection) -> i64 {
    c.query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap()
}
#[test]
fn indexed_windows_bound_inspection_and_advance_past_fresh_and_protected_rows() {
    let c = database();
    let mut p = FixturePolicy::age();
    p.protect.push(602);
    for n in 1..=600 {
        insert(&c, n, 100, "active", Some("x"));
    }
    for n in 601..=605 {
        insert(&c, n, 0, "done", Some("x"));
    }
    let first = r::run_slice_timed(
        &c,
        &p,
        100,
        Budget {
            rows: 5000,
            target_ms: 20,
        },
        None,
        || 0,
    )
    .unwrap();
    assert_eq!(
        (first.inspected, first.affected, first.next),
        (500, 0, Some(500))
    );
    let second =
        r::run_slice_timed(&c, &p, 100, Budget::default(), first.next.as_ref(), || 0).unwrap();
    assert_eq!(
        (second.inspected, second.affected, second.next),
        (105, 4, None)
    );
    assert_eq!(count(&c), 601);
    let mut cursor = None;
    p.protect.clear();
    loop {
        let page =
            r::run_slice_timed(&c, &p, 200, Budget::default(), cursor.as_ref(), || 0).unwrap();
        assert!(page.inspected <= 500);
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(count(&c), 0);
}
#[test]
fn terminal_purge_has_strict_age_and_is_idempotent() {
    let c = database();
    let mut p = FixturePolicy::age();
    p.definition.select = Select::Terminal {
        state: "state".into(),
        done: vec![Cell::Text("done".into())],
        since: "at".into(),
        keep_ms: 10,
    };
    p.definition.action = Action::Purge {
        fields: vec!["payload".into()],
    };
    insert(&c, 1, 0, "done", Some("x"));
    insert(&c, 2, 0, "done", None);
    insert(&c, 3, 0, "active", Some("x"));
    insert(&c, 4, 90, "done", Some("x"));
    assert_eq!(
        r::run_slice_timed(&c, &p, 100, Budget::default(), None, || 0)
            .unwrap()
            .affected,
        1
    );
    assert_eq!(
        r::run_slice_timed(&c, &p, 100, Budget::default(), None, || 0)
            .unwrap()
            .affected,
        0
    );
    assert_eq!(count(&c), 4);
    assert!(!r::selected(
        &Row(vec![("at".into(), Cell::Millis(i64::MAX))]),
        &FixturePolicy::age().definition.select,
        i64::MIN,
        Some(0)
    ));
}
#[test]
fn time_budget_returns_the_last_checked_key_and_reports_an_overrun() {
    let c = database();
    let p = FixturePolicy::age();
    for n in 1..=100 {
        insert(&c, n, 0, "done", Some("x"));
    }
    let clock = std::cell::Cell::new(0);
    let page = r::run_slice_timed(
        &c,
        &p,
        100,
        Budget {
            rows: 500,
            target_ms: 1,
        },
        None,
        || {
            let n = clock.get() + 2;
            clock.set(n);
            n
        },
    )
    .unwrap();
    assert_eq!(
        (page.inspected, page.affected, page.next, page.overruns),
        (1, 1, Some(1), 1)
    );
    let tail =
        r::run_slice_timed(&c, &p, 100, Budget::default(), page.next.as_ref(), || 0).unwrap();
    assert_eq!(tail.affected, 99);
    assert!(!tail.more());
    assert_eq!(count(&c), 0);
}
#[test]
fn a_poison_row_rolls_back_only_its_action_and_does_not_block_later_rows() {
    let c = database();
    let mut p = FixturePolicy::age();
    p.fail = Some(2);
    for n in 1..=5 {
        insert(&c, n, 0, "done", Some("original"));
    }
    let page = r::run_slice_timed(&c, &p, 100, Budget::default(), None, || 0).unwrap();
    assert_eq!((page.affected, page.failures, page.next), (4, 1, None));
    assert_eq!(
        c.query_row("SELECT payload FROM t WHERE id=2", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "original"
    );
    assert!(c.is_autocommit());
}
#[test]
fn before_runs_without_a_write_transaction_only_for_selected_unprotected_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("before.sqlite3");
    let c = Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY,at INTEGER,state TEXT,payload TEXT);CREATE TABLE external(id INTEGER);").unwrap();
    for n in 1..=5 {
        insert(
            &c,
            n,
            if n == 2 { 100 } else { 0 },
            "done",
            Some("original"),
        );
    }
    let mut p = FixturePolicy::age();
    p.protect.push(3);
    p.definition.before = Some(OpName("remove_external".into()));
    let candidates = r::before_candidates(&c, &p, 100, Budget::default(), None).unwrap();
    let mut called = vec![];
    let prepared = r::prepare(&p, candidates, |row| {
        called.push(id(row));
        if id(row) == 4 {
            return Err(r::invalid("external failure"));
        }
        let other = Connection::open(&path)?;
        other.execute("INSERT INTO external VALUES(?)", [id(row)])?;
        Ok(true)
    });
    assert_eq!(called, [1, 4, 5]);
    assert_eq!(prepared.failed, 1);
    c.execute("UPDATE t SET payload='changed' WHERE id=5", [])
        .unwrap();
    let page = r::run_slice_timed(&c, &prepared, 100, Budget::default(), None, || 0).unwrap();
    assert_eq!((page.affected, page.failures), (1, 1));
    assert_eq!(count(&c), 4);
    assert_eq!(
        c.query_row("SELECT payload FROM t WHERE id=5", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "changed"
    );
}

#[test]
fn a_slice_inside_the_callers_transaction_does_not_commit_it() {
    let c = database();
    let p = FixturePolicy::age();
    insert(&c, i64::MIN, 0, "done", Some("x"));
    insert(&c, 1, 0, "done", Some("x"));
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    let report = r::run_slice_timed(&c, &p, 100, Budget::default(), None, || 0).unwrap();
    assert_eq!(
        report.affected, 2,
        "initial window includes the lowest possible primary key"
    );
    assert!(
        !c.is_autocommit(),
        "only the caller can commit its unit of work"
    );
    c.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        count(&c),
        2,
        "retention effects roll back with the containing unit of work"
    );
    assert!(
        r::run_slice(
            &c,
            &p,
            100,
            Budget {
                rows: 0,
                target_ms: 20
            },
            None
        )
        .is_err()
    );
    assert!(c.is_autocommit(), "invalid budget acquires no transaction");
}

#[test]
fn a_setting_without_a_scoped_resolver_uses_the_contract_default() {
    let c = database();
    let mut p = FixturePolicy::age();
    p.definition.keep_from = KeepFrom::Setting {
        key: "keep".into(),
        per: steve_storage_api::retention::SettingScope::Workspace,
        default_ms: 10,
    };
    insert(&c, 1, 0, "done", Some("x"));
    insert(&c, 2, 90, "done", Some("x"));
    let report = r::run_slice_timed(&c, &p, 100, Budget::default(), None, || 0).unwrap();
    assert_eq!(
        (report.affected, report.failures),
        (1, 0),
        "same default and strict boundary as the entity adapter"
    );
}
