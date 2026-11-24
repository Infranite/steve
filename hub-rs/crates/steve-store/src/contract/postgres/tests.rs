use std::sync::{Arc, Mutex};

use rusqlite::{
    Connection,
    hooks::{AuthContext, Authorization},
};
use sea_query::{Expr, ExprTrait, Query, Values};
use serde_json::json;

use super::{Backend, Pg};
use crate::contract::{Q, audit};
use crate::views::Views;
use crate::{
    BusEvent, DecisionRows, EventSource, Fault, FaultNote, FnPublisher, Ingest, KeptFacts,
    MeterKey, NewDecision, R, Result, Store, StoreError, StreamKey, W, WorkspaceSettings,
    Workspaces, channel_write_policy as policy, stats_changed,
};

const MIGRATIONS: &[&str] = &[include_str!("001.sql")];

fn migrate(db: &mut Pg) -> Result<()> {
    db.batch(
        "CREATE TABLE IF NOT EXISTS schema_version (version BIGINT NOT NULL);
              INSERT INTO schema_version SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM schema_version);",
    )?;
    let mut version = 0;
    db.query(
        "SELECT version FROM schema_version",
        &Values(vec![]),
        &mut |row| {
            version = row.get::<i64>(0)?;
            Ok(())
        },
    )?;
    let current = usize::try_from(version)
        .ok()
        .filter(|n| *n <= MIGRATIONS.len())
        .ok_or_else(|| StoreError::Rejected(format!("unknown schema version {version}")))?;
    for (offset, migration) in MIGRATIONS[current..].iter().enumerate() {
        db.begin_write()?;
        let next = current + offset + 1;
        if let Err(err) = db.batch(migration).and_then(|_| {
            db.exec(
                "UPDATE schema_version SET version = $1",
                &Values(vec![(next as i64).into()]),
            )?;
            db.batch("COMMIT")
        }) {
            db.batch("ROLLBACK")?;
            return Err(err);
        }
    }
    Ok(())
}

struct Fixture {
    _directory: tempfile::TempDir,
    pg: Pg,
    schema: String,
    // Store owns the real staging machinery. SQL for these handles exclusively uses pg.
    events: Store,
    seen: Arc<Mutex<Vec<BusEvent>>>,
    no_sqlite: Connection,
}

