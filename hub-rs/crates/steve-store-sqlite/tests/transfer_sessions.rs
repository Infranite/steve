#[path = "scan_support.rs"]
mod support;
use steve_storage_api::{
    cursor::{CursorKey, Position},
    query::{decode, encode},
    uow::{Committed, RequestDigest},
    *,
};
use steve_store_sqlite::transfer::{self, ExportManager, TransferReceipt, TransferState};
use steve_types::Code;
use support::*;
#[test]
fn fixed_snapshot_chunks_roundtrip_retry_and_durable_batch_identity() {
    let src = Fixture::new();
    src.seed(300);
    let dst = Fixture::new();
    let exports = ExportManager::new(src.engine.clone(), vec![src.desc.clone()]).unwrap();
    let session = exports.begin(Domain::Core).unwrap();
    src.writer
        .execute("UPDATE rec SET label='later',rev=999 WHERE id=1", [])
        .unwrap();
    src.writer
        .execute("INSERT INTO rec VALUES(301,0,'extra',999)", [])
        .unwrap();
    let mut cursor = ExportCursor::default();
    let mut total = 0;
    let mut batches = 0;
    loop {
        let batch = exports.export(&session, cursor.clone()).unwrap();
        assert_eq!(batch, exports.export(&session, cursor).unwrap());
        assert_eq!(transfer::batch_digest(&batch).unwrap(), batch.digest);
        assert!(batch.rows.len() <= 128);
        if batches == 0 {
            assert_eq!(batch.rows[0].0.0[2].1, Cell::Text("row1".into()));
        }
        let imported = transfer::import(&dst.writer, &dst.desc, batch.clone()).unwrap();
        assert!(!imported.replayed);
        total += imported.imported;
        let retry_conn = rusqlite::Connection::open(dst.engine.path()).unwrap();
        let retry = transfer::import(&retry_conn, &dst.desc, batch.clone()).unwrap();
        assert!(retry.replayed);
        assert_eq!(retry.imported, 0);
        batches += 1;
        match batch.next {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!((total, batches), (300, 3));
    assert_eq!(
        dst.writer
            .query_row("SELECT count(*) FROM rec", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        300
    );
    exports.end(&session).unwrap();
    assert_eq!(
        exports
            .export(&session, ExportCursor::default())
            .unwrap_err()
            .code,
        Code::Stale
    );
    assert_eq!(
        exports.begin(Domain::Stream).unwrap_err().code,
        Code::Unsupported
    );
    let session = exports.begin(Domain::Core).unwrap();
    let mut batch = exports.export(&session, ExportCursor::default()).unwrap();
    batch.rows[0].0.0[0].1 = Cell::Int(999);
    assert_eq!(
        transfer::import(&dst.writer, &dst.desc, batch)
            .unwrap_err()
            .code,
        Code::Invalid
    );
}
#[test]
fn validation_and_constraint_failure_roll_back_rows_metadata_and_batch_receipt() {
    let src = Fixture::new();
    src.seed(3);
    let dst = Fixture::new();
    let exports = ExportManager::new(src.engine.clone(), vec![src.desc.clone()]).unwrap();
    let session = exports.begin(Domain::Core).unwrap();
    let batch = exports.export(&session, ExportCursor::default()).unwrap();
    let err = transfer::import_with(
        &dst.writer,
        &dst.desc,
        batch.clone(),
        |conn, domain, state| {
            transfer::write_common_state(conn, domain, state)?;
            Err(error(Code::Invalid, "restore failed"))
        },
    )
    .unwrap_err();
    assert_eq!(err.code, Code::Invalid);
    assert_eq!(
        dst.writer
            .query_row("SELECT count(*) FROM rec", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let mut invalid = batch.clone();
    invalid.rows[1].0.0[2].1 = Cell::Null;
    invalid.digest = transfer::batch_digest(&invalid).unwrap();
    assert_eq!(
        transfer::import(&dst.writer, &dst.desc, invalid)
            .unwrap_err()
            .code,
        Code::Invalid
    );
    assert_eq!(
        dst.writer
            .query_row("SELECT count(*) FROM rec", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let mut invalid = batch.clone();
    invalid.rows.swap(0, 1);
    invalid.digest = transfer::batch_digest(&invalid).unwrap();
    assert_eq!(
        transfer::import(&dst.writer, &dst.desc, invalid)
            .unwrap_err()
            .code,
        Code::Invalid
    );
    dst.writer
        .execute("INSERT INTO rec VALUES(2,1,'existing',1)", [])
        .unwrap();
    assert_eq!(
        transfer::import(&dst.writer, &dst.desc, batch.clone())
            .unwrap_err()
            .code,
        Code::Invalid
    );
    assert_eq!(
        dst.writer
            .query_row("SELECT count(*) FROM rec", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    dst.writer.execute("DELETE FROM rec", []).unwrap();
    transfer::import(&dst.writer, &dst.desc, batch.clone()).unwrap();
    let mut changed = batch;
    changed.rows[0].0.0[2].1 = Cell::Text("changed".into());
    changed.digest = transfer::batch_digest(&changed).unwrap();
    assert_eq!(
        transfer::import(&dst.writer, &dst.desc, changed)
            .unwrap_err()
            .code,
        Code::Invalid
    );
}
#[test]
fn metadata_uses_reference_format_and_is_restored_atomically_without_events() {
    let src = Fixture::new();
    src.seed(1);
    let dst = Fixture::new();
    let state = TransferState {
        natural: vec![(
            (
                Domain::Core,
                "rec".into(),
                String::new(),
                serde_json::to_vec(&vec![Cell::Int(1)]).unwrap(),
            ),
            TransferReceipt {
                digest: RequestDigest([3; 32]),
                result: Committed {
                    rev: Rev(19),
                    seqs: vec![],
                    replayed: false,
                },
            },
        )],
        receipts: vec![(
            (Domain::Core, 12, "intent".into()),
            TransferReceipt {
                digest: RequestDigest([8; 32]),
                result: Committed {
                    rev: Rev(20),
                    seqs: vec![],
                    replayed: false,
                },
            },
        )],
        cursors: vec![(
            (Domain::Core, "consumer".into()),
            Position(CursorKey::Int(4)),
        )],
        seqs: vec![((Domain::Core, "stream".into()), 10)],
        ..Default::default()
    };
    transfer::write_state(&src.writer, Domain::Core, &state).unwrap();
    let exports = ExportManager::new(src.engine.clone(), vec![src.desc.clone()]).unwrap();
    let session = exports.begin(Domain::Core).unwrap();
    assert_eq!(session.rev, Rev(20));
    let batch = exports.export(&session, ExportCursor::default()).unwrap();
    let decoded: TransferState = decode(&batch.state).unwrap();
    assert_eq!(
        serde_json::to_string(&decoded).unwrap(),
        serde_json::to_string(&state).unwrap()
    );
    transfer::import(&dst.writer, &dst.desc, batch.clone()).unwrap();
    let restored = transfer::read_state(&dst.writer, Domain::Core).unwrap();
    assert_eq!(
        serde_json::to_string(&restored).unwrap(),
        serde_json::to_string(&state).unwrap()
    );
    // Reference digest fields are hashed as a JSON object with canonical field ordering.
    #[derive(serde::Serialize)]
    struct Contents<'a> {
        session: &'a ExportSession,
        id: &'a str,
        entity: &'a EntityDesc,
        rows: &'a [(Row, Rev)],
        state: &'a RawJson,
        next: &'a Option<ExportCursor>,
    }
    use sha2::{Digest, Sha256};
    let content = Contents {
        session: &batch.session,
        id: &batch.id,
        entity: &batch.entity,
        rows: &batch.rows,
        state: &batch.state,
        next: &batch.next,
    };
    let expected: [u8; 32] =
        Sha256::digest(uow::canonical_json(&encode(&content).unwrap()).unwrap()).into();
    assert_eq!(batch.digest, RequestDigest(expected));
    let mut mixed = batch;
    let mut bad = decoded;
    bad.cursors[0].0.0 = Domain::Comms;
    mixed.state = encode(&bad).unwrap();
    mixed.digest = transfer::batch_digest(&mixed).unwrap();
    assert_eq!(
        transfer::import(&dst.writer, &dst.desc, mixed)
            .unwrap_err()
            .code,
        Code::Invalid
    );
}

#[test]
fn domain_fact_adapters_restore_outbox_rows_in_the_same_batch_without_publishing() {
    let src = Fixture::new();
    src.seed(1);
    let dst = Fixture::new();
    let fact = fact::FactRecord {
        fact_id: "stable-fact".into(),
        topic: "verify.core.fact".into(),
        key: "key".into(),
        payload: RawJson::from_str("{ \"unchanged\": true }").unwrap(),
    };
    src.writer.execute("INSERT INTO outbox_events(id,workspace_id,topic,version,payload,created) VALUES(?1,'verify',?2,1,?3,0)",rusqlite::params![fact.fact_id,fact.topic,serde_json::to_string(&fact).unwrap()]).unwrap();
    let exports = ExportManager::new(src.engine.clone(), vec![src.desc.clone()]).unwrap();
    assert_eq!(
        exports.begin(Domain::Core).unwrap_err().code,
        Code::Unsupported
    );
    let session = exports
        .begin_with(Domain::Core, |conn, domain| {
            let mut state = transfer::read_state(conn, domain)?;
            let payload: String = conn
                .query_row(
                    "SELECT payload FROM outbox_events WHERE topic='verify.core.fact'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let fact: fact::FactRecord = serde_json::from_str(&payload).unwrap();
            state.facts.push(((domain, fact.fact_id.clone()), fact));
            Ok(state)
        })
        .unwrap();
    let batch = exports.export(&session, ExportCursor::default()).unwrap();
    assert_eq!(
        transfer::import(&dst.writer, &dst.desc, batch.clone())
            .unwrap_err()
            .code,
        Code::Unsupported
    );
    assert_eq!(
        dst.writer
            .query_row("SELECT count(*) FROM rec", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let restore = |conn: &rusqlite::Connection, domain, state: &TransferState| {
        transfer::write_common_state(conn, domain, state)?;
        for (_, fact) in &state.facts {
            conn.execute("INSERT INTO outbox_events(id,workspace_id,topic,version,payload,created) VALUES(?1,'verify',?2,1,?3,0)",rusqlite::params![fact.fact_id,fact.topic,serde_json::to_string(fact).unwrap()]).unwrap();
        }
        Ok(())
    };
    transfer::import_with(&dst.writer, &dst.desc, batch.clone(), restore).unwrap();
    assert!(
        transfer::import_with(&dst.writer, &dst.desc, batch, restore)
            .unwrap()
            .replayed
    );
    let restored: String = dst
        .writer
        .query_row("SELECT payload FROM outbox_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<fact::FactRecord>(&restored).unwrap(),
        fact
    );
    assert_eq!(
        dst.writer
            .query_row("SELECT count(*) FROM outbox_events", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn export_uses_the_domains_attached_file_for_schema_cursors_and_sequences() {
    use steve_store_sqlite::{layout::CORE_FILE, open};
    let temp = tempfile::tempdir().unwrap();
    let engine = open::open_runtime(temp.path()).unwrap();
    let mut layout = engine.layout().clone();
    drop(engine);
    layout
        .domains
        .insert(Domain::Stream, "stream.sqlite3".into());
    layout
        .files
        .insert("stream.sqlite3".into(), uuid::Uuid::new_v4().to_string());
    let core = rusqlite::Connection::open(temp.path().join(CORE_FILE)).unwrap();
    layout.write_file_meta(&core, CORE_FILE).unwrap();
    let peer = rusqlite::Connection::open(temp.path().join("stream.sqlite3")).unwrap();
    peer.execute_batch(steve_store_sqlite::schema::CONTRACT)
        .unwrap();
    layout.write_file_meta(&peer, "stream.sqlite3").unwrap();
    peer.execute_batch("INSERT INTO domain_schema VALUES('stream',1); CREATE TABLE remote(id INTEGER PRIMARY KEY,rev INTEGER NOT NULL); INSERT INTO remote VALUES(7,8); INSERT INTO meta VALUES('stream','seq:remote','42'); INSERT INTO cursor VALUES('stream','reader','{\"int\":9}',0);").unwrap();
    let engine = std::sync::Arc::new(open::open_runtime(temp.path()).unwrap());
    let desc = steve_storage_api::EntityDesc {
        name: "remote".into(),
        domain: Domain::Stream,
        key: vec!["id".into()],
        fields: vec![entity::FieldDesc {
            name: "id".into(),
            ty: entity::FieldType::Int,
            nullable: false,
        }],
        indexes: vec![],
        kind: entity::EntityKind::Mutable,
        retention: None,
    };
    let exports = ExportManager::new(engine, vec![desc]).unwrap();
    let session = exports.begin(Domain::Stream).unwrap();
    let batch = exports.export(&session, ExportCursor::default()).unwrap();
    assert_eq!(batch.rows[0].0.0[0].1, Cell::Int(7));
    assert_eq!(session.rev, Rev(8));
    let state: TransferState = decode(&batch.state).unwrap();
    assert_eq!(state.seqs, vec![((Domain::Stream, "remote".into()), 42)]);
    assert_eq!(
        state.cursors,
        vec![(
            (Domain::Stream, "reader".into()),
            Position(CursorKey::Int(9))
        )]
    );
}
