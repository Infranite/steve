//! K3 acceptance, with fixed gateway output: an item completion yields at most 5
//! candidates, `dup_of` ones are dropped, a “关于” nobody can be resolved to is dropped; at most 20
//! memory decisions are open; confirming with `supersedes` makes the old memory stale; a batch confirm
//! leaves decisions consistent; candidates expire after 30 days. Plus the chat source, the 待确认 list
//! and `steve_remember`.

use axum::Router;
use serde_json::{Value, json};
use steve_knowledge::Knowledge;
use steve_knowledge::candidates::{
    self, EXPIRE_AFTER_MS, MAX_PER_ORIGIN, MemDecisions, NewCandidate, Origin, OriginKind, SAVED,
};
use steve_knowledge::extract::PROMPT_HEAD;
use steve_knowledge::retrieve::{Purpose, Query, retrieve};
use steve_knowledge::sources::{chat, items};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::decisions;
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, call, caller};
use steve_store::base::mems::Mem;
use steve_store::base::{Level, ObjRef, Viewer};
use steve_store::testkit::knowledge as fixture;

#[test]
fn private_address_is_not_injected_into_shared_workspace_chat() {
    let h = setup();
    h.env
        .store()
        .transaction(|tx| -> steve_modules::ApiResult<()> {
            let who = caller(WS, ME);
            let me = steve_modules::contacts::service::ensure_base(tx, &who)?;
            steve_modules::contacts::repo::value_insert(
                tx,
                WS,
                &steve_modules::contacts::repo::NewValue {
                    party_id: me.id,
                    key: "call".into(),
                    label: "Internal name".into(),
                    value: "Private nickname".into(),
                    source: "me".into(),
                    source_label: "Manual".into(),
                    source_ref: None,
                    level: Level::Public,
                    status: "confirmed".into(),
                    decision_id: None,
                },
            )?;
            Ok(())
        })
        .unwrap();
    let context = || {
        h.env
            .store()
            .read(|c| {
                let viewer = Viewer::bare(WS, ME);
                Ok::<_, steve_store::StoreError>(
                    steve_knowledge::chat_context(
                        c,
                        &h.kb,
                        &viewer,
                        "unrelated question",
                        steve_store::now(),
                    )
                    .unwrap(),
                )
            })
            .unwrap()
    };
    assert!(!context().is_some_and(|k| k.ctx.text.contains("Private nickname")));
    fixture::only_member(h.env.store(), WS, ME).unwrap();
    assert!(context().is_some_and(|k| k.ctx.text.contains("Private nickname")));
}

struct Env {
    env: TestEnv,
    kb: Knowledge,
    app: Router,
}