impl Fixture {
    fn open(url: &str) -> Result<Self> {
        let directory = tempfile::tempdir().map_err(|e| StoreError::Storage(e.to_string()))?;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let events = Store::open(
            directory.path().join("events.sqlite3"),
            FnPublisher(move |event| sink.lock().unwrap().push(event)),
        )?;
        let no_sqlite = Connection::open_in_memory()?;
        no_sqlite.authorizer(Some(|_: AuthContext<'_>| Authorization::Deny))?;
        let mut pg = Pg::connect(url)?;
        let schema = format!("contract_{}", uuid::Uuid::new_v4().simple());
        pg.batch(&format!(
            "CREATE SCHEMA {schema}; SET search_path TO {schema}"
        ))?;
        Ok(Self {
            _directory: directory,
            pg,
            schema,
            events,
            seen,
            no_sqlite,
        })
    }

    fn write<T>(&mut self, f: impl FnOnce(&mut W<'_>) -> Result<T>) -> Result<T> {
        self.events
            .root_on(&self.no_sqlite, &mut self.pg, |conn, db| {
                f(&mut W {
                    db,
                    conn,
                    store: &self.events,
                })
            })
    }

    fn read<T>(&mut self, f: impl FnOnce(&mut R<'_>) -> Result<T>) -> Result<T> {
        self.pg.begin_read()?;
        let result = f(&mut R {
            db: &mut self.pg,
            conn: &self.no_sqlite,
        });
        self.pg
            .batch(if result.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
        result
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.pg.batch("ROLLBACK");
        let dropped = self
            .pg
            .batch(&format!("DROP SCHEMA {} CASCADE", self.schema));
        if !std::thread::panicking() {
            dropped.expect("remove the contract test schema");
        }
    }
}

#[test]
#[ignore = "requires the isolated STEVE_TEST_POSTGRES_URL service"]
fn postgres_contract() -> Result<()> {
    let url = std::env::var("STEVE_TEST_POSTGRES_URL").expect("set the isolated PostgreSQL URL");
    let mut f = Fixture::open(&url)?;
    migrate(&mut f.pg)?;
    migrate(&mut f.pg)?;
    f.write(|w| {
        Workspaces(w).insert("w", "Workspace", 1)?;
        WorkspaceSettings(w).set("w", "locale", "en", 1)?;
        WorkspaceSettings(w).set("w", "locale", "zh", 2)?;
        w.db().batch(
            "INSERT INTO sessions (id, workspace_id) VALUES ('s', 'w');
             INSERT INTO runtime_attempts (attempt_id, state) VALUES ('a', 'running');
             INSERT INTO runtime_sessions (session_id, node_id) VALUES ('s', 'n');",
        )
    })?;
    let k = StreamKey {
        node: "n",
        stream: "turn",
        epoch: "e",
    };
    let meter = MeterKey {
        node: k.node,
        stream: k.stream,
        epoch: k.epoch,
        meter_epoch: "meter",
    };
    let facts = KeptFacts {
        tools: json!(["inspect"]),
        permissions: 4_000_000_000,
        model: None,
        cost: Some(1.25),
    };
    let source = EventSource {
        node: k.node,
        stream: k.stream,
        epoch: k.epoch,
        seq: 4_000_000_001,
        attempt: Some("a"),
    };
    let seen = f.seen.clone();
    let (seq, decision) = f.write(|w| {
        assert_eq!(w.streams().open(k, Some("s"))?.applied, 0);
        w.streams().applied(k, source.seq)?;
        w.streams().applied(k, 1)?;
        assert!(w.streams().fault(k, Fault::Gap, FaultNote::default())?);
        w.attempts().keep_facts("a", &facts)?;
        w.attempts().touch("a", 3)?;
        w.cost_marks().raise(meter, 1.25)?;
        w.cost_marks().raise(meter, 0.5)?;
        w.runtime().seen_view("s", "n", "v1", Some("resume"))?;
        w.runtime().seen_view("s", "n", "v2", None)?;
        w.emit(stats_changed("before", None));
        let rejected = w.scope(|w| {
            w.emit(stats_changed("discarded", None));
            w.events().add_local("absent", "text", json!({}), 1)
        });
        assert!(
            matches!(rejected, Err(StoreError::Rejected(_))),
            "{rejected:?}"
        );
        let seq = w
            .events()
            .add(
                "s",
                "user",
                json!({"text":"kept", "steve_protocol_hash":"hash"}),
                Some(source),
                2,
            )?
            .expect("new event");
        assert_eq!(
            w.events().add("s", "user", json!({}), Some(source), 3)?,
            None
        );
        let decision = DecisionRows(w).open(&NewDecision {
            workspace_id: "w".into(),
            kind: "confirm".into(),
            subject_type: "test".into(),
            subject_id: "subject".into(),
            options: Some(vec!["yes".into()]),
            ..Default::default()
        })?;
        assert!(DecisionRows(w).settle(
            &decision.id,
            &json!({"choice":"yes"}),
            crate::decision_repo::DecisionChange::Resolve,
            None
        )?);
        assert!(policy::record(
            w,
            policy::WriteRefusal {
                ws: "w",
                connection: "c",
                chat: Some("chat"),
                reason: "read-only",
                now: 4,
                memory_ms: 10,
            }
        )?);
        audit::record(
            w,
            "w",
            "test",
            "chan_message.send.failed",
            "message",
            &json!({"error":"refused"}),
        )?;
        audit::revise(
            w,
            "w",
            "chan_message",
            "message",
            1,
            &json!("before"),
            &json!("after"),
            "test",
        )?;
        assert!(seen.lock().unwrap().is_empty());
        Ok((seq, decision.id))
    })?;
    assert_eq!(seen.lock().unwrap().len(), 4);
    f.read(|r| {
        assert_eq!(Workspaces(r).name("w")?.as_deref(), Some("Workspace"));
        assert_eq!(
            WorkspaceSettings(r).get("w", "locale")?.as_deref(),
            Some("zh")
        );
        let stream = r.streams().state(k)?.unwrap();
        assert!(stream.faulted);
        assert_eq!(stream.applied, source.seq);
        assert_eq!(r.attempts().facts("a")?, Some(facts));
        assert_eq!(r.cost_marks().mark(meter)?, Some(1.25));
        assert!(r.runtime().on_node("s", "n")?);
        let runtime: Option<(String, String, String)> = r.one(
            Query::select()
                .columns(["node_revision", "node_resume", "node_views"])
                .from("runtime_sessions")
                .and_where(Expr::col("session_id").eq("s")),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let (rev, resume, views) = runtime.unwrap();
        assert_eq!(rev, "v2");
        assert_eq!(resume, "resume");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&views)?,
            json!(["v1", "v2"])
        );
        assert_eq!(
            r.events().latest_protocol_hash("s")?.as_deref(),
            Some("hash")
        );
        assert_eq!(r.events().latest_input_seq("s")?, Some(seq));
        assert!(r.transcript().exists("w", "s")?);
        assert_eq!(r.transcript().marks("s", 1)?.marks, [seq]);
        assert_eq!(
            DecisionRows(r).get(&decision)?.unwrap().result,
            json!({"choice":"yes"})
        );
        assert_eq!(
            policy::active(r, "c", "chat", 5)?.as_deref(),
            Some("read-only")
        );
        assert_eq!(
            audit::channel_send_error(r, "w", "message")?.as_deref(),
            Some("refused")
        );
        assert_eq!(audit::next_revision(r, "w", "chan_message", "message")?, 2);
        Ok(())
    })?;
    let aborted = f.write::<()>(|w| {
        WorkspaceSettings(w).set("w", "locale", "de", 5)?;
        w.emit(stats_changed("rolled-back", None));
        Err(StoreError::Conflict("abort".into()))
    });
    assert!(matches!(aborted, Err(StoreError::Conflict(_))));
    assert_eq!(seen.lock().unwrap().len(), 4);
    assert_eq!(
        f.read(|r| WorkspaceSettings(r).get("w", "locale"))?
            .as_deref(),
        Some("zh")
    );
    domain_round_trips(&mut f)?;
    f.pg.batch("UPDATE schema_version SET version = 9223372036854775807")?;
    assert!(matches!(migrate(&mut f.pg), Err(StoreError::Rejected(_))));
    Ok(())
}

fn domain_round_trips(f: &mut Fixture) -> Result<()> {
    use crate::calendar::CalendarRepo;
    use crate::contract::{FlowSources, SourceConfig, github::Repositories, mail};
    use crate::digest::{Briefings, NewBriefing};
    use crate::knowledge::Documents;
    use crate::statistics::Catalog;
    use crate::{node_directory, nodes::NewNode};

    let at = 4_000_000_200;
    let material = json!({"changes":["build"]});
    let types = json!({"mail.new":{"on":true}});
    let poll_state = json!({"after":at});
    let key = [7; 32];
    f.write(|w| {
        w.db().batch(
            "INSERT INTO connections (id, workspace_id) VALUES ('c','w');
             INSERT INTO gh_repos (id, workspace_id, ingest) VALUES ('gh','w','poll');
             INSERT INTO cal_calendars (id,workspace_id,connection_id,owner_user,ext_id,name,created,updated)
                 VALUES ('cal','w','c','user','ext','Calendar',0,0);",
        )?;
        node_directory::register_local(w, &NewNode {
            node_id: "n", name: "Node", pubkey: &key, fingerprint: "fp",
        }, "owner", at)?;
        let config = SourceConfig {
            connection: "c", ws: "w", paused: false, triage_bot: None, types: &types,
            scopes: &json!([]), cursor: None, now: at,
        };
        FlowSources(w).save(&config)?;
        FlowSources(w).save(&SourceConfig { paused: true, ..config })?;
        FlowSources(w).save_cursor("w", "c", "next", at)?;
        Repositories::new(w).set_poll_state("gh", &poll_state)?;
        mail::ensure_settings(w, "w", "c", at)?;
        let mut mail = mail::settings(w, "c")?.unwrap();
        mail.archive_on_done = true;
        mail::update_settings(w, "c", &mail, at)?;
        CalendarRepo::new(w).set_synced("cal", Some("next"), true, at)?;
        Briefings(w).insert(NewBriefing {
            ws: "w", owner: "user", day: "2026-11-23", schedule: "schedule", event: "event",
            material: &material, pushed: true, created: at,
        })?;
        Documents(w).set_mark("w", "cursor", "next", at)?;
        Catalog(w).record_ok("provider", at, Some("etag"), Some(2))?;
        Catalog(w).record_err("provider", at + 1, "unavailable")?;
        Ok(())
    })?;
    f.read(|r| {
        assert_eq!(node_directory::link_key(r, "n")?, Some(key.to_vec()));
        let source = FlowSources(r).get("w", "c")?.unwrap();
        assert!(source.paused);
        assert_eq!(source.types, types);
        assert_eq!(source.cursor.as_deref(), Some("next"));
        assert_eq!(FlowSources(r).revision("w", &["c".into()])?, 2);
        assert_eq!(
            Repositories::new(r).poll_target("gh")?.unwrap().state,
            poll_state
        );
        let mail = mail::settings(r, "c")?.unwrap();
        assert!(mail.archive_on_done);
        assert_eq!(mail.revision, 2);
        let cal = CalendarRepo::new(r).calendar("cal")?.unwrap();
        assert_eq!(cal.sync_cursor.as_deref(), Some("next"));
        assert_eq!(cal.full_at, Some(at));
        let briefing = Briefings(r).get("w", "user", "2026-11-23")?.unwrap();
        assert_eq!(briefing.material, material);
        assert!(briefing.pushed);
        assert_eq!(Documents(r).mark("w", "cursor")?.as_deref(), Some("next"));
        let prices = Catalog(r).state("provider")?;
        assert_eq!(prices.synced, Some(at));
        assert_eq!(prices.tried, Some(at + 1));
        assert_eq!(prices.etag.as_deref(), Some("etag"));
        assert_eq!(prices.error.as_deref(), Some("unavailable"));
        Ok(())
    })?;
    workflow_ledger_round_trip(f)?;
    Ok(())
}

fn workflow_ledger_round_trip(f: &mut Fixture) -> Result<()> {
    use crate::workflow_runs::{
        NewWfAttempt, NewWfCall, ReviewGateFailure, ReviewGateVerdict, WfAttemptStatus,
        WorkflowRuns,
    };

    let (run_id, work_call_id, rev_call_id) = f.write(|w| {
        let run = WorkflowRuns(w).create_run(
            "w",
            "item-pg",
            1,
            1,
            "export default async () => 1",
            "sha1",
            None,
        )?;
        let (work_call, created) = WorkflowRuns(w).ensure_call(&NewWfCall {
            run_id: &run.id,
            workspace_id: "w",
            identity_hash: "h-work",
            phase: Some("impl"),
            title: Some("Implement"),
            prompt: "do work",
            options_canonical_json: "{}",
            schema_json: None,
            occurrence: 0,
            is_review: false,
            mutex_key: None,
        })?;
        assert!(created);
        WorkflowRuns(w).record_version_call(&run.id, 1, 0, &work_call.id, None)?;
        WorkflowRuns(w).create_attempt(&NewWfAttempt {
            call_id: &work_call.id,
            run_id: &run.id,
            parent_item_id: "item-pg",
            parent_cycle: 1,
            attempt_no: 1,
            child_item_id: Some("child-work"),
            bot_id: Some("bot-worker"),
            placement_id: None,
            harness: Some("claude-code"),
            model: Some("sonnet"),
            status: WfAttemptStatus::Dispatched,
            dispatched_at_revision: run.run_revision,
            error_code: None,
            error_message: None,
        })?;
        WorkflowRuns(w).stage_attempt_closing("child-work", 1, r#"{"done":true}"#, "[]")?;
        let fin = WorkflowRuns(w)
            .finalize_attempt("child-work", 1, "completed", None, None)?
            .expect("finalized work");
        let (rev_call, _) = WorkflowRuns(w).ensure_call(&NewWfCall {
            run_id: &run.id,
            workspace_id: "w",
            identity_hash: "h-rev",
            phase: Some("review"),
            title: Some("Review"),
            prompt: "review work",
            options_canonical_json: r#"{"review":true}"#,
            schema_json: None,
            occurrence: 0,
            is_review: true,
            mutex_key: None,
        })?;
        WorkflowRuns(w).record_version_call(&run.id, 1, 1, &rev_call.id, None)?;
        WorkflowRuns(w).create_attempt(&NewWfAttempt {
            call_id: &rev_call.id,
            run_id: &run.id,
            parent_item_id: "item-pg",
            parent_cycle: 1,
            attempt_no: 1,
            child_item_id: Some("child-rev"),
            bot_id: Some("bot-reviewer"),
            placement_id: None,
            harness: Some("codex"),
            model: Some("o3"),
            status: WfAttemptStatus::Dispatched,
            dispatched_at_revision: fin.new_run_revision + 1,
            error_code: None,
            error_message: None,
        })?;
        WorkflowRuns(w).stage_attempt_closing("child-rev", 1, r#"{"approved":true}"#, "[]")?;
        WorkflowRuns(w).finalize_attempt("child-rev", 1, "completed", None, None)?;
        Ok((run.id, work_call.id, rev_call.id))
    })?;

    f.read(|r| {
        assert_eq!(WorkflowRuns(r).count_cycle_attempts("item-pg", 1)?, 2);
        assert_eq!(WorkflowRuns(r).count_call_attempts(&work_call_id)?, 1);
        assert_eq!(
            WorkflowRuns(r).verify_independent_review(&run_id)?,
            ReviewGateVerdict::Verified {
                review_call_id: rev_call_id.clone()
            }
        );
        Ok(())
    })?;

    // Verify stale/same-bot checks fail cleanly on PG as well.
    let same_bot_run = f.write(|w| {
        let run = WorkflowRuns(w).create_run("w", "item-pg-2", 1, 1, "s", "sha2", None)?;
        let (wc, _) = WorkflowRuns(w).ensure_call(&NewWfCall {
            run_id: &run.id,
            workspace_id: "w",
            identity_hash: "hw",
            phase: None,
            title: None,
            prompt: "w",
            options_canonical_json: "{}",
            schema_json: None,
            occurrence: 0,
            is_review: false,
            mutex_key: None,
        })?;
        WorkflowRuns(w).create_attempt(&NewWfAttempt {
            call_id: &wc.id,
            run_id: &run.id,
            parent_item_id: "item-pg-2",
            parent_cycle: 1,
            attempt_no: 1,
            child_item_id: Some("c2-w"),
            bot_id: Some("bot-a"),
            placement_id: None,
            harness: None,
            model: None,
            status: WfAttemptStatus::Dispatched,
            dispatched_at_revision: 1,
            error_code: None,
            error_message: None,
        })?;
        WorkflowRuns(w).stage_attempt_closing("c2-w", 1, r#"{"ok":true}"#, "[]")?;
        let fin = WorkflowRuns(w)
            .finalize_attempt("c2-w", 1, "completed", None, None)?
            .unwrap();
        let (rc, _) = WorkflowRuns(w).ensure_call(&NewWfCall {
            run_id: &run.id,
            workspace_id: "w",
            identity_hash: "hr",
            phase: None,
            title: None,
            prompt: "r",
            options_canonical_json: r#"{"review":true}"#,
            schema_json: None,
            occurrence: 0,
            is_review: true,
            mutex_key: None,
        })?;
        WorkflowRuns(w).create_attempt(&NewWfAttempt {
            call_id: &rc.id,
            run_id: &run.id,
            parent_item_id: "item-pg-2",
            parent_cycle: 1,
            attempt_no: 1,
            child_item_id: Some("c2-r"),
            bot_id: Some("bot-a"),
            placement_id: None,
            harness: None,
            model: None,
            status: WfAttemptStatus::Dispatched,
            dispatched_at_revision: fin.new_run_revision + 1,
            error_code: None,
            error_message: None,
        })?;
        WorkflowRuns(w).stage_attempt_closing("c2-r", 1, r#"{"approved":true}"#, "[]")?;
        WorkflowRuns(w).finalize_attempt("c2-r", 1, "completed", None, None)?;
        Ok(run.id)
    })?;
    f.read(|r| {
        assert_eq!(
            WorkflowRuns(r).verify_independent_review(&same_bot_run)?,
            ReviewGateVerdict::Blocked {
                reason: ReviewGateFailure::SameBotAsWorker {
                    bot_id: "bot-a".into()
                }
            }
        );
        Ok(())
    })
}
