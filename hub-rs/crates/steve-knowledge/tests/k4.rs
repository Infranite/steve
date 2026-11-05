//! K4 acceptance: the opening prompt is unchanged with an empty knowledge base;
//! with hits it has the block format and stays ≤6000 characters; a main-thread answer citing a number
//! that does not exist loses that mark and `cites` lists only what was cited; a shared workspace's
//! main thread never shows private knowledge; MCP cannot read another user's private document.
//! Plus `/kb/uses` and the read tool's paging.

use std::sync::Arc;

use axum::Router;
use serde_json::{Value, json};
use steve_knowledge::context::{
    self, CHAT_HEAD, CHAT_RULES, KnowledgeContext, SESSION_HEAD, SESSION_MAX, SESSION_RULES,
};
use steve_knowledge::tools::{self, LEVELS, READ, SEARCH};
use steve_knowledge::uses;
use steve_knowledge::{Knowledge, NoGuard};
use steve_modules::Registry;
use steve_modules::contacts::mcp::McpCx;
use steve_modules::context::{Cite, PromptContext};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, caller};
use steve_store::Item;
use steve_store::base::{Level, Viewer};

struct Env {
    env: TestEnv,
    kb: Knowledge,
    app: Router,
}

fn setup() -> Env {
    let env = TestEnv::new(|_| {});
    let kb = Knowledge::new(Arc::new(NoGuard), |_| {});
    let app = steve_knowledge::routes(kb.clone()).with_state(env.modules.clone());
    Env { env, kb, app }
}

impl Env {
    async fn req(
        &self,
        ws: &str,
        who: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        call(
            &self.app,
            Some(&caller(ws, who)),
            method,
            path,
            body.as_ref(),
        )
        .await
    }

    async fn note(&self, ws: &str, who: &str, title: &str, body: &str, vis: &str) -> String {
        let (s, v) = self
            .req(
                ws,
                who,
                "POST",
                "/kb/notes",
                Some(json!({"title": title, "body": body, "visibility": vis})),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    /// An item (with goal, and `item.create` by `by` when given) and its first session.
    fn item(
        &self,
        ws: &str,
        id: &str,
        title: &str,
        goal: &str,
        by: Option<&str>,
    ) -> (Item, String) {
        let sid = format!("ses_{id}");
        self.env
            .store()
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated) VALUES (?, ?, (SELECT COUNT(*) + 1 FROM items), 'task', ?, 'run', '执行', 1, 1)",
                    rusqlite::params![id, ws, title],
                )?;
                c.execute(
                    "INSERT INTO item_goals (item_id, version, text, created) VALUES (?, 1, ?, 1)",
                    rusqlite::params![id, goal],
                )?;
                c.execute(
                    "INSERT INTO sessions (id, workspace_id, item_id, no, harness, cwd, status, created, updated) VALUES (?, ?, ?, 1, 'claude', '/tmp', 'idle', 1, 1)",
                    rusqlite::params![sid, ws, id],
                )?;
                if let Some(u) = by {
                    c.execute(
                        "INSERT INTO audit (workspace_id, ts, actor, action, target) VALUES (?, 1, ?, 'item.create', ?)",
                        rusqlite::params![ws, u, id],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let item = self.env.store().items().get(id).unwrap().unwrap();
        (item, sid)
    }

    fn provider(&self) -> KnowledgeContext {
        KnowledgeContext::new(self.env.store().clone(), self.kb.clone())
    }

    fn session_text(&self, item: &Item) -> String {
        let mut r = Registry::new();
        r.prompt_context(Box::new(self.provider()));
        self.env
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(r.contexts().session(c, &item.workspace_id, item))
            })
            .unwrap()
    }

    fn viewer(&self, ws: &str, who: &str) -> Viewer {
        self.env
            .store()
            .reader(|c| Viewer::load(c, ws, who))
            .unwrap()
    }

    fn uses(&self, ws: &str, ctype: &str, id: &str) -> Vec<uses::UseRow> {
        self.env
            .store()
            .reader(|c| uses::uses_of(c, ws, ctype, id))
            .unwrap()
    }

    fn cx(&self, ws: &str, user: &str, item: &str, sid: &str) -> McpCx {
        McpCx {
            ws: ws.into(),
            user: user.into(),
            bot_id: "steve".into(),
            item_id: Some(item.into()),
            session_id: Some(sid.into()),
        }
    }

