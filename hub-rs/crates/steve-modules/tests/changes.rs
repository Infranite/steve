//! The changeset engine with a toy `contact` target (rename + create an org and link to it).

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::changes::{
    self, ChangeOp, DraftTarget, FieldSpec, OpCx, OpSpec, Origin, Plan, Prepared,
};
use steve_modules::testkit::{ME, TestEnv, WS, WS2, caller};
use steve_modules::{ApiError, ApiResult, Registry};
use steve_store::base::changesets::{self, UNDO_WINDOW_MS};
use steve_store::base::links::{self, NewLink};
use steve_store::base::{Change, ObjRef, Viewer};
use steve_store::{ChangeEvent, Tx, now};

struct Contact;

fn name_of(c: &Connection, id: &str) -> Option<(String, i64)> {
    c.query_row(
        "SELECT name, revision FROM parties WHERE id = ?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .unwrap()
}

impl DraftTarget for Contact {
    fn prepare(&self, c: &Connection, viewer: &Viewer, id: Option<&str>) -> ApiResult<Prepared> {
        let id = id.ok_or_else(|| ApiError::invalid("target.id"))?;
        let ws: Option<String> = c
            .query_row("SELECT workspace_id FROM parties WHERE id = ?", [id], |r| {
                r.get(0)
            })
            .optional()?;
        if ws.as_deref() != Some(viewer.ws.as_str()) {
            return Err(ApiError::not_found("联系人"));
        }
        let (name, rev) = name_of(c, id).unwrap();
        Ok(Prepared {
            target_id: Some(id.into()),
            revision: Some(rev),
            title: format!("{name} 的资料"),
            href: format!("#/contacts/{id}"),
            subject: format!("the contact \"{name}\""),
            fields: vec![
                FieldSpec::new("name", "名字", "text", json!(name)),
                FieldSpec::new("important", "重要", "enum", json!("否")).values(&["是", "否"]),
            ],
            ops: vec![OpSpec::new("job.open", "{\"org\": <organization name>}")],
            free_text: Some("note".into()),
            context: json!({ "name": name }),
        })
    }

    fn plan(&self, _: &Connection, _: &Viewer, p: &Prepared, raw: Vec<Value>) -> ApiResult<Plan> {
        let id = p.target_id.clone().unwrap();
        let mut plan = Plan::default();
        for op in raw {
            match op["op"].as_str() {
                Some("set") if op["key"] == "name" => {
                    let to = op["value"].as_str().unwrap_or("").to_string();
                    plan.changes.push(Change::new(
                        "名字",
                        p.context["name"].as_str().unwrap(),
                        to.clone(),
                    ));
                    plan.ops
                        .push(json!({"op": "party.rename", "id": id, "name": to}));
                }
                Some("job.open") => {
                    let org = op["org"].as_str().unwrap_or("").to_string();
                    plan.changes
                        .push(Change::new("任职（新建组织）", "—", org.clone()));
                    plan.ops
                        .push(json!({"op": "org.create", "name": org, "as": "org"}));
                    plan.ops
                        .push(json!({"op": "job.open", "contact": id, "org": "$org"}));
                }
                _ => plan
                    .changes
                    .push(Change::new("（没认出）", "—", op.to_string())),
            }
        }
        Ok(plan)
    }

    fn revision(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<i64>> {
        Ok(c.query_row(
            "SELECT revision FROM parties WHERE id = ? AND workspace_id = ?",
            [id, ws],
            |r| r.get(0),
        )
        .optional()?)
    }

    fn candidates(&self, _: &Connection, _: &Viewer) -> ApiResult<Vec<changes::Candidate>> {
        Ok(vec![changes::Candidate {
            code: "contact:ct_s".into(),
            label: "Sarah（Acme）".into(),
        }])
    }
}

struct Rename;
impl ChangeOp for Rename {
    fn validate(&self, c: &Connection, _: &OpCx, op: &Value) -> ApiResult<()> {
        if op["name"].as_str().unwrap_or("").is_empty() {
            return Err(ApiError::invalid("name"));
        }
        name_of(c, op["id"].as_str().unwrap())
            .map(|_| ())
            .ok_or_else(|| ApiError::not_found("联系人"))
    }
    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let id = op["id"].as_str().unwrap();
        let (old, _) = name_of(tx, id).unwrap();
        tx.execute(
            "UPDATE parties SET name = ?, revision = revision + 1 WHERE id = ?",
            params![op["name"].as_str(), id],
        )?;
        tx.publish(ChangeEvent::Party {
            id: id.into(),
            ws: cx.ws.into(),
        });
        Ok(json!({ "name": old }))
    }
    fn undo(&self, tx: &Tx, _: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        tx.execute(
            "UPDATE parties SET name = ?, revision = revision + 1 WHERE id = ?",
            params![before["name"].as_str(), op["id"].as_str()],
        )?;
        Ok(())
    }
}

struct OrgCreate;
impl ChangeOp for OrgCreate {
    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let id = steve_store::uid("org");
        tx.execute(
            "INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES (?, ?, 'org', ?, ?, 1, 1)",
            params![id, cx.ws, op["name"].as_str(), cx.actor],
        )?;
        cx.bind("org", json!(id));
        Ok(json!({ "id": id }))
    }
    fn undo(&self, tx: &Tx, _: &OpCx, _: &Value, before: &Value) -> ApiResult<()> {
        tx.execute("DELETE FROM parties WHERE id = ?", [before["id"].as_str()])?;
        Ok(())
    }
}

