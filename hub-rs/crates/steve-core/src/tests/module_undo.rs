//! `undoDecision` of decisions a module answered through `dispatch`: the stored undo marker makes them
//! undoable within core's rules, the module's `before_undo` runs in the same transaction as the reopen,
//! and a refusal leaves the decision answered with nothing of the undo kept.

use crate::CoreError;
use crate::testkit::Kit;
use serde_json::json;
use steve_modules::accounts::{self, Hints};
use steve_modules::contacts::{self, merge};
use steve_modules::decisions::{self, DecisionCx, DecisionHandler};
use steve_modules::testkit::{ME, WS, call, caller};
use steve_modules::{ApiError, ApiResult, Caller, Modules, Registry};

use steve_store::{Decision, W};

fn attach(k: &Kit, f: impl FnOnce(&mut Registry)) -> Modules {
    steve_store::core_fixture::thread::owner(k.db().store(), WS, "usr_dev", "dev", "Dev");
    let mut r = Registry::new();
    f(&mut r);
    let m = Modules::new(k.db().store().clone(), k.core.module_gateway(), r);
    k.core.attach_modules(m.clone());
    m
}

fn me() -> Caller {
    caller(WS, ME)
}

fn decision(k: &Kit, id: &str) -> Decision {
    k.db().decision(id).unwrap().unwrap()
}

#[tokio::test]
async fn a_merge_answered_through_dispatch_is_split_by_undo_decision() {
    let k = Kit::new();
    let m = attach(&k, |r| {
        accounts::register(r);
        merge::register(r, &accounts::ports());
    });
    let app = contacts::routes(accounts::ports())
        .merge(accounts::routes())
        .with_state(m.clone());
    let (s, org) = call(
        &app,
        Some(&me()),
        "POST",
        "/orgs",
        Some(&json!({ "name": "Acme", "domains": ["acme.com"] })),
    )
    .await;
    assert_eq!(s, 200, "{org}");
    let (s, sarah) = call(
        &app,
        Some(&me()),
        "POST",
        "/contacts",
        Some(&json!({ "name": "Sarah Chen", "org_id": org["id"] })),
    )
    .await;
    assert_eq!(s, 200, "{sarah}");
    let sarah = sarah["id"].as_str().unwrap().to_string();
    let got = m
        .store
        .write(|w| {
            steve_modules::accounts::identify::identify(
                w,
                WS,
                ME,
                "mail",
                "sarah.c.design@gmail.com",
                &Hints {
                    display_name: Some("Sarah Chen".into()),
                    signature_org: Some("Acme".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let (mp, from) = (got.proposal.expect("proposal"), got.party_id);
    let store = k.db().store();
    let (d, _) = steve_store::core_fixture::work::merge_proposal(store, &mp);
    let account = store
        .read(|r| steve_store::accounts::by_email(r, WS, "sarah.c.design@gmail.com"))
        .unwrap()
        .unwrap();
    let party = || {
        store
            .read(|r| steve_store::accounts::owned_party(r, WS, &account))
            .unwrap()
            .unwrap()
    };

    let r = decisions::dispatch(&m, &me(), &d, "merge", "")
        .expect("ours")
        .unwrap();
    assert_eq!(r.undo, json!({ "module": "party_merge" }));
    assert_eq!(party(), sarah);

    let back = k.core.undo_decision(&d, ME).unwrap();
    assert_eq!(back.status, "open");
    assert_eq!(back.undo, serde_json::Value::Null);
    assert_eq!(decision(&k, &d).status, "open");
    assert_eq!(
        steve_store::core_fixture::work::merge_proposal(store, &mp).1,
        "open"
    );
    assert_eq!(
        steve_store::core_fixture::work::party_status(store, &from),
        "active"
    );
    assert_eq!(party(), from, "the merge is split");
    assert_eq!(
        steve_store::core_fixture::work::audit_count(store, &["decision.undo"], Some(&d)),
        1
    );
    /* open again: nothing to undo, and it can be answered anew */
    assert_eq!(
        k.core.undo_decision(&d, ME).unwrap_err().to_string(),
        "这个决定不能撤销"
    );
    let r = decisions::dispatch(&m, &me(), &d, "merge", "")
        .expect("ours")
        .unwrap();
    assert_eq!(r.status, "resolved");
    assert_eq!(party(), sarah);
}

/// Writes an audit row in `before_undo`, then refuses unless told otherwise.
struct Refuses;

impl DecisionHandler for Refuses {
    fn resolve(
        &self,
        _: &mut W<'_>,
        cx: &DecisionCx,
        _: &Decision,
    ) -> ApiResult<serde_json::Value> {
        Ok(decisions::choice_result(cx))
    }
    fn before_undo(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        steve_store::audit_repo::record(
            tx,
            &d.workspace_id,
            "x",
            "x.before_undo",
            &d.id,
            &json!({}),
        )?;
        if d.result["choice"] == "yes" {
            return Err(ApiError::conflict("已经发出去了，撤不回"));
        }
        Ok(())
    }
}

fn open_confirm(m: &Modules, subject: &str) -> String {
    m.store
        .write(|w| {
            steve_store::DecisionRows(w).open(&decisions::confirm(
                WS,
                "x_thing",
                subject,
                "要吗？",
                ME,
            ))
        })
        .unwrap()
        .id
}

#[test]
fn a_refused_module_undo_leaves_the_decision_answered() {
    let k = Kit::new();
    let m = attach(&k, |r| {
        r.decision("x_thing", Box::new(Refuses));
    });
    let d = open_confirm(&m, "a");
    decisions::dispatch(&m, &me(), &d, "yes", "")
        .unwrap()
        .unwrap();
    let e = k.core.undo_decision(&d, ME).unwrap_err();
    assert!(
        matches!(&e, CoreError::Module(ApiError::Conflict(s)) if s == "已经发出去了，撤不回"),
        "{e:?}"
    );
    let after = decision(&k, &d);
    assert_eq!(after.status, "resolved");
    assert_eq!(after.result, json!({ "choice": "yes" }));
    assert_eq!(after.undo, json!({ "module": "x_thing" }));
    assert_eq!(
        steve_store::core_fixture::work::audit_count(
            k.db().store(),
            &["x.before_undo", "decision.undo"],
            None
        ),
        0,
        "the handler's write and the undo were rolled back together"
    );

    /* the same handler allows `no`; and core's 24-hour window holds for module decisions too */
    let d2 = open_confirm(&m, "b");
    decisions::dispatch(&m, &me(), &d2, "no", "")
        .unwrap()
        .unwrap();
    steve_store::core_fixture::decision(
        k.db().store(),
        &d2,
        steve_store::core_fixture::DecisionState::ShiftResolved(-86_400_001),
    );
    assert_eq!(
        k.core.undo_decision(&d2, ME).unwrap_err().to_string(),
        "这个决定不能撤销"
    );
    steve_store::core_fixture::decision(
        k.db().store(),
        &d2,
        steve_store::core_fixture::DecisionState::ShiftResolved(86_400_001),
    );
    k.core.undo_decision(&d2, ME).unwrap();
    assert_eq!(decision(&k, &d2).status, "open");
    assert_eq!(
        steve_store::core_fixture::work::audit_count(k.db().store(), &["x.before_undo"], None),
        1
    );
}
