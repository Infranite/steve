//! K3 acceptance, with fixed gateway output: an item completion yields at most 5
//! candidates, `dup_of` ones are dropped, a “关于” nobody can be resolved to is dropped; at most 20
//! memory decisions are open; confirming with `supersedes` makes the old memory stale; a batch confirm
//! leaves decisions consistent; candidates expire after 30 days. Plus the chat source, the 待确认 list
//! and `steve_remember`.

use std::sync::Arc;

use axum::Router;
use serde_json::{Value, json};
use steve_knowledge::candidates::{
    self, EXPIRE_AFTER_MS, MAX_PER_ORIGIN, MemDecisions, NewCandidate, Origin, OriginKind, SAVED,
};
use steve_knowledge::extract::PROMPT_HEAD;
use steve_knowledge::retrieve::{Purpose, Query, retrieve};
use steve_knowledge::sources::{chat, items};
use steve_knowledge::{Knowledge, NoGuard};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::decisions;
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, call, caller};
use steve_store::base::mems::{self, Mem};
use steve_store::base::{Level, ObjRef, Viewer};

struct Env {
    env: TestEnv,
    kb: Knowledge,
    app: Router,
}

fn setup() -> Env {
    let env = TestEnv::new(|r| {
        r.decision(candidates::SUBJECT, Box::new(MemDecisions));
    });
    let kb = Knowledge::new(Arc::new(NoGuard), |_| {});
    let app = steve_knowledge::routes(kb.clone()).with_state(env.modules.clone());
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO parties (id, workspace_id, type, name, aliases, created_by, created, updated) VALUES
                   ('ct_sarah', 'default', 'contact', 'Sarah Chen', '[\"Sarah\"]', 'usr_dev', 1, 1),
                   ('org_acme', 'default', 'org', 'Acme', '[]', 'usr_dev', 1, 1);
                 INSERT INTO items (id, workspace_id, no, title, status, stage, cwd, created, updated)
                   VALUES ('ST-12', 'default', 12, '给 Acme 写续签邮件', 'done', '完成', NULL, 1, 100);
                 INSERT INTO item_goals (item_id, version, text, created) VALUES
                   ('ST-12', 1, '给 Acme 写续签邮件', 1), ('ST-12', 2, '给 Acme 写续签邮件，抄送 Sarah', 2);
                 INSERT INTO timeline (item_id, ts, actor, kind, text) VALUES
                   ('ST-12', 3, 'usr_dev', 'me', 'Sarah 一直喜欢简短的邮件，别超过五行'),
                   ('ST-12', 4, 'usr_dev', 'me', '验收通过');
                 INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, question, draft, options, status, result, requested_by, created) VALUES
                   ('dec_ask', 'default', 'ask', 'question', 'ses_1', 'ST-12', '用中文还是英文？', '', '[]', 'resolved', '{\"answer\":\"Acme 的邮件一律用英文\"}', 'bot', 5),
                   ('dec_acc', 'default', 'accept', 'item', 'ST-12', 'ST-12', '做完了，请验收', '写好了续签邮件草稿，五行以内', '[\"pass\",\"rework\"]', 'resolved', '{\"choice\":\"pass\"}', 'bot', 6);
                 INSERT INTO artifacts (id, item_id, kind, title, location, created) VALUES ('art_1', 'ST-12', 'doc', '邮件草稿', NULL, 7);
                 INSERT INTO audit (workspace_id, ts, actor, action, target, detail) VALUES ('default', 1, 'usr_dev', 'item.create', 'ST-12', '{}');",
            )?;
            Ok(())
        })
        .expect("seed");
    Env { env, kb, app }
}