struct JobOpen;
impl ChangeOp for JobOpen {
    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let org = op["org"].as_str().unwrap();
        if org.starts_with('$') {
            return Err(ApiError::Internal("unbound".into()));
        }
        if org == "org_fail" {
            return Err(ApiError::rule("组织不对"));
        }
        links::add(
            tx,
            cx.ws,
            &NewLink::new(
                ObjRef::new("party", op["contact"].as_str().unwrap()),
                "works_at",
                ObjRef::new("party", org),
                "chat",
            ),
        )?;
        Ok(Value::Null)
    }
    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, _: &Value) -> ApiResult<()> {
        links::remove(
            tx,
            cx.ws,
            &links::LinkKey {
                subject: ObjRef::new("party", op["contact"].as_str().unwrap()),
                predicate: "works_at".into(),
                object: ObjRef::new("party", op["org"].as_str().unwrap()),
            },
        )?;
        Ok(())
    }
}

fn env() -> TestEnv {
    let t = TestEnv::new(|r: &mut Registry| {
        r.target("contact", Box::new(Contact))
            .op("party.rename", Box::new(Rename))
            .op("org.create", Box::new(OrgCreate))
            .op("job.open", Box::new(JobOpen));
    });
    t.store()
        .with_conn(|c| {
            c.execute_batch("INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES ('ct_s', 'default', 'contact', 'Sarah', 'usr_dev', 1, 1);")?;
            Ok(())
        })
        .unwrap();
    t
}

fn state(t: &TestEnv) -> (String, i64, i64, i64) {
    t.store()
        .with_conn(|c| {
            let (n, r) = name_of(c, "ct_s").unwrap();
            let orgs: i64 =
                c.query_row("SELECT count(*) FROM parties WHERE type = 'org'", [], |r| {
                    r.get(0)
                })?;
            let jobs: i64 = c.query_row(
                "SELECT count(*) FROM links WHERE predicate = 'works_at'",
                [],
                |r| r.get(0),
            )?;
            Ok((n, r, orgs, jobs))
        })
        .unwrap()
}

const REPLY: &str = r#"好的：{"ops":[{"op":"set","key":"name","value":"Sarah Chen"},{"op":"job.open","org":"Globex"}]}"#;

