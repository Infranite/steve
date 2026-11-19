//! Descriptor-backed windows/actions for the shared retention runner. Selection,
//! time/row budgets, row rollback and continuation belong exclusively to that runner.
use super::SqlEntity;
use crate::retention::{Action, Policy, RetentionPolicy, Select, Window};
use rusqlite::Connection;
use steve_storage_api::{
    entity::FieldType,
    query::{IndexName, KeyRange, Order, Page, ResumeToken, Scan, ScanBudget, encode},
    *,
};

/// Check capabilities and descriptor references before acquiring the write grant.
pub(crate) fn authorize(entity: &SqlEntity) -> Result<&RetentionPolicy, Error> {
    let p = entity.desc.retention.as_ref().ok_or_else(|| {
        error(
            steve_types::Code::Unsupported,
            "entity has no retention policy",
        )
    })?;
    if p.before.is_some()
        || p.protect.is_some()
        || !matches!(p.select, Select::MaxAge { .. } | Select::Terminal { .. })
        || matches!(p.action, Action::Op(_))
    {
        return Err(error(
            steve_types::Code::Unsupported,
            "retention policy requires a domain handler",
        ));
    }
    crate::scan::validate_descriptor(&entity.desc)?;
    if p.entity != entity.desc.name || p.budget.rows == 0 || p.budget.target_ms == 0 {
        return Err(error(
            steve_types::Code::Invalid,
            "invalid retention declaration",
        ));
    }
    let since = match &p.select {
        Select::MaxAge { field, .. } => field,
        Select::Terminal {
            state, done, since, ..
        } => {
            entity.field(state).map_err(Error::from)?;
            for v in done {
                entity.check_cell(state, v).map_err(Error::from)?;
            }
            since
        }
        _ => unreachable!("capability checked above"),
    };
    if entity.field(since).map_err(Error::from)?.ty != FieldType::Millis {
        return Err(error(
            steve_types::Code::Invalid,
            "retention age field must be millis",
        ));
    }
    if let Action::Purge { fields } = &p.action {
        for name in fields {
            let f = entity.field(name).map_err(Error::from)?;
            if !f.nullable || entity.desc.key.contains(name) {
                return Err(error(
                    steve_types::Code::Invalid,
                    "retention purge requires nullable non-key fields",
                ));
            }
        }
    }
    Ok(p)
}

pub(crate) struct EntityPolicy<'a> {
    pub entity: &'a SqlEntity,
    pub definition: &'a RetentionPolicy,
    pub rev: Rev,
}

// Keep the original domain failure as the source, including retry classification.
// The runner rolls back a failed row, records it and continues the window.
fn action_error(e: CommitError) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}
fn query_error(e: Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}
pub(crate) fn read_error(e: rusqlite::Error) -> Error {
    match e {
        rusqlite::Error::ToSqlConversionFailure(source) => match source.downcast::<Error>() {
            Ok(e) => *e,
            Err(source) => super::sql_error(rusqlite::Error::ToSqlConversionFailure(source)).into(),
        },
        e => super::sql_error(e).into(),
    }
}

