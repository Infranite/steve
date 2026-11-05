#![allow(clippy::unit_arg, clippy::cloned_ref_to_slice_refs)] // terse assertions in tests
//! Items, deps, timeline, artifacts, sessions, events, queued, decisions, messages.

mod common;

use common::*;
use serde_json::{Value, json};
use steve_store::*;

fn with_steve() -> T {
    let t = fresh();
    t.store
        .bots()
        .seed(
            "steve",
            true,
            &NewBot {
                name: "Steve".into(),
                harness: "claude-code".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    t
}

fn new_item(s: &Store, ws: &str, title: &str) -> Item {
    s.items()
        .create(
            ws,
            &NewItem {
                title: title.into(),
                goal: format!("{title} goal"),
                bot_id: "steve".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
}

fn new_session(s: &Store, item: Option<&str>, token: &str) -> Session {
    s.sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            item_id: item.map(Into::into),
            bot_id: Some("steve".into()),
            harness: "claude-code".into(),
            model: None,
            reasoning_effort: None,
            cwd: "/tmp".into(),
            title: "t".into(),
            token: token.into(),
        })
        .unwrap()
}

#[test]
fn item_numbers_are_global_per_prefix() {
    let t = with_steve();
    let s = &t.store;
    let a = new_item(s, WS, "a");
    assert_eq!(
        (
            a.id.as_str(),
            a.no,
            a.status.as_str(),
            a.stage.as_str(),
            a.kind.as_str()
        ),
        ("ST-1", 1, "run", "定义", "task")
    );
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Item { id: "ST-1".into() }]
    );
    s.with_conn(|c| Ok(c.execute("INSERT INTO workspaces (id, name, item_prefix, created) VALUES ('w2', 'B', 'ST', 1), ('w3', 'C', 'XY', 1)", [])?)).unwrap();
    assert_eq!(
        new_item(s, "w2", "b").id,
        "ST-2",
        "same prefix in another workspace continues the count"
    );
    assert_eq!(new_item(s, "w3", "c").id, "XY-1");
    assert_eq!(
        new_item(s, "missing-ws", "d").id,
        "ST-3",
        "unknown workspace falls back to ST"
    );
    assert_eq!(s.peek_counter("item:ST").unwrap(), 3);
    let chat = s
        .items()
        .create(
            WS,
            &NewItem {
                title: "c".into(),
                goal: "g".into(),
                bot_id: "steve".into(),
                kind: Some("chat".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    assert!(chat.id.starts_with("chat_") && chat.no == 0);
    assert_eq!(s.items().all(WS).unwrap().len(), 1, "chats are not listed");
}

#[test]
fn item_json_shape() {
    let t = with_steve();
    let s = &t.store;
    let fork = NewItem {
        title: "派生".into(),
        goal: "g".into(),
        bot_id: "steve".into(),
        source: Some(("派生".into(), "fork".into())),
        parent: Some(Parent {
            item: Some(None),
            session: Some(Some("ses_1".into())),
            at: Some(Some(12)),
        }),
        status: Some("wait".into()),
        stage: Some("规划".into()),
        ..Default::default()
    };
    let i = s.items().create(WS, &fork).unwrap().unwrap();
    let j = serde_json::to_value(&i).unwrap();
    assert_eq!(
        j,
        json!({
            "id": "ST-1", "workspace_id": "default", "no": 1, "kind": "task", "title": "派生", "status": "wait", "stage": "规划",
            "bot_id": "steve", "project_dir": null, "cwd": null, "branch": null, "plan": [], "now": "", "accept_mode": null,
            "priority": "中", "archived": false, "created": i.created, "updated": i.updated, "project_id": null, "star": false,
            "due": null, "snooze": null, "source": ["派生", "fork"], "parent": {"item": null, "session": "ses_1", "at": 12},
            "phase": "active", "cycle": 1, "revision": 0, "outcome": null, "acceptance": null, "closing": null,
            "closed_at": null, "closed_reason": null
        })
    );
    s.with_conn(|c| {
        let (src, parent): (String, String) =
            c.query_row("SELECT source, parent FROM items", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        assert_eq!(
            (src.as_str(), parent.as_str()),
            (
                r#"["派生","fork"]"#,
                r#"{"item":null,"session":"ses_1","at":12}"#
            )
        );
        // TS fallbacks: bad JSON → defaults
        c.execute(
            "UPDATE items SET plan = 'oops', source = '', snooze = 'x'",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    let i = s.items().get("ST-1").unwrap().unwrap();
    assert!(i.plan.is_empty() && i.snooze.is_none());
    assert_eq!(i.source, ("对话".to_string(), "chat".to_string()));
}

#[test]
fn item_patch() {
    let t = with_steve();
    let s = &t.store;
    let i = new_item(s, WS, "a");
    t.events.take();
    assert_eq!(
        s.items()
            .patch(&i.id, &ItemPatch::default())
            .unwrap()
            .unwrap()
            .updated,
        i.updated
    );
    assert!(t.events.take().is_empty(), "empty patch publishes nothing");
    let p = ItemPatch {
        plan: Some(vec![PlanStep {
            step: "一".into(),
            done: true,
            left: false,
        }]),
        star: Some(true),
        cwd: Some(Some("/w".into())),
        snooze: Some(Some(Snooze {
            label: "明天".into(),
            until: 99,
        })),
        ..Default::default()
    };
    let got = s.items().patch(&i.id, &p).unwrap().unwrap();
    assert!(got.star && got.cwd.as_deref() == Some("/w") && got.plan.len() == 1);
    assert_eq!(
        serde_json::to_value(&got).unwrap()["snooze"],
        json!({"label": "明天", "until": 99})
    );
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Item { id: i.id.clone() }]
    );
    // a stored decision undo applies as a patch; explicit null clears
    let undo: ItemPatch = serde_json::from_value(json!({"status": "wait", "now": "在等你回答", "stage": "交付与验收", "cwd": null, "snooze": null})).unwrap();
    let got = s.items().patch(&i.id, &undo).unwrap().unwrap();
    assert_eq!(
        (got.status.as_str(), got.now.as_str(), got.stage.as_str()),
        ("wait", "在等你回答", "交付与验收")
    );
    assert!(
        got.cwd.is_none() && got.snooze.is_none() && got.star,
        "omitted keys stay"
    );
    s.items()
        .patch(
            &i.id,
            &ItemPatch {
                archived: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(s.items().list(WS, false).unwrap().is_empty());
    assert_eq!(s.items().list(WS, true).unwrap().len(), 1);
}

#[test]
fn goals_deps_timeline_artifacts_remove() {
    let t = with_steve();
    let s = &t.store;
    let (a, b) = (new_item(s, WS, "a"), new_item(s, WS, "b"));
    assert_eq!(s.items().goal(&a.id).unwrap(), "a goal");
    assert_eq!(s.items().add_goal(&a.id, "补充").unwrap(), 2);
    assert_eq!(
        s.items()
            .goals(&a.id)
            .unwrap()
            .iter()
            .map(|g| (g.version, g.text.as_str()))
            .collect::<Vec<_>>(),
        [(1, "a goal"), (2, "补充")]
    );
    assert_eq!(s.items().goal(&a.id).unwrap(), "补充");
    assert_eq!(s.items().goal("nope").unwrap(), "");

    s.deps().add(&b.id, &a.id).unwrap();
    s.deps().add(&b.id, &a.id).unwrap();
    assert_eq!(s.deps().of(&b.id).unwrap(), [a.id.clone()]);
    assert_eq!(s.deps().dependents(&a.id).unwrap(), [b.id.clone()]);
    s.timeline()
        .add(&a.id, "me", "me", "你好", Some("ses_x"), &[])
        .unwrap();
    s.artifacts()
        .add(&a.id, "pr", "PR #1", Some("https://example.com/1"))
        .unwrap();
    let tl = s.timeline().list(&a.id).unwrap();
    assert_eq!(
        (tl[0].text.as_str(), tl[0].session_id.as_deref()),
        ("你好", Some("ses_x"))
    );
    assert!(s.artifacts().list(&a.id).unwrap()[0].id.starts_with("art_"));
    t.events.take();

    let ses = new_session(s, Some(&a.id), "tok");
    let dec = s
        .decisions()
        .create(&NewDecision {
            workspace_id: WS.into(),
            kind: "ask".into(),
            subject_type: "item".into(),
            subject_id: a.id.clone(),
            item_id: Some(a.id.clone()),
            question: "q".into(),
            requested_by: "steve".into(),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
    t.events.take();
    s.items().remove(&a.id).unwrap();
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Item { id: a.id.clone() }]
    );
    assert!(s.items().get(&a.id).unwrap().is_none());
    let ses = s.sessions().get(&ses.id).unwrap().unwrap();
    assert_eq!((ses.item_id.clone(), ses.no), (None, 0));
    assert!(
        s.decisions()
            .get(&dec.id)
            .unwrap()
            .unwrap()
            .item_id
            .is_none()
    );
    assert!(s.deps().of(&b.id).unwrap().is_empty() && s.timeline().list(&a.id).unwrap().is_empty());
}

#[test]
fn sessions() {
    let t = with_steve();
    let s = &t.store;
    let i = new_item(s, WS, "a");
    t.events.take();
    let s1 = new_session(s, Some(&i.id), "t1");
    let s2 = new_session(s, Some(&i.id), "t2");
    let free = new_session(s, None, "t3");
    assert_eq!(
        (s1.no, s2.no, free.no, s1.status.as_str(), s1.readonly),
        (1, 2, 1, "starting", 0)
    );
    assert_eq!(
        t.events.take(),
        vec![
            ChangeEvent::Session {
                id: s1.id.clone(),
                item_id: Some(Some(i.id.clone()))
            },
            ChangeEvent::Session {
                id: s2.id.clone(),
                item_id: Some(Some(i.id.clone()))
            },
            ChangeEvent::Session {
                id: free.id.clone(),
                item_id: Some(None)
            },
        ]
    );
    assert_eq!(serde_json::to_value(&s1).unwrap()["readonly"], json!(0));
    assert_eq!(s.sessions().by_token("t2").unwrap().unwrap().id, s2.id);
    assert_eq!(
        s.sessions()
            .of_item(&i.id)
            .unwrap()
            .iter()
            .map(|x| x.no)
            .collect::<Vec<_>>(),
        [1, 2]
    );

    s.sessions()
        .patch(
            &s1.id,
            &SessionPatch {
                status: Some("idle".into()),
                used: Some(Some(10)),
                cost: Some(Some(1.5)),
                ..Default::default()
            },
        )
        .unwrap();
    s.sessions()
        .patch(&s1.id, &SessionPatch::default())
        .unwrap();
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Session {
            id: s1.id.clone(),
            item_id: Some(Some(i.id.clone()))
        }]
    );
    let got = s.sessions().get(&s1.id).unwrap().unwrap();
    assert_eq!(
        (got.status.as_str(), got.used, got.cost),
        ("idle", Some(10), Some(1.5))
    );
    s.sessions()
        .patch(
            &s1.id,
            &SessionPatch {
                used: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(s.sessions().get(&s1.id).unwrap().unwrap().used, None);
    s.sessions().rotate_token(&s1.id, "new").unwrap();
    assert_eq!(s.sessions().by_token("new").unwrap().unwrap().id, s1.id);
    assert!(s.sessions().by_token("t1").unwrap().is_none());
    // an ended session's token opens nothing; a session that runs again is reachable again
    let end = |status: &str| {
        s.sessions()
            .patch(
                &s1.id,
                &SessionPatch {
                    status: Some(status.into()),
                    ..Default::default()
                },
            )
            .unwrap()
    };
    end("ended");
    assert!(s.sessions().by_token("new").unwrap().is_none());
    end("idle");
    assert_eq!(s.sessions().by_token("new").unwrap().unwrap().id, s1.id);
    assert_eq!(s.sessions().list(Some(WS)).unwrap().len(), 3);
    assert_eq!(s.sessions().list(None).unwrap().len(), 3);
}

#[test]
fn events_paging_and_search() {
    let t = with_steve();
    let s = &t.store;
    let ses = new_session(s, None, "t");
    t.events.take();
    let data = json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "你好 needle"}});
    let first = s
        .events()
        .add(&ses.id, "agent_message_chunk", data.clone())
        .unwrap();
    let seqs: Vec<i64> = (0..4)
        .map(|n| {
            s.events()
                .add(&ses.id, "user", json!({"text": n, "via": "me"}))
                .unwrap()
        })
        .collect();
    let evs = t.events.take();
    assert_eq!(evs.len(), 5);
    assert_eq!(
        evs[0],
        ChangeEvent::Event {
            session_id: ses.id.clone(),
            seq: first,
            kind: "agent_message_chunk".into(),
            data: data.clone()
        }
    );
    // the stored text keeps key order (preserve_order)
    s.with_conn(|c| {
        let d: String = c.query_row("SELECT data FROM events WHERE seq = ?", [first], |r| r.get(0))?;
        Ok(assert_eq!(d, r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"你好 needle"}}"#))
    })
    .unwrap();
    assert_eq!(s.events().after(&ses.id, 0, 5000).unwrap().len(), 5);
    assert_eq!(
        s.events()
            .after(&ses.id, first, 2)
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect::<Vec<_>>(),
        seqs[..2]
    );
    assert_eq!(s.events().up_to(&ses.id, seqs[0]).unwrap().len(), 2);
    assert_eq!(s.events().after(&ses.id, 0, 1).unwrap()[0].data, data);
    assert_eq!(s.sessions().search(WS, "needle").unwrap(), [ses.id.clone()]);
    assert!(s.sessions().search("other", "needle").unwrap().is_empty());
}

#[test]
fn queued() {
    let t = with_steve();
    let s = &t.store;
    let ses = new_session(s, None, "t");
    t.events.take();
    s.queued().add(&ses.id, "一", "me", &[]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    s.queued().add(&ses.id, "二", "steve", &[]).unwrap();
    let q = s.queued().list(&ses.id).unwrap();
    assert_eq!(
        q.iter().map(|q| q.text.as_str()).collect::<Vec<_>>(),
        ["一", "二"]
    );
    assert_eq!(
        s.queued().get(&q[0].id).unwrap().unwrap().session_id,
        ses.id
    );
    s.queued().drop(&q[0].id).unwrap();
    s.queued().drop("q_missing").unwrap();
    let ev = ChangeEvent::Session {
        id: ses.id.clone(),
        item_id: None,
    };
    assert_eq!(t.events.take(), vec![ev.clone(), ev.clone(), ev]);
    assert_eq!(
        serde_json::to_string(&ChangeEvent::Session {
            id: "x".into(),
            item_id: None
        })
        .unwrap(),
        r#"{"type":"session","id":"x"}"#
    );
    let taken = s.queued().take(&ses.id).unwrap();
    assert_eq!(taken.len(), 1);
    assert!(s.queued().list(&ses.id).unwrap().is_empty() && t.events.take().is_empty());
}

/// Edit, remove and move-to-front touch only a row still waiting in that session's queue: once a turn
/// took it they change nothing and say so (`None`), so the API can answer 409.
#[test]
fn queued_edit_remove_front() {
    let t = with_steve();
    let s = &t.store;
    let ses = new_session(s, None, "t");
    let other = new_session(s, None, "o");
    for x in ["一", "二", "三"] {
        s.queued().add(&ses.id, x, "me", &[]).unwrap();
    }
    let texts = |s: &steve_store::Store| {
        s.queued()
            .list(&ses.id)
            .unwrap()
            .into_iter()
            .map(|q| q.text)
            .collect::<Vec<_>>()
    };
    let q = s.queued().list(&ses.id).unwrap();
    t.events.take();

    let (before, after) = s.queued().edit(&ses.id, &q[1].id, "二改").unwrap().unwrap();
    assert_eq!((before.text.as_str(), after.text.as_str()), ("二", "二改"));
    assert_eq!(texts(s), ["一", "二改", "三"]);
    // another session's id does not reach this row
    assert!(s.queued().edit(&other.id, &q[1].id, "x").unwrap().is_none());
    assert!(s.queued().remove(&other.id, &q[1].id).unwrap().is_none());

    // to the front, the rest in order; the first stays first
    assert_eq!(
        s.queued()
            .to_front(&ses.id, &q[2].id)
            .unwrap()
            .unwrap()
            .text,
        "三"
    );
    assert_eq!(texts(s), ["三", "一", "二改"]);
    assert_eq!(
        s.queued()
            .to_front(&ses.id, &q[2].id)
            .unwrap()
            .unwrap()
            .text,
        "三"
    );
    assert_eq!(texts(s), ["三", "一", "二改"]);

    assert_eq!(
        s.queued().remove(&ses.id, &q[0].id).unwrap().unwrap().text,
        "一"
    );
    assert_eq!(texts(s), ["三", "二改"]);
    assert_eq!(
        t.events.take().len(),
        3,
        "edit, front and remove publish the session"
    );

    // a turn takes the queue: every later edit/remove/front is a miss and changes nothing
    let taken = s.queued().take(&ses.id).unwrap();
    assert_eq!(
        taken.iter().map(|q| q.text.as_str()).collect::<Vec<_>>(),
        ["三", "二改"]
    );
    assert!(
        s.queued()
            .edit(&ses.id, &q[1].id, "晚了")
            .unwrap()
            .is_none()
    );
    assert!(s.queued().remove(&ses.id, &q[1].id).unwrap().is_none());
    assert!(s.queued().to_front(&ses.id, &q[1].id).unwrap().is_none());
    assert!(texts(s).is_empty() && t.events.take().is_empty());
}

#[test]
fn queued_add_unless_waiting() {
    let t = with_steve();
    let s = &t.store;
    let ses = new_session(s, None, "t");
    assert!(
        s.queued()
            .add_unless_waiting(&ses.id, "验收通过", "steve", &[])
            .unwrap()
    );
    t.events.take();
    assert!(
        !s.queued()
            .add_unless_waiting(&ses.id, "验收通过", "me", &[])
            .unwrap()
    );
    assert!(
        t.events.take().is_empty(),
        "nothing added, nothing published"
    );
    assert!(
        s.queued()
            .add_unless_waiting(&ses.id, "另一条", "me", &[])
            .unwrap()
    );
    let q = s.queued().list(&ses.id).unwrap();
    assert_eq!(
        q.iter()
            .map(|q| (q.text.as_str(), q.via.as_str()))
            .collect::<Vec<_>>(),
        [("验收通过", "steve"), ("另一条", "me")]
    );
    // once taken, the same text queues again
    s.queued().take(&ses.id).unwrap();
    assert!(
        s.queued()
            .add_unless_waiting(&ses.id, "验收通过", "me", &[])
            .unwrap()
    );
}

#[test]
fn imported_sessions() {
    let t = with_steve();
    let s = &t.store;
    let p = ImportedSession {
        workspace_id: WS.into(),
        harness: "claude-code".into(),
        cwd: "/w".into(),
        title: "旧会话".into(),
        source: "本机 Claude Code".into(),
        import_key: "cc:/f.jsonl".into(),
        created: 5,
    };
    let id = s.sessions().insert_imported(&p).unwrap();
    assert!(t.events.take().is_empty());
    let got = s.sessions().get(&id).unwrap().unwrap();
    assert_eq!(
        (
            got.readonly,
            got.no,
            got.status.as_str(),
            got.created,
            got.import_key.as_deref()
        ),
        (1, 0, "ended", 5, Some("cc:/f.jsonl"))
    );
    assert!(
        s.sessions().is_imported("cc:/f.jsonl").unwrap()
            && s.sessions().import_keys().unwrap().contains("cc:/f.jsonl")
    );
    s.events()
        .add(&id, "user", json!({"text": "x", "via": "me"}))
        .unwrap();
    s.queued().add(&id, "q", "me", &[]).unwrap();
    t.events.take();
    s.sessions().remove(&id).unwrap();
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Session {
            id: id.clone(),
            item_id: None
        }]
    );
    assert!(s.sessions().get(&id).unwrap().is_none());
}

#[test]
fn decisions() {
    let t = with_steve();
    let s = &t.store;
    let i = new_item(s, WS, "a");
    t.events.take();
    let nd = |item: Option<&str>| NewDecision {
        workspace_id: WS.into(),
        kind: "accept".into(),
        subject_type: "item".into(),
        subject_id: i.id.clone(),
        item_id: item.map(Into::into),
        question: "验收？".into(),
        options: Some(vec!["通过".into(), "继续".into()]),
        requested_by: "steve".into(),
        ..Default::default()
    };
    let d = s.decisions().create(&nd(Some(&i.id))).unwrap().unwrap();
    let j = serde_json::to_value(&d).unwrap();
    assert_eq!(
        (
            j["options"].clone(),
            j["result"].clone(),
            j["undo"].clone(),
            j["status"].clone(),
            j["draft"].clone()
        ),
        (
            json!(["通过", "继续"]),
            Value::Null,
            Value::Null,
            json!("open"),
            json!("")
        )
    );
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Decision {
            id: d.id.clone(),
            item_id: Some(Some(i.id.clone()))
        }]
    );
    assert_eq!(s.decisions().open(WS).unwrap().len(), 1);

    let undo = json!({"status": "wait", "now": "交付了，等你验收", "stage": "交付与验收"});
    let r = s
        .decisions()
        .resolve(&d.id, &json!({"choice": "pass"}), "resolved", &undo)
        .unwrap()
        .unwrap();
    assert_eq!(
        (r.status.as_str(), r.result.clone(), r.undo.clone()),
        ("resolved", json!({"choice": "pass"}), undo.clone())
    );
    assert!(r.resolved.is_some());
    // resolving again is a no-op (only open decisions change) but still publishes
    let again = s
        .decisions()
        .resolve(
            &d.id,
            &json!({"choice": "continue"}),
            "resolved",
            &Value::Null,
        )
        .unwrap()
        .unwrap();
    assert_eq!(again.result, json!({"choice": "pass"}));
    assert_eq!(t.events.take().len(), 2);
    assert!(s.decisions().open(WS).unwrap().is_empty());
    assert_eq!(s.decisions().recent(WS, 0).unwrap().len(), 1);
    assert!(s.decisions().recent(WS, now() + 1000).unwrap().is_empty());

    let back = s.decisions().reopen(&d.id).unwrap().unwrap();
    assert_eq!(
        (
            back.status.as_str(),
            back.result.clone(),
            back.resolved,
            back.undo.clone()
        ),
        ("open", Value::Null, None, Value::Null)
    );
    // a falsy undo is stored as NULL
    let e = s
        .decisions()
        .resolve(&d.id, &json!({"expired": true}), "expired", &json!(false))
        .unwrap()
        .unwrap();
    assert_eq!(
        (e.status.as_str(), e.undo.clone()),
        ("expired", Value::Null)
    );
    s.with_conn(|c| {
        Ok(assert_eq!(
            count(c, "SELECT count(*) FROM decisions WHERE undo IS NULL"),
            1
        ))
    })
    .unwrap();
    assert_eq!(s.decisions().of_item(&i.id).unwrap().len(), 1);

    t.events.take();
    let free = s.decisions().create(&nd(None)).unwrap().unwrap();
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Decision {
            id: free.id.clone(),
            item_id: Some(None)
        }]
    );
    s.decisions().reopen("dec_missing").unwrap();
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Decision {
            id: "dec_missing".into(),
            item_id: None
        }]
    );
}