#[tokio::test]
async fn draft_apply_undo_round_trip() {
    let t = env();
    let me = caller(WS, ME);
    t.gateway.reply(REPLY);
    let p = changes::draft(
        &t.modules,
        &me,
        "contact",
        Some("ct_s"),
        "Sarah 全名 Sarah Chen，换到 Globex 了",
        Origin::default(),
    )
    .await
    .unwrap();
    let prompt = t.gateway.last_prompt().unwrap();
    assert!(prompt.contains("the contact \"Sarah\""));
    assert!(prompt.contains("name · 名字 · text · \"Sarah\""));
    assert!(prompt.contains("important · 重要 · enum · 是|否 · \"否\""));
    assert!(prompt.contains("job.open · {\"org\": <organization name>}"));
    assert!(prompt.contains("append it to \"note\""));
    assert_eq!(p.title, "Sarah 的资料");
    assert_eq!(p.target, json!({"kind": "contact", "id": "ct_s"}));
    assert_eq!(
        serde_json::to_value(&p.changes).unwrap(),
        json!([
            {"label": "名字", "from": "Sarah", "to": "Sarah Chen"},
            {"label": "任职（新建组织）", "from": "—", "to": "Globex"},
        ])
    );
    assert_eq!(
        state(&t),
        ("Sarah".into(), 1, 0, 0),
        "a draft changes nothing"
    );
    let ev = t.take_events();
    assert_eq!(
        ev,
        [ChangeEvent::Changeset {
            id: p.id.clone(),
            ws: WS.into()
        }]
    );

    assert_eq!(
        changes::apply(&t.modules, &me, &p.id).unwrap().unwrap(),
        json!({"ok": true, "undo": p.id})
    );
    assert_eq!(state(&t), ("Sarah Chen".into(), 2, 1, 1));
    let ev = t.take_events();
    assert!(ev.contains(&ChangeEvent::Party {
        id: "ct_s".into(),
        ws: WS.into()
    }));
    // the bound org id reached job.open and is what undo will see
    let cs = t
        .store()
        .reader(|c| changesets::get(c, &p.id))
        .unwrap()
        .unwrap();
    let before = cs.before.unwrap();
    assert_eq!(before.len(), 3);
    assert!(before[2]["op"]["org"].as_str().unwrap().starts_with("org_"));
    assert_eq!(before[0]["before"], json!({"name": "Sarah"}));

    assert_eq!(
        changes::apply(&t.modules, &me, &p.id).unwrap(),
        Err(ApiError::conflict(changes::STALE)),
        "second apply"
    );
    assert_eq!(
        changes::undo(&t.modules, &me, &p.id).unwrap().unwrap(),
        json!({"ok": true})
    );
    assert_eq!(state(&t).0, "Sarah");
    assert_eq!((state(&t).2, state(&t).3), (0, 0), "org and job gone");
    assert_eq!(
        changes::undo(&t.modules, &me, &p.id).unwrap(),
        Err(ApiError::conflict(changes::NOT_UNDOABLE)),
        "second undo"
    );
    t.store()
        .with_conn(|c| {
            let n: i64 = c.query_row("SELECT count(*) FROM audit WHERE action IN ('config.apply', 'config.undo') AND target = ?", [&p.id], |r| r.get(0))?;
            assert_eq!(n, 2);
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn target_revision_change_makes_the_draft_stale() {
    let t = env();
    let me = caller(WS, ME);
    t.gateway.reply(REPLY);
    let p = changes::draft(
        &t.modules,
        &me,
        "contact",
        Some("ct_s"),
        "x",
        Origin::default(),
    )
    .await
    .unwrap();
    t.store()
        .with_conn(|c| {
            c.execute(
                "UPDATE parties SET note = 'edited', revision = revision + 1 WHERE id = 'ct_s'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let r = changes::apply(&t.modules, &me, &p.id).unwrap();
    assert_eq!(r, Err(ApiError::conflict("这份草稿已经失效，请重新说一次")));
    assert_eq!(r.unwrap_err().status(), 409);
    assert_eq!(state(&t), ("Sarah".into(), 2, 0, 0));
    let cs = t
        .store()
        .reader(|c| changesets::get(c, &p.id))
        .unwrap()
        .unwrap();
    assert_eq!(cs.status, "stale");
}

#[tokio::test]
async fn undo_after_24_hours_is_refused() {
    let t = env();
    let me = caller(WS, ME);
    t.gateway.reply(REPLY);
    let p = changes::draft(
        &t.modules,
        &me,
        "contact",
        Some("ct_s"),
        "x",
        Origin::default(),
    )
    .await
    .unwrap();
    changes::apply(&t.modules, &me, &p.id).unwrap().unwrap();
    let applied = t
        .store()
        .reader(|c| changesets::get(c, &p.id))
        .unwrap()
        .unwrap()
        .applied
        .unwrap();
    let late = changes::undo_at(&t.modules, &me, &p.id, applied + UNDO_WINDOW_MS + 1).unwrap();
    assert_eq!(late, Err(ApiError::conflict("这次改动已经不能撤销了")));
    assert_eq!(state(&t).0, "Sarah Chen");
    assert!(
        changes::undo_at(&t.modules, &me, &p.id, applied + UNDO_WINDOW_MS)
            .unwrap()
            .is_ok(),
        "exactly 24 h is still fine"
    );
    assert!(now() >= applied);
}

#[tokio::test]
async fn failing_op_rolls_back_everything_and_keeps_the_draft() {
    let t = env();
    let me = caller(WS, ME);
    let prepared = t
        .store()
        .reader(|c| Contact.prepare(c, &Viewer::bare(WS, ME), Some("ct_s")))
        .unwrap();
    let plan = Plan {
        ops: vec![
            json!({"op": "party.rename", "id": "ct_s", "name": "X"}),
            json!({"op": "job.open", "contact": "ct_s", "org": "org_fail"}),
        ],
        changes: vec![],
    };
    let p = changes::store_plan(
        &t.modules,
        &me,
        "contact",
        &prepared,
        plan,
        Origin {
            origin: "signal".into(),
            basis: "evt_1".into(),
        },
    )
    .unwrap();
    assert_eq!(
        changes::apply(&t.modules, &me, &p.id).unwrap(),
        Err(ApiError::rule("组织不对"))
    );
    assert_eq!(state(&t), ("Sarah".into(), 1, 0, 0));
    let cs = t
        .store()
        .reader(|c| changesets::get(c, &p.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        (cs.status.as_str(), cs.origin.as_str(), cs.basis.as_str()),
        ("draft", "signal", "evt_1")
    );
    // validation failures too
    let plan = Plan {
        ops: vec![json!({"op": "party.rename", "id": "ct_s", "name": ""})],
        changes: vec![],
    };
    let p2 = changes::store_plan(
        &t.modules,
        &me,
        "contact",
        &prepared,
        plan,
        Origin::default(),
    )
    .unwrap();
    assert_eq!(
        changes::apply(&t.modules, &me, &p2.id).unwrap(),
        Err(ApiError::invalid("name"))
    );
    assert_eq!(
        changes::drop_draft(&t.modules, &me, &p2.id).unwrap(),
        Ok(json!({"ok": true}))
    );
    assert!(
        changes::drop_draft(&t.modules, &me, &p2.id)
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn unknown_ids_fall_back_and_other_workspaces_are_refused() {
    let t = env();
    let me = caller(WS, ME);
    assert!(
        changes::apply(&t.modules, &me, "chg_inmemory").is_none(),
        "not ours: the in-memory drafts handle it"
    );
    assert!(changes::undo(&t.modules, &me, "chg_inmemory").is_none());
    t.gateway.reply(REPLY);
    let p = changes::draft(
        &t.modules,
        &me,
        "contact",
        Some("ct_s"),
        "x",
        Origin::default(),
    )
    .await
    .unwrap();
    let there = caller(WS2, ME);
    assert_eq!(
        changes::apply(&t.modules, &there, &p.id).unwrap(),
        Err(ApiError::conflict(changes::STALE))
    );
    // drafting a target in another workspace is a 404 from the target's own check
    assert_eq!(
        changes::draft(
            &t.modules,
            &there,
            "contact",
            Some("ct_s"),
            "x",
            Origin::default()
        )
        .await
        .unwrap_err(),
        ApiError::not_found("联系人")
    );
    assert_eq!(
        changes::draft(&t.modules, &me, "nope", None, "x", Origin::default())
            .await
            .unwrap_err(),
        ApiError::invalid("target.kind")
    );
    assert_eq!(
        changes::candidates(&t.modules, &me).unwrap()[0].line(),
        "contact:ct_s — Sarah（Acme）"
    );
}

#[tokio::test]
async fn unusable_model_reply_gives_an_empty_draft() {
    let t = env();
    let me = caller(WS, ME);
    t.gateway.reply("抱歉，我不明白");
    let p = changes::draft(
        &t.modules,
        &me,
        "contact",
        Some("ct_s"),
        "x",
        Origin::default(),
    )
    .await
    .unwrap();
    assert!(p.changes.is_empty());
    t.gateway.fail("timeout");
    assert_eq!(
        changes::draft(
            &t.modules,
            &me,
            "contact",
            Some("ct_s"),
            "x",
            Origin::default()
        )
        .await
        .unwrap_err()
        .status(),
        500
    );
    assert_eq!(changes::first_json("x {\"a\":1} y"), Some(json!({"a": 1})));
    assert_eq!(changes::first_json("none"), None);
}