    fn tool(&self, cx: &McpCx, name: &str, args: Value) -> Result<String, String> {
        tools::call_sync(&self.env.modules, &self.kb, cx, name, &args)
    }
}

fn knowledge_lines(text: &str) -> Vec<&str> {
    text.lines().filter(|l| l.starts_with("[K")).collect()
}

#[tokio::test]
async fn empty_knowledge_base_leaves_the_opening_prompt_unchanged() {
    let e = setup();
    let (item, sid) = e.item(WS2, "W2-1", "部署脚本改造", "把部署脚本改成幂等的", None);
    // no provider registered and the knowledge provider on an empty base say exactly the same: ""
    let without = Registry::new();
    let before = e
        .env
        .store()
        .reader(|c| Ok::<_, steve_store::StoreError>(without.contexts().session(c, WS2, &item)))
        .unwrap();
    assert_eq!(before, "");
    assert_eq!(e.session_text(&item), before);
    let p = e.provider();
    let none = e
        .env
        .store()
        .reader(|c| Ok::<_, steve_store::StoreError>(p.session(c, WS2, &item)))
        .unwrap();
    assert_eq!(none, None);
    // and nothing recorded
    assert!(e.uses(WS2, "session", &sid).is_empty());
    // the main thread too
    let v = e.viewer(WS2, ME);
    let chat = e
        .env
        .store()
        .reader(|c| Ok::<_, steve_store::StoreError>(p.chat(c, WS2, &v, "部署脚本怎么写")))
        .unwrap();
    assert_eq!(chat, None);
    // knowledge that does not match the item changes nothing either
    e.note(WS2, ME, "午饭", "楼下的面馆周一休息。", "private")
        .await;
    assert_eq!(e.session_text(&item), "");
}

#[tokio::test]
async fn session_context_has_the_snippet_format_and_stays_under_6000() {
    let e = setup();
    let nid = e
        .note(
            WS2,
            ME,
            "部署脚本说明",
            "部署脚本在 ops/deploy.sh，部署前要先跑数据库迁移。",
            "private",
        )
        .await;
    let (item, sid) = e.item(WS2, "W2-2", "部署脚本改造", "让部署脚本可以重复执行", None);
    let text = e.session_text(&item);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], SESSION_HEAD, "{text}");
    assert_eq!(*lines.last().unwrap(), SESSION_RULES);
    assert_eq!(
        lines[1],
        "[K1] 部署脚本在 ops/deploy.sh，部署前要先跑数据库迁移。 —— 来源：笔记 · 部署脚本说明；级别：可转述结论"
    );
    // recorded for the session that is starting, ids only
    let u = e.uses(WS2, "session", &sid);
    assert_eq!(u.len(), 1);
    assert_eq!(u[0].hits[0].obj_id, nid);
    assert_eq!(u[0].hits[0].n, 1);
    assert!(u[0].query.contains("部署脚本改造"));

    // many long hits: entries dropped from the end, never over 6000 characters, each ≤600
    for i in 0..20 {
        let body = format!(
            "部署脚本第{i}节。{}",
            "部署脚本要幂等，失败可以重跑。".repeat(80)
        );
        e.note(WS2, ME, &format!("部署脚本附录 {i}"), &body, "private")
            .await;
    }
    let text = e.session_text(&item);
    assert!(
        text.chars().count() <= SESSION_MAX,
        "{}",
        text.chars().count()
    );
    assert!(text.starts_with(SESSION_HEAD) && text.ends_with(SESSION_RULES));
    let ks = knowledge_lines(&text);
    assert!(ks.len() >= 3, "{text}");
    for (i, l) in ks.iter().enumerate() {
        assert!(l.starts_with(&format!("[K{}] ", i + 1)), "{l}");
        let body = l.split(" —— 来源：").next().unwrap();
        assert!(body.chars().count() <= 600 + 6, "{l}");
        assert!(l.contains("；级别："));
    }
}

