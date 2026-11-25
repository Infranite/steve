//! K1 acceptance: notes searchable on save with single-user read visibility,
//! `never` memories never returned, source scope changes reach the index, deleted memories leave
//! it, old Bot memories become searchable after a sync; plus the REST shapes and error codes.

use steve_store::testkit::knowledge as fixture;

use axum::Router;
use serde_json::{Value, json};
use steve_knowledge::Knowledge;
use steve_knowledge::index;
use steve_knowledge::retrieve::{Purpose, Query, anchor_mems, retrieve};
use steve_knowledge::sync;
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, caller};
use steve_store::StoreError;
use steve_store::base::mems::{self, NewMem};
use steve_store::base::{Level, ObjRef, Viewer, Visibility};

struct Env {
    env: TestEnv,
    kb: Knowledge,
    app: Router,
}

fn setup() -> Env {
    let env = TestEnv::new(|_| {});
    let kb = Knowledge::bare();
    let app = steve_knowledge::routes(kb.clone()).with_state(env.modules.clone());
    Env { env, kb, app }
}

/// Percent-encodes a query value.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Env {
    fn knowledge_events(&self) -> Vec<(String, String)> {
        self.env
            .events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e.as_domain() {
                Some(steve_types::DomainEvent::Knowledge(steve_types::KnowledgeEvent::Kb {
                    id,
                    ws,
                })) => Some((id.clone(), ws.to_string())),
                _ => None,
            })
            .collect()
    }

    async fn as_(&self, who: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        call(
            &self.app,
            Some(&caller(WS, who)),
            method,
            path,
            body.as_ref(),
        )
        .await
    }
    async fn me(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.as_(ME, method, path, body).await
    }
    async fn search(&self, who: &str, q: &str) -> Vec<Value> {
        let (s, v) = self
            .as_(who, "GET", &format!("/kb/search?q={}", enc(q)), None)
            .await;
        assert_eq!(s, 200, "{v}");
        v["hits"].as_array().cloned().unwrap_or_default()
    }
    async fn note(&self, title: &str, body: &str, extra: Value) -> Value {
        let mut b = json!({"title": title, "body": body});
        if let Value::Object(o) = extra {
            for (k, v) in o {
                b[k] = v;
            }
        }
        let (s, v) = self.me("POST", "/kb/notes", Some(b)).await;
        assert_eq!(s, 200, "{v}");
        v
    }
}