impl Policy for EntityPolicy<'_> {
    type Cursor = ResumeToken;
    fn definition(&self) -> &RetentionPolicy {
        self.definition
    }
    fn window(
        &self,
        c: &Connection,
        after: Option<&ResumeToken>,
        limit: u32,
    ) -> rusqlite::Result<Window<ResumeToken>> {
        let mut physical = self.entity.desc.clone();
        physical.name = self.entity.table.clone();
        let request = Scan::<Row> {
            index: IndexName {
                entity: physical.name.clone(),
                name: String::new(),
            },
            range: KeyRange::default(),
            filter: vec![],
            order: Order::Asc,
            limit,
            budget: ScanBudget(limit),
            resume: after.cloned(),
            marker: std::marker::PhantomData,
        };
        let page: Page<Row> = crate::scan::query(
            c,
            &physical,
            AnyQuery {
                name: "scan".into(),
                domain: physical.domain,
                body: Payload::Json(encode(&request).map_err(query_error)?),
            },
        )
        .and_then(|output| output.body.decode())
        .map_err(query_error)?;
        Ok(Window {
            more: page.resume.is_some(),
            rows: page
                .rows
                .into_iter()
                .map(|(row, _)| {
                    // Page.resume is the prefetched page's endpoint. A time-limited
                    // slice needs the PK of each *inspected* row, in descriptor order.
                    let cursor = ResumeToken {
                        key: super::values(&row, &self.entity.desc.key),
                    };
                    (row, cursor)
                })
                .collect(),
        })
    }
    fn apply(&self, c: &Connection, row: &Row) -> rusqlite::Result<u64> {
        let key = super::values(row, &self.entity.desc.key);
        match &self.definition.action {
            Action::Delete => self.entity.delete(c, &key).map(|()| 1),
            Action::Purge { fields } => {
                let patch = Row(fields.iter().map(|f| (f.clone(), Cell::Null)).collect());
                self.entity
                    .update(c, &key, &patch, self.rev, None)
                    .map(|n| n as u64)
            }
            Action::Op(_) => unreachable!("authorized before acquiring the write grant"),
        }
        .map_err(action_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{domains::Registry, retention as r};
    use std::{cell::RefCell, error::Error as _};
    use steve_storage_api::retention::*;

    fn fixture(action: Action) -> (Connection, Registry) {
        let mut desc = super::super::fixtures::LOG.to_wire();
        desc.name = "events".into();
        // Deliberately different from column order, and from the physical name.
        desc.key = vec!["value".into(), "id".into()];
        desc.retention = Some(RetentionPolicy {
            entity: desc.name.clone(),
            select: Select::MaxAge {
                field: "at".into(),
                keep_ms: 10,
            },
            keep_from: KeepFrom::Fixed { millis: 10 },
            action,
            before: None,
            protect: None,
            budget: r::DEFAULT_BUDGET,
        });
        let mut reg = Registry::default();
        reg.register(desc, "physical_events").unwrap();
        let c = Connection::open_in_memory().unwrap();
        reg.install(&c).unwrap();
        let entity = reg.entity("events", Domain::Core).unwrap();
        for (group, id, at) in [
            (2, "b", 0),
            (1, "d", 0),
            (1, "a", 100),
            (2, "a", 0),
            (1, "c", 0),
            (1, "b", 0),
        ] {
            let row = Row(entity
                .desc
                .fields
                .iter()
                .map(|f| {
                    let v = match f.name.as_str() {
                        "id" => Cell::Text(id.into()),
                        "value" => Cell::Int(group),
                        "at" => Cell::Millis(at),
                        "label" => Cell::Text("payload".into()),
                        _ => Cell::Null,
                    };
                    (f.name.clone(), v)
                })
                .collect());
            entity.insert(&c, &row, Rev(1), false).unwrap();
        }
        (c, reg)
    }
    fn adapter(reg: &Registry) -> EntityPolicy<'_> {
        let entity = reg.entity("events", Domain::Core).unwrap();
        EntityPolicy {
            entity,
            definition: authorize(entity).unwrap(),
            rev: Rev(99),
        }
    }
    fn token(group: i64, id: &str) -> ResumeToken {
        ResumeToken {
            key: vec![Cell::Int(group), Cell::Text(id.into())],
        }
    }
    // Production still rejects protection without a domain handler. This test hook
    // combines the real descriptor window/actions with protection in the shared loop.
    struct Protected<'a> {
        inner: EntityPolicy<'a>,
        windows: RefCell<Vec<Vec<ResumeToken>>>,
    }
    impl Policy for Protected<'_> {
        type Cursor = ResumeToken;
        fn definition(&self) -> &RetentionPolicy {
            self.inner.definition()
        }
        fn window(
            &self,
            c: &Connection,
            after: Option<&ResumeToken>,
            n: u32,
        ) -> rusqlite::Result<Window<ResumeToken>> {
            let w = self.inner.window(c, after, n)?;
            self.windows
                .borrow_mut()
                .push(w.rows.iter().map(|(_, k)| k.clone()).collect());
            Ok(w)
        }
        fn protect(&self, _: &Connection, row: &Row) -> rusqlite::Result<bool> {
            Ok(super::super::values(row, &self.inner.entity.desc.key) == token(1, "b").key)
        }
        fn apply(&self, c: &Connection, row: &Row) -> rusqlite::Result<u64> {
            self.inner.apply(c, row)
        }
    }

    #[test]
    fn timed_prefetch_resumes_at_the_last_inspected_composite_key_without_loss_or_replay() {
        let (c, reg) = fixture(Action::Delete);
        let p = Protected {
            inner: adapter(&reg),
            windows: RefCell::default(),
        };
        let budget = Budget {
            rows: 6,
            target_ms: r::SLICE_TARGET_MS,
        };
        // Two checks permit rows 2/3; the next check stops before row 4.
        let mut times = [
            0,
            0,
            u64::from(budget.target_ms),
            u64::from(budget.target_ms),
        ]
        .into_iter();
        let first =
            r::run_slice_timed(&c, &p, 100, budget, None, || times.next().unwrap()).unwrap();
        assert_eq!((first.inspected, first.affected, first.failures), (3, 1, 0));
        assert_eq!(
            first.next,
            Some(token(1, "c")),
            "fresh a and protected b still advance the inspected cursor"
        );
        let tail = r::run_slice_timed(&c, &p, 100, budget, first.next.as_ref(), || 0).unwrap();
        assert_eq!((tail.inspected, tail.affected, tail.next), (3, 3, None));
        assert_eq!(
            *p.windows.borrow(),
            vec![
                vec![
                    token(1, "a"),
                    token(1, "b"),
                    token(1, "c"),
                    token(1, "d"),
                    token(2, "a"),
                    token(2, "b")
                ],
                vec![token(1, "d"), token(2, "a"), token(2, "b")],
            ],
            "the whole page was prefetched; only uninspected rows are revisited"
        );
        let remaining = p.inner.window(&c, None, 6).unwrap();
        assert_eq!(
            remaining
                .rows
                .into_iter()
                .map(|(_, k)| k)
                .collect::<Vec<_>>(),
            vec![token(1, "a"), token(1, "b")]
        );
    }

    #[test]
    fn bounded_windows_keep_the_page_more_flag_but_each_row_has_its_own_key() {
        let (c, reg) = fixture(Action::Delete);
        let p = adapter(&reg);
        let w = p.window(&c, None, 2).unwrap();
        assert!(w.more);
        assert_eq!(
            w.rows.iter().map(|(_, k)| k.clone()).collect::<Vec<_>>(),
            [token(1, "a"), token(1, "b")]
        );
        let tail = p.window(&c, Some(&token(1, "b")), 6).unwrap();
        assert!(!tail.more);
        assert_eq!(tail.rows.len(), 4);
    }

    #[test]
    fn purge_uses_the_root_revision_and_does_not_rewrite_already_null_fields() {
        let (c, reg) = fixture(Action::Purge {
            fields: vec!["label".into()],
        });
        let p = adapter(&reg);
        c.execute(
            "UPDATE physical_events SET label=NULL WHERE value=1 AND id='b'",
            [],
        )
        .unwrap();
        let first = r::run_slice_timed(&c, &p, 100, r::DEFAULT_BUDGET, None, || 0).unwrap();
        assert_eq!((first.affected, first.failures), (4, 0));
        let second = r::run_slice_timed(&c, &p, 100, r::DEFAULT_BUDGET, None, || 0).unwrap();
        assert_eq!(second.affected, 0);
        for (id, rev, label) in [("a", 1, Some("payload")), ("b", 1, None), ("c", 99, None)] {
            let row = p.entity.get(&c, &token(1, id).key).unwrap().unwrap();
            assert_eq!(row.1, Rev(rev));
            assert_eq!(
                super::super::value(&row.0, "label"),
                Some(&label.map(|s| Cell::Text(s.into())).unwrap_or(Cell::Null))
            );
        }
    }

    #[test]
    fn action_errors_keep_the_commit_source_and_rollback_only_the_poison_row() {
        let (c, reg) = fixture(Action::Delete);
        let p = adapter(&reg);
        c.execute_batch(
            "CREATE TABLE effects(id TEXT);
            CREATE TRIGGER poison AFTER DELETE ON physical_events WHEN OLD.value=1 AND OLD.id='b'
            BEGIN INSERT INTO effects VALUES(OLD.id); SELECT RAISE(FAIL, 'poison row'); END;",
        )
        .unwrap();
        let row = p.entity.get(&c, &token(1, "b").key).unwrap().unwrap().0;
        c.execute_batch("BEGIN").unwrap();
        let e = p.apply(&c, &row).unwrap_err();
        assert!(matches!(e, rusqlite::Error::ToSqlConversionFailure(_)));
        assert!(matches!(
            e.source().unwrap().downcast_ref::<CommitError>(),
            Some(CommitError::Fatal(_))
        ));
        c.execute_batch("ROLLBACK").unwrap();
        let first = r::run_slice_timed(
            &c,
            &p,
            100,
            Budget {
                rows: 3,
                ..r::DEFAULT_BUDGET
            },
            None,
            || 0,
        )
        .unwrap();
        assert_eq!(
            (
                first.inspected,
                first.affected,
                first.failures,
                first.next.clone()
            ),
            (3, 1, 1, Some(token(1, "c")))
        );
        assert!(
            p.entity.get(&c, &token(1, "b").key).unwrap().is_some(),
            "FAIL can leave SQL effects until the runner rolls the row back"
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM effects", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        let tail =
            r::run_slice_timed(&c, &p, 100, r::DEFAULT_BUDGET, first.next.as_ref(), || 0).unwrap();
        assert_eq!((tail.affected, tail.failures, tail.next), (3, 0, None));
        c.execute_batch("DROP TRIGGER poison").unwrap();
        let retry = r::run_slice_timed(&c, &p, 100, r::DEFAULT_BUDGET, None, || 0).unwrap();
        assert_eq!(retry.affected, 1, "failed row is retried on the next sweep");
        let original = CommitError::Retryable {
            after_ms: Some(17),
            detail: "busy action".into(),
        };
        let bridged = action_error(original.clone());
        assert_eq!(
            bridged.source().unwrap().downcast_ref::<CommitError>(),
            Some(&original)
        );
    }

    #[test]
    fn capability_and_descriptor_errors_are_rejected_before_running_a_slice() {
        let (c, mut reg) = fixture(Action::Delete);
        let entity = reg.entities.get_mut("events").unwrap();
        entity.desc.retention.as_mut().unwrap().protect = Some(OpName("live".into()));
        assert_eq!(
            authorize(entity).unwrap_err().code,
            steve_types::Code::Unsupported
        );
        entity.desc.retention.as_mut().unwrap().protect = None;
        entity.desc.retention.as_mut().unwrap().action = Action::Purge {
            fields: vec!["id".into()],
        };
        assert_eq!(
            authorize(entity).unwrap_err().code,
            steve_types::Code::Invalid
        );
        assert!(c.is_autocommit());
        let original = error(steve_types::Code::Incompatible, "bad stored cell");
        assert_eq!(read_error(query_error(original.clone())), original);
    }
}