#[tokio::test]
async fn private_knowledge_follows_the_items_user_and_never_reaches_a_shared_main_thread() {
    let e = setup();
    let private = e
        .note(
            WS,
            ME,
            "Acme 合同",
            "Acme 的合同每年三月续签，底价八折。",
            "private",
        )
        .await;
    let shared = e
        .note(
            WS,
            ME,
            "Acme 联系方式",
            "Acme 的合同由采购部 Sarah 负责续签。",
            "workspace",
        )
        .await;

    // the session of an item ME raised sees ME's private note
    let (mine, _) = e.item(
        WS,
        "ST-1",
        "Acme 合同续签",
        "准备 Acme 合同续签材料",
        Some(ME),
    );
    let t = e.session_text(&mine);
    assert!(t.contains("底价八折"), "{t}");
    // OTHER's item, and an item nobody in particular raised in a shared workspace: no private note
    let (theirs, _) = e.item(
        WS,
        "ST-2",
        "Acme 合同续签",
        "准备 Acme 合同续签材料",
        Some(OTHER),
    );
    let (steves, _) = e.item(WS, "ST-3", "Acme 合同续签", "准备 Acme 合同续签材料", None);
    for it in [&theirs, &steves] {
        let t = e.session_text(it);
        assert!(t.contains("Sarah"), "{t}");
        assert!(!t.contains("底价八折"), "{t}");
    }

    // main thread of a two-member workspace: only workspace knowledge, even for the note's owner
    let p = e.provider();
    let v = e.viewer(WS, ME);
    let cx = e
        .env
        .store()
        .reader(|c| Ok::<_, steve_store::StoreError>(p.chat(c, WS, &v, "Acme 的合同什么时候续签")))
        .unwrap()
        .expect("context");
    assert!(
        cx.text.contains("Sarah") && !cx.text.contains("底价八折"),
        "{}",
        cx.text
    );
    assert!(cx.cites.iter().all(|c| !c.href.contains(&private)));
    assert!(cx.cites.iter().any(|c| c.href.contains(&shared)));

    // a one-member workspace's main thread may use the owner's private knowledge
    e.note(
        WS2,
        ME,
        "Acme 合同",
        "Acme 的合同每年三月续签，底价八折。",
        "private",
    )
    .await;
    let v2 = e.viewer(WS2, ME);
    let cx2 = e
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(p.chat(c, WS2, &v2, "Acme 的合同什么时候续签"))
        })
        .unwrap()
        .expect("context");
    assert!(cx2.text.contains("底价八折"));
}

#[tokio::test]
async fn chat_answer_loses_unknown_citations_and_cites_only_what_it_used() {
    let e = setup();
    e.note(WS2, ME, "部署说明", "部署脚本在 ops/deploy.sh。", "private")
        .await;
    e.note(WS2, ME, "部署窗口", "部署窗口是每周二下午。", "private")
        .await;
    let p = e.provider();
    let v = e.viewer(WS2, ME);
    let cx = e
        .env
        .store()
        .reader(|c| Ok::<_, steve_store::StoreError>(p.chat(c, WS2, &v, "部署")))
        .unwrap()
        .expect("context");
    let lines: Vec<&str> = cx.text.lines().collect();
    assert_eq!(lines[0], CHAT_HEAD);
    assert_eq!(*lines.last().unwrap(), CHAT_RULES);
    assert!(
        lines[1].starts_with("[1] ") && lines[2].starts_with("[2] "),
        "{}",
        cx.text
    );
    assert_eq!(cx.cites.len(), 2);

    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO messages (id, workspace_id, space, author, text, created) VALUES ('msg_1', 'w2', 'main', 'steve', '', 1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let done = e
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(p.chat_done(
                c,
                WS2,
                "msg_1",
                "脚本在 ops/deploy.sh [2]，负责人是运维 [3]。",
                &cx,
            ))
        })
        .unwrap();
    assert_eq!(done.text, "脚本在 ops/deploy.sh [2]，负责人是运维。");
    let meta = done.meta.unwrap();
    let cites: Vec<Cite> = serde_json::from_value(meta["cites"].clone()).unwrap();
    assert_eq!(cites, vec![cx.cites[1].clone()]);
    assert_eq!(meta["cites"][0]["n"], 2);
    assert!(
        meta["cites"][0]["href"]
            .as_str()
            .unwrap()
            .starts_with("#/knowledge/doc/kd_")
    );
    assert_eq!(meta["cites"][0]["level"], "conclusion");
    // kb_uses(chat, message) has what the answer was given
    let u = e.uses(WS2, "chat", "msg_1");
    assert_eq!(u.len(), 1);
    assert_eq!(u[0].hits.len(), 2);
    assert_eq!(u[0].query, "部署");

    // and through /kb/uses
    let (s, v) = e
        .req(
            WS2,
            ME,
            "GET",
            "/kb/uses?consumer_type=chat&consumer_id=msg_1",
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["hits"].as_array().unwrap().len(), 2);
    assert_eq!(v["hits"][0]["deleted"], false);
}