impl Env {
    async fn me(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        call(
            &self.app,
            Some(&caller(WS, ME)),
            method,
            path,
            body.as_ref(),
        )
        .await
    }
    fn mem(&self, id: &str) -> Option<Mem> {
        self.env.store().reader(|c| mems::get(c, id)).unwrap()
    }
    fn open_confirms(&self) -> i64 {
        self.env
            .store()
            .reader(|c| steve_modules::contacts::people_repo::open_mem_confirms(c, WS))
            .unwrap()
    }
    fn decision(&self, id: &str) -> steve_store::Decision {
        self.env
            .store()
            .reader(|c| steve_store::base::decisions::get(c, id))
            .unwrap()
            .unwrap()
    }
    fn origin(&self, n: usize) -> Origin {
        Origin {
            ws: WS.into(),
            kind: OriginKind::Item,
            id: format!("ST-{n}"),
            label: format!("ST-{n} 交付时"),
            user: ME.into(),
            writer: "steve".into(),
            default_level: Level::Conclusion,
            item_id: None,
            session_id: None,
        }
    }
    /// `n` candidates written directly (no model), one origin each.
    fn propose(&self, n: usize) -> Vec<Mem> {
        self.env
            .store()
            .transaction(|tx| {
                (0..n)
                    .map(|i| {
                        candidates::propose(
                            tx,
                            &self.origin(100 + i),
                            &NewCandidate {
                                kind: "fact".into(),
                                text: format!("第 {i} 条候选"),
                                about: vec![],
                                level: Level::Conclusion,
                                evidence: String::new(),
                                supersedes: None,
                                why: String::new(),
                                scope: String::new(),
                            },
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap()
    }
}

fn item_json(extra: Value) -> String {
    let mut items = vec![
        json!({"kind":"preference","text":"Sarah 喜欢简短的邮件，不超过五行","about":["Sarah Chen"],"level":"conclusion","evidence":"Sarah 一直喜欢简短的邮件，别超过五行","basis":"explicit","dup_of":null,"conflicts":null,"why":"以后写信用","scope":"给 Sarah 写邮件时"}),
        json!({"kind":"rule","text":"给 Acme 的邮件一律用英文","about":["Acme"],"level":"public","evidence":["Acme 的邮件一律用英文"],"basis":"explicit","dup_of":null,"conflicts":null}),
        json!({"kind":"fact","text":"重复的一条","about":[],"evidence":"五行以内","dup_of":"mm_whatever"}),
        json!({"kind":"preference","text":"用户希望续签邮件抄送 Sarah","about":[],"evidence":"给 Acme 写续签邮件，抄送 Sarah","basis":"explicit"}),
        json!({"kind":"fact","text":"没有原话的一条","about":[]}),
        json!({"kind":"fact","text":"王五负责 Acme 的采购","about":["王五"],"evidence":"五行以内"}),
        json!({"kind":"fact","text":"关于 Sarah 和王五","about":["Sarah","王五"],"level":"reasoning","evidence":"写好了续签邮件草稿"}),
    ];
    for i in 0..5 {
        items.push(json!({"kind":"fact","text":format!("额外的第 {i} 条"),"about":[],"evidence":"五行以内"}));
    }
    if let Value::Array(x) = extra {
        items.splice(0..0, x);
    }
    format!("```json\n{}\n```", json!({ "items": items }))
}

#[tokio::test]
async fn item_completion_yields_at_most_three_filtered_candidates() {
    let e = setup();
    e.env.gateway.script(PROMPT_HEAD, item_json(json!([])));
    let got = items::finished(&e.env.modules, &e.kb, "ST-12")
        .await
        .unwrap();

    // ≤3; the dup, the one-off request that restates the goal, the one without a quote and the
    // unresolvable “关于” dropped; the partly-resolved one kept with what resolved.
    assert_eq!(got.len(), MAX_PER_ORIGIN, "{got:#?}");
    let texts: Vec<&str> = got.iter().map(|m| m.text.as_str()).collect();
    assert!(!texts.contains(&"重复的一条"));
    assert!(!texts.contains(&"用户希望续签邮件抄送 Sarah"));
    assert!(!texts.contains(&"没有原话的一条"));
    assert!(!texts.contains(&"王五负责 Acme 的采购"));
    assert_eq!(texts[0], "Sarah 喜欢简短的邮件，不超过五行");
    assert_eq!(got[0].about, vec![ObjRef::new("party", "ct_sarah")]);
    assert_eq!(got[1].about, vec![ObjRef::new("party", "org_acme")]);
    assert_eq!(
        got[1].level,
        Level::Conclusion,
        "public is raised to the item default"
    );
    assert_eq!(texts[2], "关于 Sarah 和王五");
    assert_eq!(got[2].about, vec![ObjRef::new("party", "ct_sarah")]);
    assert_eq!(got[2].level, Level::Reasoning);
    for m in &got {
        assert_eq!(m.status, "candidate");
        assert_eq!(m.created_by, ME);
        assert_eq!(m.visibility.code(), "private");
        assert_eq!(m.source_type, "item");
        assert_eq!(m.source_id.as_deref(), Some("ST-12"));
        assert_eq!(m.writer, "steve");
        let d = e.decision(m.decision_id.as_deref().unwrap());
        assert_eq!(d.kind, "confirm");
        assert_eq!(d.subject_type, "mem");
        assert_eq!(d.status, "open");
        let what = if m.kind == "fact" {
            "知识条目"
        } else {
            "长期记忆"
        };
        assert_eq!(d.question, format!("存为{what}吗？{}", m.text));
        assert!(d.draft.starts_with(&format!("类型：{what}")), "{}", d.draft);
        assert!(d.draft.contains("\n来源：ST-12 交付时"), "{}", d.draft);
    }
    let first = e.decision(got[0].decision_id.as_deref().unwrap()).draft;
    assert!(
        first.contains("\n原话：Sarah 一直喜欢简短的邮件，别超过五行"),
        "{first}"
    );
    assert!(
        first.starts_with(
            "类型：长期记忆 · 偏好\n为什么值得保留：以后写信用\n适用：给 Sarah 写邮件时\n"
        ),
        "{first}"
    );
    // The 待确认 list says what each one is and why it is worth keeping.
    let (_, list) = e.me("GET", "/kb/candidates", None).await;
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["mem"]["id"] == got[0].id.as_str())
        .unwrap();
    assert_eq!(row["record"], "长期记忆");
    assert_eq!(row["why"], "以后写信用");
    assert_eq!(row["scope"], "给 Sarah 写邮件时");

    // The prompt had the goals, what you said, the answer, the summary and the names.
    let p = e.env.gateway.last_prompt().unwrap();
    for s in [
        "ST-12 交付时",
        "目标 v2：给 Acme 写续签邮件，抄送 Sarah",
        "属于事项记录",
        "你补充：Sarah 一直喜欢简短的邮件",
        "你的回答：Acme 的邮件一律用英文",
        "交付摘要：写好了续签邮件草稿",
        "- Sarah Chen（联系人）",
        "- Acme（组织）",
    ] {
        assert!(p.contains(s), "missing {s} in\n{p}");
    }
    assert!(!p.contains("你补充：验收通过"));

    // The item document is in the index (workspace-wide, so the other user finds it too).
    let viewer = Viewer::bare(WS, OTHER);
    let hits = e
        .env
        .store()
        .reader(|c| {
            retrieve(
                c,
                &Query::new(
                    &steve_knowledge::port::SqliteStore,
                    &viewer,
                    "续签邮件",
                    Purpose::Search,
                ),
            )
        })
        .unwrap();
    assert!(
        hits.iter()
            .any(|h| h.source_kind == "items" && h.href == "#/items/ST-12"),
        "{hits:#?}"
    );

    // Once per completion: the same completion again does not call the model.
    let calls = e.env.gateway.prompts.lock().unwrap().len();
    assert!(
        items::finished(&e.env.modules, &e.kb, "ST-12")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(e.env.gateway.prompts.lock().unwrap().len(), calls);
    // Reopened and finished again: extracted again.
    e.env
        .store()
        .with_conn(|c| {
            c.execute("UPDATE items SET updated = 200 WHERE id = 'ST-12'", [])?;
            Ok(())
        })
        .unwrap();
    let again = items::finished(&e.env.modules, &e.kb, "ST-12")
        .await
        .unwrap();
    // …but what is already waiting is not proposed twice: the pending ones are shown to the model
    // and the server drops what repeats them (the extra facts behind them move up).
    assert!(!again.is_empty());
    assert!(
        again.iter().all(|m| got.iter().all(|g| g.text != m.text)),
        "{again:#?}"
    );
    assert_eq!(e.env.gateway.prompts.lock().unwrap().len(), calls + 1);
    let p = e.env.gateway.last_prompt().unwrap();
    assert!(
        p.contains(&format!("[{}]（待确认） Sarah 喜欢简短的邮件", got[0].id)),
        "{p}"
    );
}

#[tokio::test]
async fn at_most_twenty_memory_decisions_are_open() {
    let e = setup();
    let made = e.propose(23);
    assert_eq!(e.open_confirms(), 20);
    assert_eq!(made.iter().filter(|m| m.decision_id.is_some()).count(), 20);
    assert!(made[20..].iter().all(|m| m.decision_id.is_none()));

    // All 23 are on the 待确认 page, newest first; the extra ones without a decision.
    let (s, v) = e.me("GET", "/kb/candidates", None).await;
    assert_eq!(s, 200, "{v}");
    let list = v.as_array().unwrap();
    assert_eq!(list.len(), 23);
    let without: Vec<&Value> = list.iter().filter(|c| c["decision_id"].is_null()).collect();
    assert_eq!(without.len(), 3);
    assert_eq!(list[0]["origin"], "item");
    assert!(
        list[0]["origin_label"]
            .as_str()
            .unwrap()
            .ends_with("交付时")
    );
    assert_eq!(list[0]["mem"]["status"], "candidate");
    assert!(list[0]["supersedes"].is_null());

    // Private to the user they are for.
    let (s, v) = call(
        &e.app,
        Some(&caller(WS, OTHER)),
        "GET",
        "/kb/candidates",
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(v, json!([]));
}

#[tokio::test]
async fn confirming_with_supersedes_makes_the_old_memory_stale() {
    let e = setup();
    let (s, old) = e
        .me(
            "POST",
            "/kb/mems",
            Some(
                json!({"text": "Sarah 喜欢很长很详细的邮件", "kind": "preference",
                        "about": [{"type": "party", "id": "ct_sarah"}]}),
            ),
        )
        .await;
    assert_eq!(s, 200, "{old}");
    let old_id = old["id"].as_str().unwrap().to_string();
    e.env.gateway.script(
        PROMPT_HEAD,
        json!({"items": [{"kind": "preference", "text": "Sarah 喜欢简短的邮件", "about": ["Sarah Chen"],
            "evidence": "Sarah 一直喜欢简短的邮件，别超过五行", "dup_of": null, "conflicts": old_id},
            {"kind": "fact", "text": "冲突对象不存在", "evidence": "五行以内", "conflicts": "mm_nope"}]})
        .to_string(),
    );
    let got = items::finished(&e.env.modules, &e.kb, "ST-12")
        .await
        .unwrap();
    assert_eq!(got.len(), 2);
    // The model saw the old memory as a similar confirmed one, by id.
    let p = e.env.gateway.last_prompt().unwrap();
    assert!(p.contains(&format!("[{old_id}]")), "{p}");

    let new = &got[0];
    let dec = new.decision_id.clone().unwrap();
    assert!(
        e.decision(&dec)
            .draft
            .contains("\n会替换：Sarah 喜欢很长很详细的邮件")
    );
    let (_, list) = e.me("GET", "/kb/candidates", None).await;
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["mem"]["id"] == new.id.as_str())
        .unwrap();
    assert_eq!(row["supersedes"]["id"], old_id.as_str());
    let other = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["mem"]["id"] == got[1].id.as_str())
        .unwrap();
    assert!(
        other["supersedes"].is_null(),
        "unknown conflicts are not kept"
    );

    // Answer it through the decision dispatch, with an edited wording.
    let d = decisions::dispatch(
        &e.env.modules,
        &caller(WS, ME),
        &dec,
        "yes",
        "Sarah 喜欢五行以内的邮件",
    )
    .unwrap()
    .unwrap();
    assert_eq!(d.status, "resolved");
    assert_eq!(
        d.result,
        json!({"choice": "yes", "text": "Sarah 喜欢五行以内的邮件"})
    );
    let now = e.mem(&new.id).unwrap();
    assert_eq!(now.status, "confirmed");
    assert_eq!(now.text, "Sarah 喜欢五行以内的邮件");
    assert_eq!(e.mem(&old_id).unwrap().status, "stale");

    // The index follows: the new one is searchable, the old one is not.
    let viewer = Viewer::bare(WS, ME);
    let hits = e
        .env
        .store()
        .reader(|c| {
            retrieve(
                c,
                &Query::new(
                    &steve_knowledge::port::SqliteStore,
                    &viewer,
                    "邮件",
                    Purpose::Search,
                ),
            )
        })
        .unwrap();
    assert!(hits.iter().any(|h| h.obj_id == new.id));
    assert!(!hits.iter().any(|h| h.obj_id == old_id));

    // Undoing the yes puts it back and restores the old memory.
    e.env
        .store()
        .transaction(|tx| {
            let d = steve_store::base::decisions::get(tx, &dec)?.unwrap();
            e.env.modules.registry.before_undo(tx, &d)
        })
        .unwrap();
    assert_eq!(e.mem(&new.id).unwrap().status, "candidate");
    assert_eq!(e.mem(&old_id).unwrap().status, "confirmed");
}

#[tokio::test]
async fn batch_confirm_keeps_decisions_consistent() {
    let e = setup();
    let made = e.propose(22);
    let ids: Vec<&str> = made.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(e.open_confirms(), 20);

    // Validation and ownership before anything changes.
    let (s, v) = e
        .me(
            "POST",
            "/kb/candidates/resolve",
            Some(json!({"ids": [], "choice": "yes"})),
        )
        .await;
    assert_eq!((s, v), (400, json!({"error": "参数不对：ids"})));
    let (s, v) = e
        .me(
            "POST",
            "/kb/candidates/resolve",
            Some(json!({"ids": ["mm_x"], "choice": "maybe"})),
        )
        .await;
    assert_eq!((s, v), (400, json!({"error": "参数不对：choice"})));
    let (s, v) = call(
        &e.app,
        Some(&caller(WS, OTHER)),
        "POST",
        "/kb/candidates/resolve",
        Some(&json!({"ids": [ids[0]], "choice": "yes"})),
    )
    .await;
    assert_eq!((s, v), (404, json!({"error": "没有这个记忆"})));
    assert_eq!(e.mem(ids[0]).unwrap().status, "candidate");

    let (s, v) = e
        .me(
            "POST",
            "/kb/candidates/resolve",
            Some(json!({"ids": ids[..21], "choice": "yes"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v, json!({"ok": true, "done": 21}));
    let (s, v) = e
        .me(
            "POST",
            "/kb/candidates/resolve",
            Some(json!({"ids": [ids[21], ids[0]], "choice": "no"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        v,
        json!({"ok": true, "done": 1}),
        "already confirmed is skipped"
    );

    assert_eq!(e.open_confirms(), 0);
    for m in &made[..21] {
        assert_eq!(e.mem(&m.id).unwrap().status, "confirmed");
        if let Some(d) = &m.decision_id {
            let d = e.decision(d);
            assert_eq!(d.status, "resolved");
            assert_eq!(d.result, json!({"choice": "yes"}));
        }
    }
    assert!(e.mem(ids[21]).is_none(), "no deletes the candidate");
    let audits: i64 = e
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT COUNT(*) FROM audit WHERE action = 'decision.resolve'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(audits, 20, "every decision answered through the dispatch");
    let (_, v) = e.me("GET", "/kb/candidates", None).await;
    assert_eq!(v, json!([]));
}

#[tokio::test]
async fn candidates_expire_after_thirty_days() {
    let e = setup();
    let old = e.propose(2);
    let m = &e.env.modules;
    assert_eq!(candidates::expire(m, m.clock.now()).unwrap(), 0);
    e.env.clock.advance(EXPIRE_AFTER_MS + 1000);
    let fresh = e.propose(1);
    // `mems.created` is the real time; push the fresh one forward with the fake clock.
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE mems SET created = ?1 WHERE id = ?2",
                rusqlite::params![m.clock.now(), fresh[0].id],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(candidates::expire(m, m.clock.now()).unwrap(), 2);
    for x in &old {
        assert_eq!(e.mem(&x.id).unwrap().status, "stale");
        assert_eq!(
            e.decision(x.decision_id.as_deref().unwrap()).status,
            "expired"
        );
    }
    assert_eq!(e.mem(&fresh[0].id).unwrap().status, "candidate");
    assert_eq!(e.open_confirms(), 1);
    assert_eq!(candidates::expire(m, m.clock.now()).unwrap(), 0);
}

#[tokio::test]
async fn steve_remember_saves_a_candidate() {
    let e = setup();
    let cx = McpCx {
        ws: WS.into(),
        user: ME.into(),
        bot_id: "bot_dev".into(),
        item_id: Some("ST-12".into()),
        session_id: Some("ses_1".into()),
    };
    let m = &e.env.modules;
    let r = candidates::remember(
        m,
        &e.kb,
        &cx,
        &json!({"text": "Acme 每年三月续签", "kind": "fact", "about": "Acme"}),
    );
    assert_eq!(r.as_deref(), Ok(SAVED));
    let (_, list) = e.me("GET", "/kb/candidates", None).await;
    let c = &list[0];
    assert_eq!(c["origin"], "tool");
    assert_eq!(c["origin_label"], "ST-12 的会话");
    assert_eq!(
        c["mem"]["about"],
        json!([{"type": "party", "id": "org_acme", "name": "Acme"}])
    );
    let d = e.decision(c["decision_id"].as_str().unwrap());
    assert_eq!(d.requested_by, "bot_dev");
    assert_eq!(d.session_id.as_deref(), Some("ses_1"));

    let err = candidates::remember(
        m,
        &e.kb,
        &cx,
        &json!({"text": "x", "kind": "fact", "about": "王五"}),
    );
    assert!(err.unwrap_err().contains("王五"));
    let err = candidates::remember(m, &e.kb, &cx, &json!({"text": "x", "kind": "summary"}));
    assert!(err.unwrap_err().contains("at kind"));
    assert_eq!(candidates::remember_tool()["name"], "steve_remember");
}

#[tokio::test]
async fn chat_days_are_documents_and_extracted_once() {
    let e = setup();
    let m = &e.env.modules;
    let (start, _) = chat::day_bounds("2026-10-31").unwrap();
    e.env
        .store()
        .with_conn(|c| {
            c.execute_batch(&format!(
                "INSERT INTO messages (id, workspace_id, space, author, text, item_id, created) VALUES
                   ('msg_1', 'default', 'steve', 'me', '以后周五下午不安排部署', NULL, {a}),
                   ('msg_2', 'default', 'steve', 'steve', '好的，记住了：周五下午不部署。', NULL, {b}),
                   ('msg_3', 'default', 'other', 'me', '别的空间', NULL, {b});",
                a = start + 10 * 3600 * 1000,
                b = start + 10 * 3600 * 1000 + 60_000,
            ))?;
            Ok(())
        })
        .unwrap();
    let doc = chat::sync_day(m, &e.kb, WS, "2026-10-31").unwrap().unwrap();
    let viewer = Viewer::bare(WS, OTHER);
    let hits = e
        .env
        .store()
        .reader(|c| {
            retrieve(
                c,
                &Query::new(
                    &steve_knowledge::port::SqliteStore,
                    &viewer,
                    "部署",
                    Purpose::Search,
                ),
            )
        })
        .unwrap();
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].obj_id, doc);
    assert_eq!(hits[0].label, "会话记录 · 2026-10-31 的对话");
    assert!(hits[0].text.contains("我（10:00）：以后周五下午不安排部署"));
    assert!(!hits[0].text.contains("别的空间"));

    // Labels written before ST-41 get the new names once, in place.
    let label = |c: &rusqlite::Connection| -> String {
        c.query_row(
            "SELECT label FROM kb_chunks WHERE obj_id = ?",
            [&doc],
            |r| r.get(0),
        )
        .unwrap()
    };
    let relabeled = e
        .env
        .store()
        .transaction(|tx| {
            tx.execute(
                "UPDATE kb_chunks SET label = '对话 · 2026-10-31 的对话' WHERE obj_id = ?",
                [&doc],
            )?;
            let n = steve_knowledge::sync::relabel_once(tx, WS, 1)?;
            let again = steve_knowledge::sync::relabel_once(tx, WS, 2)?;
            Ok::<_, steve_store::StoreError>((n, again, label(tx)))
        })
        .unwrap();
    assert_eq!(
        relabeled,
        (1, 0, "会话记录 · 2026-10-31 的对话".to_string())
    );

    e.env.gateway.script(
        PROMPT_HEAD,
        r#"{"items":[{"kind":"rule","text":"周五下午不安排部署","about":[],"evidence":"以后周五下午不安排部署"}]}"#,
    );
    let got = chat::extract_day(m, &e.kb, WS, "2026-10-31").await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].source_type, "message");
    assert_eq!(got[0].source_label, "10-31 的对话");
    assert!(
        chat::extract_day(m, &e.kb, WS, "2026-10-31")
            .await
            .unwrap()
            .is_empty()
    );

    // A day without messages has no document.
    e.env
        .store()
        .with_conn(|c| {
            c.execute("DELETE FROM messages", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(chat::sync_day(m, &e.kb, WS, "2026-10-31").unwrap(), None);
}