fn ids(hits: &[Value]) -> Vec<String> {
    hits.iter()
        .map(|h| h["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn note_is_searchable_right_after_saving() {
    let e = setup();
    let doc = e
        .note(
            "Acme 合同说明",
            "# 概要\n\nAcme 的合同每年三月续签，续签前一个月要提醒销售。",
            json!({}),
        )
        .await;
    let id = doc["id"].as_str().unwrap();
    assert!(id.starts_with("kd_"));
    assert_eq!(doc["source"]["kind"], "notes");
    assert_eq!(doc["visibility"], "private");
    assert_eq!(doc["level"], "conclusion");
    assert_eq!(doc["inherited"], json!({"visibility": true, "level": true}));
    assert_eq!(doc["chunks"], 1);
    assert!(doc["body"].as_str().unwrap().contains("三月续签"));
    assert_eq!(doc["preview"][0]["heading"], "概要");

    let hits = e.search(ME, "续签").await;
    assert_eq!(ids(&hits), vec![id.to_string()]);
    let h = &hits[0];
    assert_eq!(h["n"], 1);
    assert_eq!(h["type"], "chunk");
    assert_eq!(h["title"], "Acme 合同说明");
    assert_eq!(h["label"], "笔记 · Acme 合同说明");
    assert_eq!(h["href"], format!("#/knowledge/doc/{id}?seq=0"));
    assert_eq!(h["source_kind"], "notes");
    assert!(h["score"].as_f64().unwrap() > 0.0);
    // The title alone finds it too (title column).
    assert_eq!(ids(&e.search(ME, "合同说明").await), vec![id.to_string()]);
    // English prefix.
    assert_eq!(ids(&e.search(ME, "acm").await), vec![id.to_string()]);

    // The search was recorded without text.
    let uses: (i64, String) = fixture::stored_search_hits(e.env.store(), ME).unwrap();
    assert_eq!(uses.0, 3);
    assert!(uses.1.contains(id) && !uses.1.contains("三月"));

    // Update: the old text is gone from the index, the new one is there, a revision is written.
    let (s, v) = e
        .me(
            "PUT",
            &format!("/kb/notes/{id}"),
            Some(json!({"body": "Acme 改为每年九月续约。"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(e.search(ME, "三月").await.is_empty());
    assert_eq!(ids(&e.search(ME, "九月").await), vec![id.to_string()]);
    let revs = e
        .env
        .store()
        .read(|c| steve_store::Revisions(c).count(WS, "kb_note", id))
        .unwrap();
    assert_eq!(revs, 1);
    // Only the notes get `kb` events.
    let evs = e.knowledge_events();
    assert!(evs.iter().all(|(doc, ws)| doc == id && ws == WS) && evs.len() == 2);

    // Delete: gone from the index and the API.
    let (s, _) = e.me("DELETE", &format!("/kb/docs/{id}"), None).await;
    assert_eq!(s, 200);
    assert!(e.search(ME, "九月").await.is_empty());
    let (s, v) = e.me("GET", &format!("/kb/docs/{id}"), None).await;
    assert_eq!((s, v["error"].as_str()), (404, Some("没有这个文档")));
    let left: i64 = fixture::search_index_rows(e.env.store()).unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn single_user_reads_private_notes_with_workspace_and_chat_audience_boundaries() {
    let e = setup();
    let private = e
        .note("部署手册", "部署脚本放在 ops 仓库。", json!({}))
        .await;
    let shared = e
        .note(
            "部署须知",
            "部署前先在群里说一声。",
            json!({"visibility": "workspace"}),
        )
        .await;
    let (p, s) = (
        private["id"].as_str().unwrap(),
        shared["id"].as_str().unwrap(),
    );

    assert_eq!(e.search(ME, "部署").await.len(), 2);
    let other_hits = ids(&e.search(OTHER, "部署").await);
    assert_eq!(other_hits.len(), 2);
    assert!(other_hits.contains(&p.to_string()) && other_hits.contains(&s.to_string()));
    let (st, _) = e.as_(OTHER, "GET", &format!("/kb/docs/{p}"), None).await;
    assert_eq!(st, 200);
    let (st, list) = e.as_(OTHER, "GET", "/kb/docs", None).await;
    assert_eq!(st, 200);
    assert_eq!(ids(list.as_array().unwrap()).len(), 2);
    // Another workspace sees nothing at all.
    let (_, v) = call(
        &e.app,
        Some(&caller(WS2, ME)),
        "GET",
        &format!("/kb/search?q={}", enc("部署")),
        None,
    )
    .await;
    assert_eq!(v["hits"], json!([]));

    // Main-thread answers in a workspace with two members use only workspace-visible knowledge.
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
                    "部署",
                    Purpose::Chat,
                ),
            )
        })
        .unwrap();
    assert_eq!(
        hits.iter().map(|h| h.obj_id.as_str()).collect::<Vec<_>>(),
        vec![s]
    );
    // Alone in a workspace, everything of yours.
    fixture::remove_user_memberships(e.env.store(), OTHER).unwrap();
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
                    Purpose::Chat,
                ),
            )
        })
        .unwrap();
    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn never_memories_are_not_returned() {
    let e = setup();
    let (s, keep) = e
        .me(
            "POST",
            "/kb/mems",
            Some(json!({"text": "周五下午不安排部署", "kind": "rule", "level": "public"})),
        )
        .await;
    assert_eq!(s, 200, "{keep}");
    assert_eq!(keep["status"], "confirmed");
    let (s, never) = e
        .me(
            "POST",
            "/kb/mems",
            Some(json!({"text": "周五下午要去医院复查", "level": "never"})),
        )
        .await;
    assert_eq!(s, 200);
    let hits = e.search(ME, "周五下午").await;
    assert_eq!(ids(&hits), vec![keep["id"].as_str().unwrap().to_string()]);
    assert_eq!(hits[0]["type"], "mem");
    assert_eq!(hits[0]["level"], "public");
    assert_eq!(
        hits[0]["href"],
        format!("#/knowledge?mem={}", keep["id"].as_str().unwrap())
    );
    // The management list still shows it to its owner.
    let (_, list) = e.me("GET", "/kb/mems", None).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    let (_, list) = e.me("GET", "/kb/mems?kind=rule", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    // Turning a memory into `never` later takes it out too (bus event → sync).
    let id = keep["id"].as_str().unwrap().to_string();
    e.env
        .store()
        .write(|tx| {
            steve_store::knowledge::Memories(tx).patch(
                &id,
                &mems::MemPatch {
                    level: Some(Level::Never),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    sync::on_event(
        &e.env.modules,
        &steve_types::BusEvent::from(steve_types::DomainEvent::from(
            steve_types::ConfigEvent::Mm {
                id: {
                    let value: String = id.clone();
                    steve_types::MemoryId::from(value)
                },
                ws: {
                    let value: String = WS.into();
                    steve_types::WorkspaceId::from(value)
                },
            },
        )),
    )
    .unwrap();
    assert!(e.search(ME, "周五下午").await.is_empty());
    let _ = never;
}

#[tokio::test]
async fn deleted_memories_leave_the_index() {
    let e = setup();
    let (_, m) = e
        .me(
            "POST",
            "/kb/mems",
            Some(json!({"text": "Sarah 喜欢简短的邮件"})),
        )
        .await;
    let id = m["id"].as_str().unwrap().to_string();
    assert_eq!(e.search(ME, "简短").await.len(), 1);
    e.env
        .store()
        .write(|tx| steve_store::knowledge::Memories(tx).delete(&id))
        .unwrap();
    // The event path …
    sync::on_event(
        &e.env.modules,
        &steve_types::BusEvent::from(steve_types::DomainEvent::from(
            steve_types::ConfigEvent::Mm {
                id: {
                    let value: String = id.clone();
                    steve_types::MemoryId::from(value)
                },
                ws: {
                    let value: String = WS.into();
                    steve_types::WorkspaceId::from(value)
                },
            },
        )),
    )
    .unwrap();
    assert!(e.search(ME, "简短").await.is_empty());

    // … and the sweep path, for a memory deleted without anyone telling us.
    let (_, m2) = e
        .me("POST", "/kb/mems", Some(json!({"text": "Sarah 不喝咖啡"})))
        .await;
    let id2 = m2["id"].as_str().unwrap().to_string();
    assert_eq!(e.search(ME, "咖啡").await.len(), 1);
    e.env
        .store()
        .write(|tx| steve_store::knowledge::Memories(tx).delete(&id2))
        .unwrap();
    sync::sweep(&e.env.modules).unwrap();
    assert!(e.search(ME, "咖啡").await.is_empty());
}

#[tokio::test]
async fn memories_name_what_they_are_about_and_anchors_rank_higher() {
    let e = setup();
    fixture::seed_sarah(e.env.store()).unwrap();
    let (s, a) = e
        .me(
            "POST",
            "/kb/mems",
            Some(json!({"text": "喜欢先看结论的邮件", "kind": "preference",
                        "about": [{"type": "party", "id": "ct_sarah"}]})),
        )
        .await;
    assert_eq!(s, 200, "{a}");
    let (_, b) = e
        .me(
            "POST",
            "/kb/mems",
            Some(json!({"text": "周报邮件要先写结论"})),
        )
        .await;
    let (s, v) = e
        .me(
            "POST",
            "/kb/mems",
            Some(json!({"text": "x", "about": [{"type": "party", "id": "ct_nobody"}]})),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (404, Some("没有这个联系人")));

    let hits = e.search(ME, "Sarah").await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["text"], "Sarah Chen：喜欢先看结论的邮件");
    assert_eq!(hits[0]["label"], "长期记忆 · Sarah Chen");
    let viewer = Viewer::bare(WS, ME);
    let anchors = [ObjRef::new("party", "ct_sarah")];
    let hits = e
        .env
        .store()
        .read(|c| {
            let mut q = Query::new(
                &steve_knowledge::port::SqliteStore,
                &viewer,
                "结论 邮件",
                Purpose::Reply,
            );
            q.anchors = &anchors;
            retrieve(c, &q)
        })
        .unwrap();
    assert_eq!(hits[0].obj_id, a["id"].as_str().unwrap());
    assert_eq!(hits[0].about, anchors.to_vec());
    assert!(hits.iter().any(|h| h.obj_id == b["id"].as_str().unwrap()));

    let am = e
        .env
        .store()
        .read(|c| anchor_mems(c, &viewer, &anchors, 5, steve_store::now()))
        .unwrap();
    assert_eq!(am.len(), 1);
    assert_eq!(am[0].obj_id, a["id"].as_str().unwrap());
    // Single-user anchor retrieval includes private memories from other creators.
    let other = Viewer::bare(WS, OTHER);
    let am = e
        .env
        .store()
        .read(|c| anchor_mems(c, &other, &anchors, 5, steve_store::now()))
        .unwrap();
    assert_eq!(am.len(), 1);
    assert_eq!(am[0].obj_id, a["id"].as_str().unwrap());
}

#[tokio::test]
async fn source_scope_changes_reach_the_index() {
    let e = setup();
    let doc = e
        .note("值班表", "十一月的值班由小王负责。", json!({}))
        .await;
    let id = doc["id"].as_str().unwrap().to_string();
    let own = e
        .note(
            "值班备忘",
            "值班电话在共享文档里。",
            json!({"level": "public"}),
        )
        .await;
    let src_id = doc["source"]["id"].as_str().unwrap().to_string();
    e.env
        .store()
        .write(|w| {
            let mut source = steve_store::knowledge::Documents(w)
                .source(&src_id)?
                .unwrap();
            source.level = Level::Reasoning;
            source.visibility = Visibility::Workspace;
            steve_store::knowledge::Documents(w).update_source(&source, steve_store::now())?;
            index::rescope_source(w, &source)
        })
        .unwrap();
    let hits = e.search(OTHER, "值班").await;
    let lv = |id: &str| {
        hits.iter()
            .find(|h| h["id"] == id)
            .map(|h| (h["level"].clone(), h["visibility"].clone()))
    };
    assert_eq!(lv(&id), Some((json!("reasoning"), json!("workspace"))));
    // A document with its own level keeps it.
    assert_eq!(
        lv(own["id"].as_str().unwrap()),
        Some((json!("public"), json!("workspace")))
    );

    // The document's own setting via PATCH, then back to following the source with null.
    let (s, v) = e
        .me(
            "PATCH",
            &format!("/kb/docs/{id}"),
            Some(json!({"level": "never"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["level"], "never");
    assert_eq!(v["inherited"]["level"], false);
    assert!(!ids(&e.search(ME, "小王").await).contains(&id));
    let (_, v) = e
        .me(
            "PATCH",
            &format!("/kb/docs/{id}"),
            Some(json!({"level": null})),
        )
        .await;
    assert_eq!(v["level"], "reasoning");
    assert_eq!(v["inherited"]["level"], true);
    assert_eq!(ids(&e.search(ME, "小王").await), vec![id.clone()]);
    let (s, _) = e
        .me(
            "PATCH",
            &format!("/kb/docs/{id}"),
            Some(json!({"level": "secret"})),
        )
        .await;
    assert_eq!(s, 400);

    // Paused sources take no part in retrieval.
    fixture::pause_source(e.env.store(), &src_id).unwrap();
    assert!(e.search(ME, "值班").await.is_empty());
}

#[tokio::test]
async fn old_bot_memories_become_searchable() {
    let e = setup();
    fixture::seed_bot_memory(e.env.store()).unwrap();
    assert!(e.search(ME, "冒烟测试").await.is_empty());
    // What the 5-minute sweep (or the `bot` bus event) does, triggered directly.
    sync::on_event(
        &e.env.modules,
        &steve_types::BusEvent::from(steve_types::DomainEvent::from(
            steve_types::ConfigEvent::Bot {
                ws: steve_types::WorkspaceId::from(steve_store::WS),
                id: {
                    let value: String = "bot_k1".into();
                    steve_types::BotId::from(value)
                },
            },
        )),
    )
    .unwrap();
    let hits = e.search(OTHER, "冒烟测试").await;
    assert_eq!(ids(&hits), vec!["mem_1".to_string()]);
    assert_eq!(hits[0]["type"], "bot_mem");
    assert_eq!(hits[0]["label"], "小运 的记忆");
    assert_eq!(hits[0]["href"], "#/bots/bot_k1");
    // Edited and deleted in the old table: the sweep follows.
    fixture::edit_bot_memory(e.env.store()).unwrap();
    sync::sweep(&e.env.modules).unwrap();
    assert!(e.search(ME, "冒烟").await.is_empty());
    assert_eq!(e.search(ME, "回归测试").await.len(), 1);
    fixture::clear_bot_memories(e.env.store()).unwrap();
    sync::on_event(
        &e.env.modules,
        &steve_types::BusEvent::from(steve_types::DomainEvent::from(
            steve_types::ConfigEvent::Bot {
                ws: steve_types::WorkspaceId::from(steve_store::WS),
                id: {
                    let value: String = "*".into();
                    steve_types::BotId::from(value)
                },
            },
        )),
    )
    .unwrap();
    assert!(e.search(ME, "回归测试").await.is_empty());
}

#[tokio::test]
async fn sources_list_and_doc_rules() {
    let e = setup();
    let (s, list) = e.me("GET", "/kb/sources", None).await;
    assert_eq!(s, 200);
    let kinds: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["notes", "files", "items", "chat", "mems", "bot_mems"]
    );
    let notes = &list[0];
    assert_eq!(notes["name"], "我的笔记");
    assert_eq!(notes["builtin"], true);
    assert_eq!(notes["paused"], false);
    assert_eq!(
        notes["stats"],
        json!({"docs": 0, "chunks": 0, "skipped": 0})
    );
    // A second call does not duplicate them.
    let (_, again) = e.me("GET", "/kb/sources", None).await;
    assert_eq!(again.as_array().unwrap().len(), 6);

    // Documents that follow an item cannot be deleted on their own.
    let items_src = list[2]["id"].as_str().unwrap().to_string();
    fixture::seed_item_doc(e.env.store(), &items_src).unwrap();
    let (s, v) = e.me("DELETE", "/kb/docs/kd_item", None).await;
    assert_eq!(s, 400);
    assert_eq!(
        v["error"],
        "这篇跟随事项，不能单独删除，可以把级别改成“不可使用”"
    );

    // Validation.
    let (s, v) = e.me("POST", "/kb/notes", Some(json!({"title": ""}))).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("参数不对：title")));
    let (s, v) = e.me("GET", "/kb/search?q=", None).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("参数不对：q")));
    let (s, _) = e.me("GET", "/kb/search?q=a&limit=21", None).await;
    assert_eq!(s, 400);
    // a token in the query is searched as written
    let (s, v) = e
        .me(
            "GET",
            &format!("/kb/search?q={}", enc("令牌 ghp_abc123")),
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, _) = e.me("GET", "/kb/docs?limit=500", None).await;
    assert_eq!(s, 400);
    let (s, _) = call(&e.app, None, "GET", "/kb/sources", None).await;
    assert_eq!(s, 401);
}

#[tokio::test]
async fn tokens_are_indexed_as_written() {
    let e = setup();
    let doc = e
        .note(
            "CI 配置",
            "CI 用的令牌是 ghp_abcDEF123，别外传。",
            json!({}),
        )
        .await;
    let preview = doc["preview"][0]["text"].as_str().unwrap();
    assert!(preview.contains("ghp_abcDEF123"), "{preview}");
    let hits = e.search(ME, "CI 令牌").await;
    assert_eq!(hits.len(), 1);
    assert!(hits[0]["text"].as_str().unwrap().contains("ghp_abcDEF123"));
}

#[tokio::test]
async fn single_character_and_typo_queries() {
    let e = setup();
    let doc = e
        .note("部署脚本", "上线流程：先跑冒烟，再发布。", json!({}))
        .await;
    let id = doc["id"].as_str().unwrap().to_string();
    // One character: LIKE on titles and memory text.
    assert_eq!(ids(&e.search(ME, "署").await), vec![id.clone()]);
    // A typo breaks the strict phrase; the relaxed pass still finds it by `脚本`.
    assert_eq!(ids(&e.search(ME, "部暑脚本").await), vec![id.clone()]);
    // Only punctuation: nothing, not an error.
    assert!(e.search(ME, "？！").await.is_empty());
}

#[tokio::test]
async fn queue_worker_reindexes_documents() {
    let e = setup();
    let doc = e.note("队列", "第一版内容", json!({})).await;
    let id = doc["id"].as_str().unwrap().to_string();
    // Body changed behind the index's back (as a folder file would); the worker picks it up.
    fixture::edit_doc_without_indexing(e.env.store(), &id).unwrap();
    let worker = index::worker(e.env.modules.clone(), e.kb.clone()).unwrap();
    assert!(index::spawn_worker(e.env.modules.clone(), e.kb.clone()).is_none());
    e.env.events.lock().unwrap().clear();
    e.kb.enqueue(&id);
    assert!(
        e.knowledge_events().is_empty(),
        "preparing a worker does not run it"
    );
    let handle = tokio::spawn(worker);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while e.knowledge_events().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the document queued before startup is indexed");
    assert_eq!(e.knowledge_events()[0].0, id);
    assert_eq!(ids(&e.search(ME, "第二版").await), vec![id.clone()]);
    assert!(e.search(ME, "第一版").await.is_empty());
    handle.abort();
}

#[test]
fn mem_rows_follow_their_memory_scope() {
    let env = TestEnv::new(|_| {});
    let now = steve_store::now();
    let id = env
        .store()
        .write(|tx| {
            let m = steve_store::knowledge::Memories(tx).create(
                WS,
                &NewMem {
                    text: "团队周会改到周二".into(),
                    visibility: Visibility::Workspace,
                    level: Level::Reasoning,
                    created_by: ME.into(),
                    ..Default::default()
                },
            )?;
            sync::sync_mem(tx, &m.id, now)?;
            Ok::<_, StoreError>(m.id)
        })
        .unwrap();
    let (vis, level): (String, String) = fixture::memory_chunk_scope(env.store(), &id).unwrap();
    assert_eq!((vis.as_str(), level.as_str()), ("workspace", "reasoning"));
}

/// A memory's fixed stored source (“你写的”) reads in the request's language; the row keeps it as stored.
#[test]
fn mem_source_label_in_the_request_language() {
    let env = TestEnv::new(|_| {});
    let shown = env
        .store()
        .write(|tx| {
            let m = steve_store::knowledge::Memories(tx).create(
                WS,
                &NewMem {
                    text: "周会改到周二".into(),
                    source_type: "me".into(),
                    source_label: "你写的".into(),
                    created_by: ME.into(),
                    ..Default::default()
                },
            )?;
            let en = steve_i18n::with("en", || steve_knowledge::service::mem_json(tx, &m));
            let zh = steve_knowledge::service::mem_json(tx, &m);
            Ok::<_, StoreError>((
                en["source_label"].clone(),
                zh["source_label"].clone(),
                m.source_label,
            ))
        })
        .unwrap();
    assert_eq!(
        shown,
        (
            json!("Written by you"),
            json!("你写的"),
            "你写的".to_string()
        )
    );
}

/// A store that answers with anything (a broken or foreign store): the Hub's second filter keeps
/// only what the viewer may read, and never `never`.
struct Leaky;

impl steve_knowledge::port::KnowledgeStore for Leaky {
    fn id(&self) -> &'static str {
        "leaky"
    }
    fn search(
        &self,
        _: &mut steve_store::R<'_>,
        _: &steve_knowledge::port::Scope,
        _: &str,
        pass: steve_knowledge::port::Pass,
        _: usize,
    ) -> steve_store::Result<Vec<steve_knowledge::port::Candidate>> {
        if pass != steve_knowledge::port::Pass::Strict {
            return Ok(Vec::new());
        }
        use steve_store::base::{Level, Visibility};
        let c =
            |id: &str, vis: Visibility, by: &str, level: Level| steve_knowledge::port::Candidate {
                obj_type: "mem".into(),
                obj_id: id.into(),
                seq: 0,
                heading: String::new(),
                text: format!("部署 {id}"),
                source_kind: "mems".into(),
                label: String::new(),
                level,
                visibility: vis,
                created_by: by.into(),
                updated: 0,
                base: 1.0,
                title: None,
                doc_ref: None,
            };
        Ok(vec![
            c("ok_ws", Visibility::Workspace, OTHER, Level::Public),
            c("mine", Visibility::Private, ME, Level::Public),
            c("theirs", Visibility::Private, OTHER, Level::Public),
            c("secret", Visibility::Workspace, ME, Level::Never),
        ])
    }
}

#[tokio::test]
async fn single_user_second_filter_admits_other_creators_but_drops_never() {
    let e = setup();
    let viewer = Viewer::bare(WS, ME);
    let hits = e
        .env
        .store()
        .read(|c| retrieve(c, &Query::new(&Leaky, &viewer, "部署", Purpose::Search)))
        .unwrap();
    let mut ids: Vec<_> = hits.iter().map(|h| h.obj_id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["mine", "ok_ws", "theirs"]);
}

#[tokio::test]
async fn single_user_mail_references_keep_workspace_and_viewer_free_index_boundaries() {
    let e = setup();
    fixture::seed_mail_threads(e.env.store()).unwrap();
    let about = json!([{"type": "mail_thread", "id": "private_mail"}]);
    let (status, body) = e
        .me(
            "POST",
            "/kb/mems",
            Some(json!({"text": "probe", "about": about})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = e.me("POST", "/kb/mems", Some(json!({"text": "foreign probe", "about": [{"type": "mail_thread", "id": "foreign_mail"}]}))).await;
    assert_eq!(status, 404, "{body}");
    let (status, shared) = e.me("POST", "/kb/mems", Some(json!({"text": "public fact", "about": [{"type": "mail_thread", "id": "shared_mail"}]}))).await;
    assert_eq!(status, 200, "{shared}");
    assert_eq!(shared["about"][0]["name"], "Shared mail subject");

    // Viewer-bound reads may resolve private subjects; the viewer-free index remains separate.
    let (status, memory) = e
        .as_(
            OTHER,
            "POST",
            "/kb/mems",
            Some(json!({
                "text": "historical shared fact", "visibility": "workspace", "about": about
            })),
        )
        .await;
    assert_eq!(status, 200, "{memory}");
    assert_eq!(memory["about"][0]["name"], "Secret mail subject");
    let id = memory["id"].as_str().unwrap();
    let (status, listed) = e.me("GET", "/kb/mems", None).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap()["about"][0]["name"],
        "Secret mail subject"
    );
    assert!(listed.to_string().contains("Secret mail subject"));
    let tool = steve_knowledge::steward::call(
        &e.env.modules,
        &steve_modules::contacts::mcp::McpCx {
            ws: WS.into(),
            user: ME.into(),
            bot_id: "fixture".into(),
            item_id: None,
            session_id: None,
        },
        steve_knowledge::steward::LIST,
        &json!({}),
    )
    .unwrap();
    assert!(tool.contains("Secret mail subject"));
    let (status, subjects) = e.me("GET", "/kb/subjects", None).await;
    assert_eq!(status, 200, "{subjects}");
    assert!(subjects.to_string().contains("Secret mail subject"));
    let (status, body) = e
        .me("GET", "/kb/subjects/mail_thread/private_mail", None)
        .await;
    assert_eq!(status, 200, "{body}");
    let hits = e.search(ME, "historical shared fact").await;
    assert!(!hits.is_empty());
    assert!(
        !serde_json::to_string(&hits)
            .unwrap()
            .contains("Secret mail subject")
    );
    // Shared notes' historical `about` values use the same reader-side name check.
    let (status, note) = e.as_(OTHER, "POST", "/kb/notes", Some(json!({
        "title": "Shared note", "body": "historical note", "visibility": "workspace", "about": about
    }))).await;
    assert_eq!(status, 200, "{note}");
    assert_eq!(note["about"][0]["name"], "Secret mail subject");
    let (status, read) = e
        .me(
            "GET",
            &format!("/kb/docs/{}", note["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["about"][0]["name"], "Secret mail subject");
}

#[test]
fn single_user_mail_reference_reads_do_not_need_membership_scopes() {
    let e = setup();
    fixture::seed_scoped_mail_threads(e.env.store()).unwrap();
    e.env
        .store()
        .read(|c| {
            let bare = Viewer::bare(WS, ME);
            for id in ["team_mail", "space_mail"] {
                let r = ObjRef::new("mail_thread", id);
                assert!(steve_store::References(c).exists(&bare, &r)?);
                assert!(
                    steve_store::References(c)
                        .in_workspace(&bare, &r)?
                        .is_some()
                );
                // A viewer-free persistent index must not copy source-private subjects.
                assert!(steve_store::References(c).name(&r)?.is_none());
            }
            Ok::<_, StoreError>(())
        })
        .unwrap();
}
