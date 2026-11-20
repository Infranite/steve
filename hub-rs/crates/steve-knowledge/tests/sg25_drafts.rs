//! SG25: document target resolution and field planning through the public draft API.
use serde_json::{Value, json};
use steve_knowledge::{
    Knowledge, drafts,
    notes::{self, NewNote},
    repo,
};
use steve_modules::changes::{self, Origin, Proposal};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, caller};
use steve_store::base::{Level, Visibility, changesets};

fn setup() -> TestEnv {
    let kb = Knowledge::new(|_| {});
    TestEnv::new(move |r| drafts::register(r, kb))
}
fn note(e: &TestEnv, ws: &str, user: &str, title: &str) -> String {
    e.store()
        .transaction(|tx| {
            notes::create(
                tx,
                ws,
                user,
                &NewNote {
                    title: title.into(),
                    body: "原始正文".into(),
                    visibility: Some(Visibility::Private),
                    level: Some(Level::Public),
                    ..Default::default()
                },
                100,
            )
        })
        .unwrap()
}
async fn plan(e: &TestEnv, user: &str, op: Value) -> Proposal {
    e.gateway.reply(json!({"ops":[op]}).to_string());
    changes::draft(
        &e.modules,
        &caller(WS, user),
        "knowledge",
        None,
        "修改文档范围",
        Origin::default(),
    )
    .await
    .unwrap()
}
fn ops(e: &TestEnv, id: &str) -> Vec<Value> {
    e.store()
        .reader(|c| changesets::get(c, id))
        .unwrap()
        .unwrap()
        .ops
}

#[tokio::test]
async fn sg25_document_resolution_is_exact_visible_unique_and_doc_key_has_precedence() {
    let e = setup();
    let id = note(&e, WS, ME, "Exact Title");
    for op in [
        json!({"op":"doc.patch","level":"never"}),
        json!({"op":"doc.patch","doc":null,"title":"Exact Title","level":"never"}),
        json!({"op":"doc.patch","doc":"","title":"Exact Title","level":"never"}),
        json!({"op":"doc.patch","doc":"Exact","level":"never"}),
        json!({"op":"doc.patch","doc":"missing","level":"never"}),
    ] {
        let p = plan(&e, ME, op).await;
        assert!(ops(&e, &p.id).is_empty());
        assert_eq!(p.changes.len(), 1);
    }
    let accepted = plan(
        &e,
        ME,
        json!({"op":"doc.patch","title":" exact TITLE ","level":"never"}),
    )
    .await;
    assert_eq!(
        ops(&e, &accepted.id),
        vec![json!({"op":"kb.doc.patch","doc":id,"set":{"level":"never"}})]
    );
    let hidden = plan(
        &e,
        "svc:sg25",
        json!({"op":"doc.patch","doc":"Exact Title","level":"never"}),
    )
    .await;
    assert!(ops(&e, &hidden.id).is_empty());
    assert_eq!(hidden.changes.len(), 1);
    assert_eq!(hidden.changes[0].to, "没有这篇文档");
    let duplicate = note(&e, WS, ME, "Exact Title");
    assert_ne!(id, duplicate);
    note(&e, WS, OTHER, "Exact Title");
    note(&e, WS2, ME, "Exact Title");
    let ambiguous = plan(
        &e,
        ME,
        json!({"op":"doc.patch","doc":"Exact Title","level":"never"}),
    )
    .await;
    assert!(ops(&e, &ambiguous.id).is_empty());
    assert_eq!(ambiguous.changes.len(), 1);
    assert!(
        ambiguous.changes[0].to.contains('3'),
        "{:?}",
        ambiguous.changes
    );
    assert_eq!(
        e.store()
            .reader(|c| repo::doc(c, &id))
            .unwrap()
            .unwrap()
            .level,
        Some(Level::Public),
        "planning never applies writes"
    );
}