#[tokio::test]
async fn mcp_never_reads_another_users_private_document() {
    let e = setup();
    let private = e
        .note(
            WS,
            ME,
            "Acme 合同",
            "Acme 的合同每年三月续签，底价八折。",
            "private",
        )
        .await;
    let shared = e
        .note(
            WS,
            ME,
            "Acme 联系人",
            "Acme 的合同由采购部 Sarah 负责。",
            "workspace",
        )
        .await;
    let (_, sid) = e.item(WS, "ST-9", "Acme 合同", "整理 Acme 合同", Some(OTHER));
    let cx = e.cx(WS, OTHER, "ST-9", &sid);

    let r = e.tool(&cx, READ, json!({"id": private}));
    assert_eq!(r, Err(format!("No document {private}.")));
    let found = e.tool(&cx, SEARCH, json!({"query": "Acme 合同"})).unwrap();
    assert!(
        !found.contains("底价八折") && !found.contains(&private),
        "{found}"
    );
    assert!(found.contains(&shared), "{found}");
    assert!(found.ends_with(LEVELS));
    assert!(
        found.starts_with("[K1] Acme 联系人 · 笔记 · Acme 联系人 · level: conclusion"),
        "{found}"
    );

    let r = e.tool(&cx, READ, json!({"id": shared})).unwrap();
    assert!(
        r.contains("Sarah") && r.ends_with("End of document."),
        "{r}"
    );

    // ME's own session (an item ME raised) can read it
    let (_, sid2) = e.item(WS, "ST-10", "Acme 合同", "整理 Acme 合同", Some(ME));
    let mine = e.cx(WS, ME, "ST-10", &sid2);
    assert!(
        e.tool(&mine, READ, json!({"id": private}))
            .unwrap()
            .contains("底价八折")
    );
    // another workspace's session cannot
    let (_, sid3) = e.item(WS2, "W2-9", "Acme", "Acme", Some(ME));
    let other_ws = e.cx(WS2, ME, "W2-9", &sid3);
    assert!(e.tool(&other_ws, READ, json!({"id": private})).is_err());

    // argument errors in steve-mcp's wording, unknown tools
    let bad = e.tool(&cx, SEARCH, json!({"limit": "3"})).unwrap_err();
    assert_eq!(
        bad,
        "MCP error -32602: Input validation error: Invalid arguments for tool steve_knowledge_search: Invalid input: expected string, received undefined at query\nInvalid input: expected number, received string at limit"
    );
    assert!(tools::handles(SEARCH) && tools::handles(READ) && !tools::handles("steve_remember"));
    assert_eq!(tools::tools().len(), 2);
}

#[tokio::test]
async fn search_numbers_continue_after_the_opening_and_read_pages() {
    let e = setup();
    e.note(
        WS2,
        ME,
        "部署脚本说明",
        "部署脚本在 ops/deploy.sh。",
        "private",
    )
    .await;
    let (item, sid) = e.item(WS2, "W2-3", "部署脚本", "改部署脚本", None);
    let opening = e.session_text(&item);
    let given = knowledge_lines(&opening).len();
    assert!(given >= 1);
    let cx = e.cx(WS2, ME, "W2-3", &sid);
    let r = e.tool(&cx, SEARCH, json!({"query": "部署脚本"})).unwrap();
    assert!(r.starts_with(&format!("[K{}] ", given + 1)), "{r}");
    let r2 = e
        .tool(&cx, SEARCH, json!({"query": "部署脚本", "limit": 1}))
        .unwrap();
    assert!(r2.starts_with(&format!("[K{}] ", given + 2)), "{r2}");
    assert_eq!(e.uses(WS2, "session", &sid).len(), 3);
    assert_eq!(
        e.tool(&cx, SEARCH, json!({"query": "完全无关的词汇"}))
            .unwrap(),
        "No knowledge matches \"完全无关的词汇\"."
    );

    // a long note reads in pages of ≤20000 characters, without the chunks' overlaps
    let para =
        |i: usize| format!("第{i:04}段：发布流程要先打标签再构建镜像，镜像推送后再更新集群。\n\n");
    let body: String = (0..800).map(para).collect();
    let long = e.note(WS2, ME, "发布手册", &body, "private").await;
    let p1 = e.tool(&cx, READ, json!({"id": long})).unwrap();
    assert!(p1.starts_with("发布手册 · level: conclusion\n\n"));
    let next: i64 = p1
        .rsplit("Continue with from=")
        .next()
        .and_then(|s| s.trim_end_matches('.').parse().ok())
        .expect("continue");
    assert!(p1.chars().count() <= 20_000 + 200);
    let p2 = e
        .tool(&cx, READ, json!({"id": long, "from": next}))
        .unwrap();
    let mut all = String::new();
    let mut page = p2.clone();
    let mut text = |p: &str| {
        let body = p.split_once("\n\n").unwrap().1;
        let body = body.rsplit_once("\n\n").unwrap().0;
        all.push_str(body);
    };
    text(&p1);
    let mut from = next;
    loop {
        text(&page);
        if page.ends_with("End of document.") {
            break;
        }
        from = page
            .rsplit("Continue with from=")
            .next()
            .and_then(|s| s.trim_end_matches('.').parse().ok())
            .unwrap();
        page = e
            .tool(&cx, READ, json!({"id": long, "from": from}))
            .unwrap();
    }
    let _ = from;
    // every paragraph exactly once, in order
    for i in [0usize, 1, 399, 400, 799] {
        let mark = format!("第{i:04}段");
        assert_eq!(all.matches(&mark).count(), 1, "{mark}");
    }
    assert!(all.find("第0398段").unwrap() < all.find("第0399段").unwrap());
}