#[test]
fn messages() {
    let t = with_steve();
    let s = &t.store;
    let ids: Vec<String> = (0..3)
        .map(|n| {
            std::thread::sleep(std::time::Duration::from_millis(2));
            s.messages()
                .add(
                    WS,
                    if n == 1 { "steve" } else { "me" },
                    &format!("m{n}"),
                    None,
                    &Value::Null,
                    "steve",
                )
                .unwrap()
        })
        .collect();
    assert_eq!(
        t.events.take(),
        ids.iter()
            .map(|id| ChangeEvent::Message { id: id.clone() })
            .collect::<Vec<_>>()
    );
    let last2 = s.messages().list(WS, "steve", 2).unwrap();
    assert_eq!(
        last2.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        ["m1", "m2"],
        "newest N, oldest first"
    );
    assert_eq!(
        s.messages().last_of(WS, "steve").unwrap().unwrap().id,
        ids[1]
    );
    let m = s.messages().get(&ids[0]).unwrap().unwrap();
    assert_eq!(m.meta, Value::Null);
    s.messages()
        .patch_meta(&ids[0], &json!({"promoted": "ST-6"}))
        .unwrap();
    assert_eq!(
        s.messages().get(&ids[0]).unwrap().unwrap().meta,
        json!({"promoted": "ST-6"})
    );
    let id = s
        .messages()
        .add(WS, "steve", "x", Some("ST-1"), &json!({"k": 1}), "other")
        .unwrap();
    let m = s.messages().get(&id).unwrap().unwrap();
    assert_eq!(
        (m.space.as_str(), m.item_id.as_deref(), m.meta.clone()),
        ("other", Some("ST-1"), json!({"k": 1}))
    );
    assert_eq!(s.messages().list(WS, "steve", 200).unwrap().len(), 3);
}

