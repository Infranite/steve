//! SG25: candidate creation, confirmation, provenance and ownership on the old implementation.
use serde_json::json;
use steve_knowledge::candidates::{
    self, MemDecisions, NewCandidate, Origin, OriginKind, ResolveBody,
};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, caller};
use steve_store::base::{
    Level, ObjRef, Viewer, Visibility, decisions,
    mems::{self, NewMem},
};

fn setup() -> TestEnv {
    TestEnv::new(|r| {
        r.decision(candidates::SUBJECT, Box::new(MemDecisions));
    })
}
fn origin(kind: OriginKind) -> Origin {
    Origin {
        ws: WS.into(),
        kind,
        id: "origin-sg25".into(),
        label: "来源标签".into(),
        user: ME.into(),
        writer: "bot-sg25".into(),
        default_level: Level::Conclusion,
        item_id: None,
        session_id: None,
    }
}
fn candidate(text: &str) -> NewCandidate {
    NewCandidate {
        kind: "preference".into(),
        text: text.into(),
        about: vec![],
        level: Level::Reasoning,
        evidence: String::new(),
        supersedes: None,
        why: "  以后有用  ".into(),
        scope: "  写作时  ".into(),
        owner_said: false,
    }
}
fn propose(e: &TestEnv, o: &Origin, c: &NewCandidate) -> mems::Mem {
    e.store()
        .transaction(|tx| candidates::propose(tx, o, c))
        .unwrap()
}
fn n(e: &TestEnv, sql: &str, args: &[&str]) -> i64 {
    e.store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                sql,
                rusqlite::params_from_iter(args),
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

#[test]
fn sg25_origin_mapping_evidence_card_order_and_private_visibility() {
    let e = setup();
    for (kind, item, session, empty_origin, source_type, source_id) in [
        (
            OriginKind::Item,
            None,
            None,
            false,
            "item",
            Some("origin-sg25"),
        ),
        (
            OriginKind::Chat,
            None,
            None,
            false,
            "message",
            Some("origin-sg25"),
        ),
        (
            OriginKind::Mail,
            None,
            None,
            false,
            "message",
            Some("origin-sg25"),
        ),
        (
            OriginKind::Space,
            None,
            None,
            false,
            "message",
            Some("origin-sg25"),
        ),
        (
            OriginKind::Tool,
            Some("ST-25"),
            Some("ses-25"),
            false,
            "item",
            Some("ST-25"),
        ),
        (
            OriginKind::Tool,
            None,
            Some("ses-25"),
            false,
            "steve",
            Some("ses-25"),
        ),
        (OriginKind::Tool, None, None, false, "steve", None),
        (OriginKind::Item, None, None, true, "item", None),
    ] {
        let mut o = origin(kind);
        o.item_id = item.map(str::to_owned);
        o.session_id = session.map(str::to_owned);
        if empty_origin {
            o.id.clear();
        }
        let mut c = candidate("偏好清楚的交付说明");
        c.evidence = "证".repeat(candidates::EVIDENCE_MAX + 5);
        let m = propose(&e, &o, &c);
        assert_eq!(m.source_type, source_type);
        assert_eq!(m.source_id.as_deref(), source_id);
        assert_eq!(m.source_label, o.label);
        assert_eq!(m.writer, o.writer);
        assert_eq!(m.created_by, ME);
        assert_eq!(m.visibility, Visibility::Private);
        assert_eq!(m.level, Level::Reasoning);
        assert_eq!(m.status, "candidate");
        let evidence: String = e
            .store()
            .reader(|db| {
                Ok::<_, steve_store::StoreError>(db.query_row(
                    "SELECT evidence FROM mem_candidates WHERE mem_id=?",
                    [&m.id],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(evidence, "证".repeat(candidates::EVIDENCE_MAX));
        let d = e
            .store()
            .reader(|db| decisions::get(db, m.decision_id.as_deref().unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(d.session_id, o.session_id);
        let draft = &d.draft;
        let positions = [
            "类型：",
            "为什么值得保留：以后有用",
            "适用：写作时",
            "来源：来源标签",
            "原话：",
        ]
        .map(|text| draft.find(text).unwrap());
        assert!(positions.windows(2).all(|p| p[0] < p[1]), "{draft}");
        for (user, visible) in [(ME, true), (OTHER, true), ("svc:sg25", false)] {
            let seen = e
                .store()
                .reader(|db| {
                    let v = Viewer::load(db, WS, user)?;
                    mems::visible(db, &v, &m.id)
                })
                .unwrap();
            assert_eq!(seen.is_some(), visible);
        }
    }
}

#[test]
fn sg25_about_duplicates_collapse_but_low_level_proposals_keep_distinct_memories() {
    let e = setup();
    let party = e
        .store()
        .transaction(|tx| steve_modules::contacts::service::ensure_base(tx, &caller(WS, ME)))
        .unwrap();
    let mut c = candidate("相同文本");
    c.about = vec![
        ObjRef {
            kind: "party".into(),
            id: party.id.clone()
        };
        2
    ];
    let a = propose(&e, &origin(OriginKind::Chat), &c);
    let b = propose(&e, &origin(OriginKind::Chat), &c);
    assert_ne!(a.id, b.id);
    assert_eq!(a.about.len(), 1);
    assert_eq!(b.about, a.about);
    assert_eq!(
        n(
            &e,
            "SELECT count(*) FROM mem_about WHERE mem_id=?",
            &[&a.id]
        ),
        1
    );
    assert_eq!(
        n(
            &e,
            "SELECT count(*) FROM mem_candidates WHERE origin_id=?",
            &["origin-sg25"]
        ),
        2
    );
}

#[test]
fn sg25_confirmation_budget_does_not_block_owner_said_auto_confirmation() {
    let e = setup();
    let o = origin(OriginKind::Chat);
    for i in 0..steve_modules::contacts::memory::MAX_OPEN_CONFIRMS {
        assert!(
            propose(&e, &o, &candidate(&format!("candidate {i}")))
                .decision_id
                .is_some()
        );
    }
    let overflow = propose(&e, &o, &candidate("over budget"));
    assert!(overflow.decision_id.is_none());
    let mut c = candidate("以后都这样写");
    c.owner_said = true;
    let saved = propose(&e, &o, &c);
    assert_eq!(saved.status, "confirmed");
    let d = e
        .store()
        .reader(|db| decisions::get(db, saved.decision_id.as_deref().unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(d.status, "resolved");
    assert_eq!(d.result, json!({"choice":"yes","auto":"owner_said"}));
    assert_eq!(
        n(
            &e,
            "SELECT count(*) FROM kb_chunks WHERE obj_type='mem' AND obj_id=?",
            &[&saved.id]
        ),
        1
    );
    let actions: Vec<String> = e
        .store()
        .reader(|db| {
            let mut st = db.prepare(
                "SELECT action FROM audit WHERE target=? AND action LIKE 'mm.%' ORDER BY rowid",
            )?;
            Ok::<_, steve_store::StoreError>(
                st.query_map([&saved.id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?,
            )
        })
        .unwrap();
    assert_eq!(actions, ["mm.confirm", "mm.auto_confirm", "mm.propose"]);
}

#[test]
fn sg25_supersession_needs_confirmation_and_never_stales_another_workspace() {
    for old_ws in [WS, WS2] {
        let e = setup();
        let old = e
            .store()
            .transaction(|tx| {
                mems::create(
                    tx,
                    old_ws,
                    &NewMem {
                        text: "旧规则".into(),
                        created_by: ME.into(),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let mut c = candidate("新规则");
        c.owner_said = true;
        c.supersedes = Some(old.id.clone());
        let proposed = propose(&e, &origin(OriginKind::Tool), &c);
        assert_eq!(proposed.status, "candidate");
        let body = ResolveBody {
            ids: vec![proposed.id.clone()],
            choice: "yes".into(),
        };
        let refused =
            candidates::resolve_many(&e.modules, &caller(WS, "svc:sg25"), &body).unwrap_err();
        assert_eq!(refused.status(), 404);
        assert_eq!(
            e.store()
                .reader(|db| mems::get(db, &old.id))
                .unwrap()
                .unwrap()
                .status,
            "confirmed"
        );
        assert_eq!(
            candidates::resolve_many(&e.modules, &caller(WS, ME), &body).unwrap(),
            json!({"ok":true,"done":1})
        );
        assert_eq!(
            e.store()
                .reader(|db| mems::get(db, &proposed.id))
                .unwrap()
                .unwrap()
                .status,
            "confirmed"
        );
        assert_eq!(
            e.store()
                .reader(|db| mems::get(db, &old.id))
                .unwrap()
                .unwrap()
                .status,
            if old_ws == WS { "stale" } else { "confirmed" }
        );
        let d = e
            .store()
            .reader(|db| decisions::get(db, proposed.decision_id.as_deref().unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(d.status, "resolved");
        assert_eq!(d.result["choice"], "yes");
    }
}

#[test]
fn sg25_confirmation_failure_rolls_back_memory_provenance_and_publication() {
    let e = setup();
    e.store().with_conn(|db|{db.execute_batch("CREATE TEMP TRIGGER sg25_refuse BEFORE INSERT ON decisions BEGIN SELECT RAISE(ABORT,'sg25 confirmation failure'); END;")?;Ok(())}).unwrap();
    e.take_events();
    let err = e
        .store()
        .transaction(|tx| {
            candidates::propose(tx, &origin(OriginKind::Mail), &candidate("rollback"))
        })
        .unwrap_err();
    assert!(
        err.to_string().contains("sg25 confirmation failure"),
        "{err}"
    );
    for table in ["mems", "mem_candidates", "audit"] {
        assert_eq!(n(&e, &format!("SELECT count(*) FROM {table}"), &[]), 0);
    }
    assert!(e.take_events().is_empty());
}