#[tokio::test]
async fn uses_endpoint_checks_params_and_ownership_and_marks_deleted_hits() {
    let e = setup();
    let nid = e
        .note(
            WS2,
            ME,
            "部署脚本说明",
            "## 位置\n\n部署脚本在 ops/deploy.sh。",
            "private",
        )
        .await;
    let (item, sid) = e.item(WS2, "W2-4", "部署脚本", "改部署脚本", None);
    assert!(!e.session_text(&item).is_empty());

    let (s, v) = e
        .req(WS2, ME, "GET", "/kb/uses?consumer_type=session", None)
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("参数不对：consumer_id"))
    );
    let (s, _) = e
        .req(
            WS2,
            ME,
            "GET",
            "/kb/uses?consumer_type=x&consumer_id=y",
            None,
        )
        .await;
    assert_eq!(s, 400);
    // the session is in w2: from default it does not exist
    let (s, v) = e
        .req(
            WS,
            ME,
            "GET",
            &format!("/kb/uses?consumer_type=session&consumer_id={sid}"),
            None,
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (404, Some("没有这个会话")));
    // someone else's searches
    let (s, _) = e
        .req(
            WS,
            ME,
            "GET",
            &format!("/kb/uses?consumer_type=search&consumer_id={OTHER}"),
            None,
        )
        .await;
    assert_eq!(s, 404);

    let path = format!("/kb/uses?consumer_type=session&consumer_id={sid}");
    let (s, v) = e.req(WS2, ME, "GET", &path, None).await;
    assert_eq!(s, 200, "{v}");
    let h = &v["hits"][0];
    assert_eq!(h["id"], nid.as_str());
    assert_eq!(h["title"], "部署脚本说明");
    assert_eq!(h["label"], "笔记 · 部署脚本说明");
    assert_eq!(h["href"], format!("#/knowledge/doc/{nid}?seq=0"));
    assert_eq!(h["deleted"], false);
    assert_eq!(h["level"], "conclusion");
    assert_eq!(h["heading"], "位置");

    let (s, _) = e
        .req(WS2, ME, "DELETE", &format!("/kb/docs/{nid}"), None)
        .await;
    assert_eq!(s, 200);
    let (_, v) = e.req(WS2, ME, "GET", &path, None).await;
    let h = &v["hits"][0];
    assert_eq!(
        (
            h["deleted"].as_bool(),
            h["title"].as_str(),
            h["label"].as_str(),
            h["href"].as_str()
        ),
        (Some(true), Some("已删除"), Some("已删除"), Some(""))
    );
    assert_eq!(h["heading"], "");
}

#[test]
fn chat_finish_without_citations_still_reports_an_empty_list() {
    let cx = steve_modules::context::ChatCtx {
        text: "[1] x".into(),
        cites: vec![Cite {
            n: 1,
            label: "笔记 · x".into(),
            href: "#/knowledge/doc/kd_x?seq=0".into(),
            level: Level::Public,
        }],
    };
    let d = context::chat_finish("没用到 [5]", &cx);
    assert_eq!(d.text, "没用到");
    assert_eq!(d.meta.unwrap()["cites"], json!([]));
}