#[test]
fn readers_run_in_parallel_with_writes() {
    let t = with_steve();
    let s = t.store.clone();
    let ses = new_session(&s, None, "t");
    let writer = {
        let (s, id) = (s.clone(), ses.id.clone());
        std::thread::spawn(move || {
            for n in 0..300 {
                s.events().add(&id, "user", json!({"n": n})).unwrap();
            }
        })
    };
    let readers: Vec<_> = (0..6)
        .map(|_| {
            let (s, id) = (s.clone(), ses.id.clone());
            std::thread::spawn(move || {
                let mut last = 0;
                for _ in 0..100 {
                    let n = s.events().after(&id, 0, 5000).unwrap().len();
                    assert!(n >= last, "a reader never sees fewer rows than before");
                    last = n;
                }
            })
        })
        .collect();
    writer.join().unwrap();
    for r in readers {
        r.join().unwrap();
    }
    assert_eq!(s.events().after(&ses.id, 0, 5000).unwrap().len(), 300);
    // readers are read-only connections
    assert!(t.store.events().after(&ses.id, 0, 1).is_ok());
}

#[test]
fn item_deleted_hook_runs_in_the_delete_transaction() {
    let t = with_steve();
    let s = &t.store;
    let keep = new_item(s, WS, "keep");
    let gone = new_item(s, WS, "go");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (log, refuse) = (seen.clone(), keep.id.clone());
    assert!(s.on_item_deleted(move |tx, ws, id| {
        // the row is already gone inside the same transaction
        let n: i64 = tx
            .query_row("SELECT COUNT(*) FROM items WHERE id = ?", [id], |r| {
                r.get(0)
            })
            .unwrap();
        log.lock()
            .unwrap()
            .push((ws.to_string(), id.to_string(), n));
        if id == refuse {
            return Err("不许删".into());
        }
        Ok(())
    }));
    assert!(!s.on_item_deleted(|_, _, _| Ok(())));
    s.items().remove(&gone.id).unwrap();
    assert!(s.items().get(&gone.id).unwrap().is_none());
    // a refusing hook rolls the delete back
    let e = s.items().remove(&keep.id).unwrap_err();
    assert_eq!(e.to_string(), "不许删");
    assert!(s.items().get(&keep.id).unwrap().is_some());
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![
            (WS.to_string(), gone.id.clone(), 0),
            (WS.to_string(), keep.id.clone(), 0)
        ]
    );
}