fn setup() -> Env {
    let env = TestEnv::new(|r| {
        r.decision(candidates::SUBJECT, Box::new(MemDecisions));
    });
    let kb = Knowledge::new(|_| {});
    let app = steve_knowledge::routes(kb.clone()).with_state(env.modules.clone());
    fixture::seed_delivery(env.store()).expect("seed");
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
        self.env
            .store()
            .read(|c| steve_store::knowledge::Memories(c).get(id))
            .unwrap()
    }
    fn open_confirms(&self) -> i64 {
        self.env
            .store()
            .read(|r| steve_store::DecisionRows(r).open_confirmations(WS, "mem"))
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
            .write(|tx| {
                (0..n)
                    .map(|i| {
                        candidates::propose_in(
                            tx,
                            &self.origin(100 + i),
                            &NewCandidate {
                                kind: "fact".into(),
                                text: format!("第 {i} 条候选"),
                                about: vec![],
                                level: Level::Conclusion,
                                evidence: String::new(),
                                owner_said: false,
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
        .read(|c| {
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
    fixture::set_delivery_stamp(e.env.store(), 200).unwrap();
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

    // Single-user reads include all candidates; the open-decision limit remains 20.
    let (s, v) = call(
        &e.app,
        Some(&caller(WS, OTHER)),
        "GET",
        "/kb/candidates",
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(v.as_array().unwrap().len(), 23);
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
        .read(|c| {
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

    // Argument validation before anything changes.
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
    let audits = fixture::decision_audits(e.env.store()).unwrap();
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
    fixture::set_memory_created(e.env.store(), &fresh[0].id, m.clock.now()).unwrap();
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
        &cx,
        &json!({"text": "x", "kind": "fact", "about": "王五"}),
    );
    assert!(err.unwrap_err().contains("王五"));
    let err = candidates::remember(m, &cx, &json!({"text": "x", "kind": "summary"}));
    assert!(err.unwrap_err().contains("at kind"));
    assert_eq!(candidates::remember_tool()["name"], "steve_remember");
}

#[tokio::test]
async fn chat_days_are_documents_and_extracted_once() {
    let e = setup();
    let m = &e.env.modules;
    let (start, _) = chat::day_bounds("2026-10-31", steve_runtime::tz::Zone::system()).unwrap();
    fixture::seed_day_messages(e.env.store(), start).unwrap();
    let doc = chat::sync_day(m, WS, "2026-10-31").unwrap().unwrap();
    let viewer = Viewer::bare(WS, OTHER);
    let hits = e
        .env
        .store()
        .read(|c| {
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

    e.env.gateway.script(
        PROMPT_HEAD,
        r#"{"items":[{"kind":"rule","text":"周五下午不安排部署","about":[],"evidence":"以后周五下午不安排部署"}]}"#,
    );
    let got = chat::extract_day(m, &e.kb, WS, "2026-10-31").await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].source_type, "message");
    assert_eq!(got[0].source_label, "10-31 的对话");
    // you said “以后…” yourself: kept at once, its card settled and undoable, one notice says so
    assert_eq!(got[0].status, "confirmed");
    let d = e.decision(got[0].decision_id.as_deref().unwrap());
    assert_eq!(
        (d.status.as_str(), d.result["choice"].as_str()),
        ("resolved", Some("yes"))
    );
    assert!(!d.undo.is_null());
    assert_eq!(e.open_confirms(), 0);
    let notices = fixture::notice_text(e.env.store()).unwrap();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("周五下午不安排部署"), "{notices:?}");
    decisions::undo(m, &d.id, ME, |d| {
        d.status == "resolved" && !d.undo.is_null()
    })
    .expect("ours")
    .expect("undone");
    assert_eq!(e.mem(&got[0].id).unwrap().status, "candidate");
    assert!(
        chat::extract_day(m, &e.kb, WS, "2026-10-31")
            .await
            .unwrap()
            .is_empty()
    );

    // A day without messages has no document.
    fixture::clear_messages(e.env.store()).unwrap();
    assert_eq!(chat::sync_day(m, WS, "2026-10-31").unwrap(), None);
}

#[test]
fn only_the_owners_own_words_saying_to_keep_it_count() {
    let day = "我（10:00）：以后周五下午不安排部署\n\nSteve（10:01）：好的，记住了：周五下午不部署。\n\n我（10:02）：这个页面改宽一点";
    let said = |q: &str| candidates::owner_said(day, "我（", q);
    assert!(said("以后周五下午不安排部署"));
    assert!(said("以后周五…不安排部署"));
    // Steve's words, even with 记住
    assert!(!said("好的，记住了：周五下午不部署"));
    // yours, but not saying to keep it
    assert!(!said("这个页面改宽一点"));
    // one quote of Steve's spoils it
    assert!(!said("以后周五下午不安排部署 / 好的，记住了"));
    assert!(!said(""));
}

#[tokio::test]
async fn replacing_a_memory_still_asks_even_when_you_said_so() {
    let e = setup();
    let old = e.propose(1).remove(0);
    let d = old.decision_id.clone().unwrap();
    decisions::dispatch(&e.env.modules, &caller(WS, ME), &d, "yes", "")
        .expect("ours")
        .expect("ok");
    let o = e.origin(7);
    let made = e
        .env
        .store()
        .write(|tx| {
            let c = |supersedes: Option<String>| NewCandidate {
                kind: "rule".into(),
                text: "周五不部署".into(),
                about: vec![],
                level: Level::Conclusion,
                evidence: "以后周五不部署".into(),
                owner_said: true,
                supersedes,
                why: String::new(),
                scope: String::new(),
            };
            Ok::<_, steve_modules::ApiError>((
                candidates::propose_in(tx, &o, &c(Some(old.id.clone())))?,
                candidates::propose_in(tx, &o, &c(None))?,
            ))
        })
        .unwrap();
    assert_eq!(made.0.status, "candidate");
    assert_eq!(
        e.decision(made.0.decision_id.as_deref().unwrap()).status,
        "open"
    );
    assert_eq!(e.mem(&old.id).unwrap().status, "confirmed");
    assert_eq!(made.1.status, "confirmed");
}

#[tokio::test]
async fn an_expired_candidate_can_be_restored() {
    let e = setup();
    let x = e.propose(1).remove(0);
    let m = &e.env.modules;
    e.env.clock.advance(EXPIRE_AFTER_MS + 1000);
    assert_eq!(candidates::expire(m, m.clock.now()).unwrap(), 1);
    let (code, list) = e.me("GET", "/kb/candidates/expired", None).await;
    assert_eq!(code, 200);
    assert_eq!(list[0]["mem"]["id"], x.id.as_str());
    let (code, _) = e
        .me("POST", &format!("/kb/candidates/{}/restore", x.id), None)
        .await;
    assert_eq!(code, 200);
    assert_eq!(e.mem(&x.id).unwrap().status, "candidate");
    let (_, list) = e.me("GET", "/kb/candidates", None).await;
    assert_eq!(list[0]["mem"]["id"], x.id.as_str());
    let (_, list) = e.me("GET", "/kb/candidates/expired", None).await;
    assert_eq!(list, json!([]));
    // waits another 30 days from now: the next hourly run leaves it alone
    assert_eq!(candidates::expire(m, steve_store::now() + 1000).unwrap(), 0);
    // a second restore: nothing expired any more
    let (code, _) = e
        .me("POST", &format!("/kb/candidates/{}/restore", x.id), None)
        .await;
    assert_eq!(code, 409);
    // and it can be answered as usual
    let (code, r) = e
        .me(
            "POST",
            "/kb/candidates/resolve",
            Some(json!({"ids": [x.id], "choice": "yes"})),
        )
        .await;
    assert_eq!((code, r["done"].clone()), (200, json!(1)));
}

#[tokio::test]
async fn chat_failed_extraction_retries_and_catches_up_only_seven_days() {
    use steve_runtime::clock::Clock;
    let e = setup();
    let zone = steve_runtime::tz::Zone::parse("UTC").unwrap();
    let (start, _) = chat::day_bounds("2026-11-10", zone).unwrap();
    let clock = steve_runtime::clock::FakeClock::new(start + 4 * 3_600_000);
    e.env.clock.set(clock.now());
    let days = [
        ("2026-11-09", "recent"),
        ("2026-11-03", "oldest"),
        ("2026-11-02", "too_old"),
    ];
    let messages = days.map(|(day, id)| (id, chat::day_bounds(day, zone).unwrap().0 + 3_600_000));
    fixture::seed_chat_catchup(e.env.store(), &messages).unwrap();
    e.env.gateway.fail("unavailable");
    assert!(
        chat::extract_day(&e.env.modules, &e.kb, WS, "2026-11-09")
            .await
            .is_err()
    );
    let marked = |day: &str| {
        e.env
            .store()
            .read(|c| steve_store::knowledge::Documents(c).mark(WS, &format!("extract:chat:{day}")))
            .unwrap()
            .is_some()
    };
    assert!(!marked("2026-11-09"));
    e.env.gateway.script(
        PROMPT_HEAD,
        r#"{"items":[{"kind":"rule","text":"周五不部署","evidence":"以后周五不部署"}]}"#,
    );
    chat::tick(&e.env.modules, &e.kb, clock.now(), false)
        .await
        .unwrap();
    assert!(marked("2026-11-03"));
    assert!(marked("2026-11-09"));
    assert!(!marked("2026-11-02"));
    let calls = e.env.gateway.prompts.lock().unwrap().len();
    chat::tick(&e.env.modules, &e.kb, clock.now(), false)
        .await
        .unwrap();
    assert_eq!(e.env.gateway.prompts.lock().unwrap().len(), calls);
}