#[tokio::test]
async fn sg25_scope_fields_plan_in_order_apply_or_follow_and_keep_unknown_fields_independent() {
    for (fields, expected_set, line_count) in [
        (
            json!({"visibility":"workspace","level":"reasoning"}),
            json!({"visibility":"workspace","level":"reasoning"}),
            2,
        ),
        (
            json!({"visibility":"follow","level":"跟随来源"}),
            json!({"visibility":null,"level":null}),
            2,
        ),
        (
            json!({"visibility":"只有我","level":" public "}),
            json!({}),
            0,
        ),
        (
            json!({"visibility":"unknown-scope","level":"unknown-level"}),
            json!({}),
            2,
        ),
        (
            json!({"visibility":"unknown-scope","level":"never"}),
            json!({"level":"never"}),
            2,
        ),
        (json!({"visibility":{},"level":[]}), json!({}), 0),
        (
            json!({"visibility":"","level":"follow","ignored":"value"}),
            json!({"level":null}),
            1,
        ),
        (json!({"visibility":17,"level":true}), json!({}), 2),
    ] {
        let e = setup();
        let id = note(&e, WS, ME, "Target");
        let mut op = json!({"op":"doc.patch","doc":"Target"});
        for (k, v) in fields.as_object().unwrap() {
            op[k] = v.clone();
        }
        let p = plan(&e, ME, op).await;
        assert_eq!(p.changes.len(), line_count, "{fields}: {:?}", p.changes);
        if line_count == 2 {
            assert!(p.changes[0].label.contains("可见范围") || p.changes[0].to == "没有改可见范围");
            assert!(p.changes[1].label.contains("披露级别") || p.changes[1].to == "没有改披露级别");
        }
        let empty = expected_set.as_object().unwrap().is_empty();
        assert_eq!(
            ops(&e, &p.id),
            if empty {
                vec![]
            } else {
                vec![json!({"op":"kb.doc.patch","doc":id,"set":expected_set})]
            },
            "{fields}"
        );
        let before = e.store().reader(|c| repo::doc(c, &id)).unwrap().unwrap();
        assert_eq!(before.visibility, Some(Visibility::Private));
        assert_eq!(before.level, Some(Level::Public));
        if !empty {
            assert_eq!(
                changes::apply(&e.modules, &caller(WS, ME), &p.id)
                    .unwrap()
                    .unwrap()["ok"],
                true
            );
            let after = e.store().reader(|c| repo::doc(c, &id)).unwrap().unwrap();
            let want_vis = expected_set
                .get("visibility")
                .map(|v| v.as_str().map(|s| Visibility::from_db(s)))
                .unwrap_or(Some(Visibility::Private));
            let want_level = expected_set
                .get("level")
                .map(|v| v.as_str().map(Level::from_db))
                .unwrap_or(Some(Level::Public));
            assert_eq!(after.visibility, want_vis);
            assert_eq!(after.level, want_level);
            assert_eq!(after.body, before.body);
            assert_eq!(after.title, before.title);
        }
    }
}

#[tokio::test]
async fn sg25_draft_apply_rechecks_visibility_instead_of_authorizing_from_a_stale_plan() {
    let e = setup();
    let id = note(&e, WS, ME, "Target");
    e.store()
        .transaction(|tx| {
            repo::set_doc_scope(
                tx,
                &id,
                Some(&Visibility::Workspace),
                Some(Level::Public),
                101,
            )?;
            steve_knowledge::index::rescope(
                tx,
                "chunk",
                &id,
                &Visibility::Workspace,
                Level::Public,
            )?;
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    let p = plan(
        &e,
        "svc:sg25",
        json!({"op":"doc.patch","doc":"Target","level":"never"}),
    )
    .await;
    assert_eq!(ops(&e, &p.id).len(), 1);
    e.store()
        .transaction(|tx| {
            repo::set_doc_scope(
                tx,
                &id,
                Some(&Visibility::Private),
                Some(Level::Public),
                102,
            )?;
            steve_knowledge::index::rescope(tx, "chunk", &id, &Visibility::Private, Level::Public)?;
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    let error = changes::apply(&e.modules, &caller(WS, "svc:sg25"), &p.id)
        .unwrap()
        .unwrap_err();
    assert_eq!(error.status(), 404);
    let row = e.store().reader(|c| repo::doc(c, &id)).unwrap().unwrap();
    assert_eq!(row.level, Some(Level::Public));
}