#[test]
fn injected_values_are_scrubbed_from_events_and_timeline() {
    let t = with_steve();
    let s = &t.store;
    let item = new_item(s, WS, "secret");
    let ses = new_session(s, Some(&item.id), "tok-secret");
    steve_vault::redact::register(&ses.id, "GitHub·token", b"ghp_injectedvalue0123456789");
    let seq = s
        .events()
        .add(
            &ses.id,
            "tool",
            json!({"out": "token=ghp_injectedvalue0123456789 ok"}),
        )
        .unwrap();
    let ev = s.events().after(&ses.id, seq - 1, 10).unwrap();
    let text = serde_json::to_string(&ev[0].data).unwrap();
    assert!(!text.contains("ghp_injectedvalue0123456789"), "{text}");
    s.timeline()
        .add(
            &item.id,
            "steve",
            "agent",
            "用了 ghp_injectedvalue0123456789",
            Some(&ses.id),
            &[],
        )
        .unwrap();
    let tl = s.timeline().list(&item.id).unwrap();
    assert!(
        tl.iter()
            .all(|e| !e.text.contains("ghp_injectedvalue0123456789"))
    );
    // without a session nothing is touched
    s.timeline()
        .add(&item.id, "me", "me", "plain", None, &[])
        .unwrap();
    s.sessions().remove(&ses.id).unwrap();
    assert!(!steve_vault::redact::is_registered(&ses.id));
}

#[test]
fn durable_dispatch_survives_claim_and_repeated_recovery_with_attachments() {
    let t = with_steve();
    let s = new_session(&t.store, None, "token");
    let q = t.store.queued();
    let files = vec![FileRef {
        id: "file-1".into(),
        name: "证据.png".into(),
        mime: "image/png".into(),
        size: 123,
    }];
    q.add(&s.id, "不能丢的输入", "me", &files).unwrap();
    let claimed = q.claim(&s.id).unwrap();
    assert_eq!(claimed.len(), 1);
    assert!(q.list(&s.id).unwrap().is_empty());
    q.recover_dispatches().unwrap();
    q.recover_dispatches().unwrap();
    let again = q.claim(&s.id).unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].text, claimed[0].text);
    assert_eq!(again[0].via, "me");
    assert_eq!(again[0].files, files);
    q.plan_resume(&s.id).unwrap();
    assert_eq!(q.resume_ids().unwrap(), vec![s.id.clone()]);
    q.submitted(&s.id).unwrap();
    assert!(q.resume_ids().unwrap().is_empty());
    q.recover_dispatches().unwrap();
    assert_eq!(q.resume_ids().unwrap(), vec![s.id.clone()]);
    let again = q.claim(&s.id).unwrap();
    assert_eq!(again.len(), 1);
    q.submitted(&s.id).unwrap();
    q.receipt(&s.id, "completed").unwrap();
    q.recover_dispatches().unwrap();
    assert!(
        q.list(&s.id).unwrap().is_empty(),
        "completed dispatch must never replay"
    );
}

#[test]
fn durable_resume_and_dispatch_are_visible_after_reopening_the_database() {
    let t = with_steve();
    let s = new_session(&t.store, None, "token");
    t.store.queued().add(&s.id, "reopen", "me", &[]).unwrap();
    t.store.queued().claim(&s.id).unwrap();
    t.store.queued().plan_resume(&s.id).unwrap();
    let reopened = Store::open(t.dir.path().join("steve.sqlite3"), FnPublisher(|_| {})).unwrap();
    reopened.queued().recover_dispatches().unwrap();
    assert_eq!(reopened.queued().list(&s.id).unwrap()[0].text, "reopen");
    assert_eq!(reopened.queued().resume_ids().unwrap(), vec![s.id]);
}

#[test]
fn an_unsubmitted_claim_can_be_deferred_without_restarting() {
    let t = with_steve();
    let s = new_session(&t.store, None, "token");
    let q = t.store.queued();
    q.add(&s.id, "storage temporarily unavailable", "me", &[])
        .unwrap();
    q.claim(&s.id).unwrap();
    q.defer_claimed(&s.id).unwrap();
    q.defer_claimed(&s.id).unwrap();
    assert_eq!(q.list(&s.id).unwrap().len(), 1);
    assert_eq!(
        q.claim(&s.id).unwrap()[0].text,
        "storage temporarily unavailable"
    );
    q.submitted(&s.id).unwrap();
    q.defer_claimed(&s.id).unwrap();
    assert!(
        q.list(&s.id).unwrap().is_empty(),
        "ambiguous submission must not be treated as unsent"
    );
}

#[test]
fn a_message_submitted_before_the_stop_comes_back_marked_never_as_a_new_request() {
    let t = with_steve();
    let s = new_session(&t.store, None, "token");
    let q = t.store.queued();
    q.add(&s.id, "发布一下", "me", &[]).unwrap();
    q.claim(&s.id).unwrap();
    q.submitted(&s.id).unwrap();
    // the Hub stops with the turn submitted, twice
    q.recover_dispatches().unwrap();
    let back = q.list(&s.id).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(
        back[0].via, "resumed:me",
        "marked: delivered before, to be checked"
    );
    q.claim(&s.id).unwrap();
    q.submitted(&s.id).unwrap();
    q.recover_dispatches().unwrap();
    assert_eq!(
        q.list(&s.id).unwrap()[0].via,
        "resumed:me",
        "never marked twice"
    );
    // a claim that was never submitted comes back unmarked
    let s2 = new_session(&t.store, None, "token2");
    q.add(&s2.id, "还没送出", "me", &[]).unwrap();
    q.claim(&s2.id).unwrap();
    q.recover_dispatches().unwrap();
    assert_eq!(q.list(&s2.id).unwrap()[0].via, "me");
}

#[test]
fn a_resume_intent_says_whether_it_is_new() {
    let t = with_steve();
    let s = new_session(&t.store, None, "token");
    let q = t.store.queued();
    assert!(q.plan_resume(&s.id).unwrap());
    assert!(
        !q.plan_resume(&s.id).unwrap(),
        "already recorded by an earlier boot"
    );
    assert!(q.resume_created(&s.id).unwrap().is_some());
    q.resume_done(&s.id).unwrap();
    assert!(q.resume_created(&s.id).unwrap().is_none());
}

#[tokio::test]
async fn maintenance_admission_is_persistent_and_waiters_resume_on_cancel() {
    let t = with_steve();
    let p = t.store.try_activity("mail_send").unwrap();
    t.store.maintenance_set(true).unwrap();
    assert!(t.store.try_activity("mail_send").is_none());
    assert_eq!(t.store.activities()["mail_send"], 1);
    let reopened = Store::open(t.dir.path().join("steve.sqlite3"), FnPublisher(|_| {})).unwrap();
    assert!(reopened.maintenance());
    assert!(reopened.try_activity("flow").is_none());
    drop(p);
    assert!(t.store.activities().is_empty());
    let s = t.store.clone();
    let waiter = tokio::spawn(async move { s.activity("thread_answer").await });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    t.store.maintenance_set(false).unwrap();
    let p = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.store.activities()["thread_answer"], 1);
    drop(p);
    assert!(t.store.activities().is_empty());
}

#[tokio::test]
async fn maintenance_allows_nested_work_only_in_an_already_admitted_task() {
    let t = with_steve();
    let p = t.store.try_activity("flow").unwrap();
    t.store
        .admitted(async {
            t.store.maintenance_set(true).unwrap();
            let nested = t
                .store
                .try_activity("mail_send")
                .expect("admitted flow may finish its mail node");
            let unrelated = t.store.clone();
            assert!(
                tokio::spawn(async move { unrelated.try_activity("mail_send").is_none() })
                    .await
                    .unwrap()
            );
            drop(nested);
        })
        .await;
    drop(p);
    assert!(t.store.activities().is_empty());
    assert!(t.store.try_activity("mail_send").is_none());
}

#[tokio::test]
async fn sealed_drain_refuses_late_running_tokens_until_explicit_cancel() {
    let t = with_steve();
    assert!(
        !t.store.maintenance_seal(),
        "normal operation cannot be sealed"
    );
    t.store.maintenance_set(true).unwrap();
    let existing = t
        .store
        .continuing_activity("mcp_existing_turn", || true)
        .unwrap();
    assert!(
        !t.store.maintenance_seal(),
        "an admitted in-flight RPC must finish first"
    );
    drop(existing);
    assert!(t.store.maintenance_seal());
    // The session database may still say running. Sealing takes precedence over that stale fact.
    assert!(
        t.store
            .continuing_activity("mcp_existing_turn", || true)
            .is_none()
    );
    assert!(t.store.try_activity("mail_send").is_none());
    t.store
        .admitted(async {
            assert!(t.store.try_activity("nested_after_ready").is_none());
        })
        .await;
    t.store.maintenance_set(true).unwrap();
    assert!(
        t.store
            .continuing_activity("retried_post", || true)
            .is_none()
    );
    t.store.maintenance_set(false).unwrap();
    assert!(t.store.try_activity("mail_send").is_some());
    t.store.maintenance_set(true).unwrap();
    assert!(t.store.continuing_activity("new_drain", || true).is_some());
}

#[test]
fn records_and_annotations_drafts_one_send_per_key_and_removal() {
    let t = with_steve();
    let s = &t.store;
    let a = new_item(s, WS, "a");
    s.artifacts()
        .add_as(&a.id, "文件", "截图", Some("/tmp/shots"), Some("record"))
        .unwrap();
    s.artifacts()
        .add_as(&a.id, "文档", "方案", Some("docs/a.md"), Some("result"))
        .unwrap();
    let arts = s.artifacts().list(&a.id).unwrap();
    assert_eq!(
        s.artifacts().roles(&a.id).unwrap(),
        [
            (arts[0].id.clone(), "record".to_string()),
            (arts[1].id.clone(), "result".to_string())
        ],
        "what the agent said"
    );
    s.artifacts()
        .add_as(&a.id, "文件", "x", None, Some("other"))
        .unwrap();
    assert_eq!(
        s.artifacts().roles(&a.id).unwrap().len(),
        2,
        "only known roles"
    );

    let n = s.annotations();
    let region = json!({"type": "region", "x": 0.1, "y": 0.2, "w": 0.3, "h": 0.4});
    let one = n.add(&a.id, &arts[1].id, "", &region, "改一下").unwrap();
    let two = n
        .add(
            &a.id,
            &arts[0].id,
            "a.png",
            &json!({"type": "file"}),
            "偏暗",
        )
        .unwrap();
    assert_eq!((one.status.as_str(), one.anchor.clone()), ("draft", region));
    assert_eq!(
        n.edit(&a.id, &one.id, "改成这样").unwrap().unwrap().note,
        "改成这样"
    );
    assert!(
        n.edit("other", &one.id, "x").unwrap().is_none(),
        "scoped to the item"
    );

    let ids = vec![one.id.clone(), two.id.clone()];
    let c = n.claim(&a.id, &ids, "k1").unwrap();
    assert_eq!((c.again, c.taken.len()), (false, 2));
    assert!(
        n.edit(&a.id, &one.id, "x").unwrap().is_none(),
        "a draft being sent is not edited"
    );
    assert!(!n.remove(&a.id, &one.id).unwrap());
    // the same send again takes nothing new; another send finds nothing left
    let again = n.claim(&a.id, &ids, "k1").unwrap();
    assert_eq!((again.again, again.taken.len()), (true, 2));
    assert!(n.claim(&a.id, &ids, "k2").unwrap().taken.is_empty());

    // a failed send gives the drafts back
    n.settle(&a.id, "k1", false, None).unwrap();
    assert!(
        n.list(&a.id)
            .unwrap()
            .iter()
            .all(|x| x.status == "draft" && x.send_key.is_none())
    );
    let c = n.claim(&a.id, &ids, "k3").unwrap();
    assert_eq!(c.taken.len(), 2);
    n.settle(&a.id, "k3", true, Some(7)).unwrap();
    let l = n.list(&a.id).unwrap();
    assert!(
        l.iter()
            .all(|x| x.status == "sent" && x.timeline_id == Some(7) && x.sent.is_some())
    );
    assert!(!n.remove(&a.id, &two.id).unwrap(), "what was sent stays");

    s.items().remove(&a.id).unwrap();
    assert!(n.list(&a.id).unwrap().is_empty());
    assert!(s.artifacts().roles(&a.id).unwrap().is_empty());
}

#[test]
fn an_unapplied_answer_expires_its_card_open_or_resolved_only() {
    let t = with_steve();
    let s = &t.store;
    let i = new_item(s, WS, "a");
    let nd = || NewDecision {
        workspace_id: WS.into(),
        kind: "approve".into(),
        subject_type: "tool_call".into(),
        subject_id: "p1".into(),
        item_id: Some(i.id.clone()),
        question: "允许？".into(),
        requested_by: "steve".into(),
        ..Default::default()
    };
    let status = |id: &str| s.decisions().get(id).unwrap().unwrap().status;
    let open = s.decisions().create(&nd()).unwrap().unwrap();
    let resolved = s.decisions().create(&nd()).unwrap().unwrap();
    let expired = s.decisions().create(&nd()).unwrap().unwrap();
    let cancelled = s.decisions().create(&nd()).unwrap().unwrap();
    s.transaction::<_, StoreError>(|tx| {
        decision_resolve_in(
            tx,
            &resolved.id,
            &json!({"optionId": "allow_once"}),
            "resolved",
        )?;
        decision_resolve_in(tx, &expired.id, &json!({}), "expired")?;
        decision_resolve_in(tx, &cancelled.id, &json!({}), "cancelled")?;
        let r = json!({"expired": true, "refused": "expired"});
        assert!(decision_expire_unapplied_in(tx, &open.id, &r)?);
        assert!(decision_expire_unapplied_in(tx, &resolved.id, &r)?);
        assert!(!decision_expire_unapplied_in(tx, &expired.id, &r)?);
        assert!(!decision_expire_unapplied_in(tx, &cancelled.id, &r)?);
        Ok(())
    })
    .unwrap();
    assert_eq!(status(&open.id), "expired");
    assert_eq!(status(&resolved.id), "expired");
    assert_eq!(status(&cancelled.id), "cancelled");
    let d = s.decisions().get(&resolved.id).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&d).unwrap()["result"]["refused"],
        "expired"
    );
}
