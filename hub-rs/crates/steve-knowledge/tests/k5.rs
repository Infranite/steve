//! K5 acceptance: both examples with a fixed model reply (preview
//! lines, apply, undo); a draft with a refused path has one line with the reason and no operation;
//! plus the `kb_source:<id>` patch (owner rule, staleness) and `doc.patch` / `note.create`.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use axum::Router;
use serde_json::{Value, json};
use steve_knowledge::sources::folder::FolderPolicy;
use steve_knowledge::{Knowledge, drafts};
use steve_modules::changes::{self, Origin, Proposal};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, call, caller};
use steve_store::base::Change;
use steve_store::base::changesets;

struct Env {
    env: TestEnv,
    kb: Knowledge,
    app: Router,
    home: PathBuf,
    _home_dir: tempfile::TempDir,
}

fn setup() -> Env {
    let kb = Knowledge::new(|_| {});
    let k2 = kb.clone();
    let env = TestEnv::new(move |r| drafts::register(r, k2));
    let home_dir = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(home_dir.path()).unwrap();
    for d in [".steve", ".ssh", "wiki/客户"] {
        fs::create_dir_all(home.join(d)).unwrap();
    }
    fs::write(
        home.join("wiki/客户/Acme.md"),
        "# Acme 合同说明\n\nAcme 的付款周期是 60 天。\n",
    )
    .unwrap();
    assert!(kb.set_folder_policy(FolderPolicy::new(&home, home.join(".steve"))));
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO parties (id, workspace_id, type, name, aliases, created_by, created, updated) VALUES
                   ('org_acme', 'default', 'org', 'Acme', '[\"艾克米\"]', 'usr_dev', 1, 1);",
            )?;
            Ok(())
        })
        .unwrap();
    let app = steve_knowledge::routes(kb.clone()).with_state(env.modules.clone());
    Env {
        env,
        kb,
        app,
        home,
        _home_dir: home_dir,
    }
}

fn line(label: &str, from: &str, to: &str) -> Change {
    Change::new(label, from, to)
}

impl Env {
    /// Drafts `request` on `kind[:id]` as `who` with the model replying `ops`.
    async fn draft(
        &self,
        who: &str,
        kind: &str,
        id: Option<&str>,
        request: &str,
        ops: Value,
    ) -> Proposal {
        self.env.gateway.reply(json!({ "ops": ops }).to_string());
        changes::draft(
            &self.env.modules,
            &caller(WS, who),
            kind,
            id,
            request,
            Origin::default(),
        )
        .await
        .unwrap()
    }
    fn ops(&self, id: &str) -> Vec<Value> {
        self.env
            .store()
            .reader(|c| changesets::get(c, id))
            .unwrap()
            .unwrap()
            .ops
    }
    fn apply(&self, who: &str, id: &str) -> Result<Value, (u16, String)> {
        changes::apply(&self.env.modules, &caller(WS, who), id)
            .expect("stored changeset")
            .map_err(|e| (e.status(), e.to_string()))
    }
    fn undo(&self, who: &str, id: &str) {
        changes::undo(&self.env.modules, &caller(WS, who), id)
            .expect("stored changeset")
            .unwrap();
    }
    async fn get(&self, who: &str, path: &str) -> Value {
        let (s, v) = call(&self.app, Some(&caller(WS, who)), "GET", path, None).await;
        assert_eq!(s, 200, "{path}: {v}");
        v
    }
    fn count(&self, sql: &str) -> i64 {
        self.env
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(sql, [], |r| r.get::<_, i64>(0))?)
            })
            .unwrap()
    }
    async fn source(&self, kind: &str) -> Value {
        let list = self.get(ME, "/kb/sources").await;
        list.as_array()
            .unwrap()
            .iter()
            .find(|s| s["kind"] == kind)
            .cloned()
            .unwrap_or(Value::Null)
    }
}

#[tokio::test]
async fn remember_for_reasoning_only() {
    let e = setup();
    let p = e
        .draft(
            ME,
            "knowledge",
            None,
            "记住：Acme 的合同每年 3 月续签，这个只用于推理",
            json!([{"op": "mem.add", "kind": "fact", "text": "Acme 的合同每年 3 月续签", "level": "reasoning", "about": ["Acme"]}]),
        )
        .await;
    // the model was offered the knowledge operations
    let prompt = e.env.gateway.last_prompt().unwrap();
    assert!(prompt.contains("mem.add ·") && prompt.contains("kb_source.create ·"));
    assert_eq!(p.title, "知识库");
    assert_eq!(p.target, json!({"kind": "knowledge"}));
    assert_eq!(
        p.changes,
        vec![line(
            "新记忆",
            "—",
            "Acme 的合同每年 3 月续签 · 事实 · 仅用于推理 · 只有我 · 关于 Acme"
        )]
    );
    assert_eq!(
        e.ops(&p.id),
        vec![json!({
            "op": "kb.mem.add", "kind": "fact", "text": "Acme 的合同每年 3 月续签",
            "level": "reasoning", "visibility": "private",
            "about": [{"type": "party", "id": "org_acme"}],
        })]
    );
    assert_eq!(
        e.count("SELECT COUNT(*) FROM mems"),
        0,
        "nothing before apply"
    );

    assert_eq!(e.apply(ME, &p.id).unwrap()["ok"], true);
    let mems = e.get(ME, "/kb/mems").await;
    let m = &mems[0];
    assert_eq!(
        (
            m["text"].as_str(),
            m["status"].as_str(),
            m["level"].as_str(),
            m["source_type"].as_str()
        ),
        (
            Some("Acme 的合同每年 3 月续签"),
            Some("confirmed"),
            Some("reasoning"),
            Some("me")
        )
    );
    assert_eq!(
        m["about"],
        json!([{"type": "party", "id": "org_acme", "name": "Acme"}])
    );
    // indexed at once, with its level; private to me
    let hits = e.get(ME, "/kb/search?q=%E7%BB%AD%E7%AD%BE").await;
    assert_eq!(hits["hits"][0]["level"], "reasoning");
    assert_eq!(hits["hits"][0]["text"], "Acme：Acme 的合同每年 3 月续签");
    let other = e.get(OTHER, "/kb/search?q=%E7%BB%AD%E7%AD%BE").await;
    assert_eq!(other["hits"], json!([]));

    e.undo(ME, &p.id);
    assert_eq!(e.count("SELECT COUNT(*) FROM mems"), 0);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM kb_chunks WHERE obj_type = 'mem'"),
        0
    );
}

#[tokio::test]
async fn about_names_resolve_only_exactly() {
    let e = setup();
    let p = e
        .draft(
            ME,
            "knowledge",
            None,
            "记住：艾克米和 Acm 都不喜欢长邮件",
            json!([{"op": "kb.mem.add", "kind": "偏好", "text": "不喜欢长邮件", "about": ["艾克米", "Acm"]}]),
        )
        .await;
    assert_eq!(
        p.changes,
        vec![
            line(
                "新记忆",
                "—",
                "不喜欢长邮件 · 偏好 · 可转述结论 · 只有我 · 关于 Acme"
            ),
            line("（没认出：Acm）", "—", "没有加进“关于”"),
        ]
    );
    // an operation this target does not offer
    let p = e
        .draft(
            ME,
            "knowledge",
            None,
            "x",
            json!([{"op": "party.patch", "id": "ct_1"}]),
        )
        .await;
    assert_eq!(p.changes[0].label, "（没认出）");
    assert!(e.ops(&p.id).is_empty());
}

async fn wait_docs(e: &Env, source: &str, n: i64) {
    for _ in 0..250 {
        let docs = e.count(&format!(
            "SELECT COUNT(*) FROM kb_docs WHERE source_id = '{source}' AND status = 'active' AND indexed IS NOT NULL"
        ));
        let syncing = e.count(&format!(
            "SELECT COUNT(*) FROM kb_sources WHERE id = '{source}' AND status = 'syncing'"
        ));
        if docs == n && syncing == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("source {source} never reached {n} documents");
}

#[tokio::test]
async fn add_wiki_folder_private() {
    let e = setup();
    let scans = steve_knowledge::spawn_scans(e.env.modules.clone(), e.kb.clone());
    let wiki = e.home.join("wiki");
    let p = e
        .draft(
            ME,
            "knowledge",
            None,
            "把 ~/wiki 加进知识库，只有我能看",
            json!([{"op": "kb_source.create", "kind": "folder", "path": "~/wiki", "visibility": "private"}]),
        )
        .await;
    assert_eq!(
        p.changes,
        vec![line(
            &format!("新增知识来源 {}", wiki.display()),
            "—",
            "「wiki」 · 只有我 · 可转述结论"
        )]
    );
    let ops = e.ops(&p.id);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0]["op"], "kb.source.create");
    assert_eq!(ops[0]["path"], wiki.to_string_lossy().as_ref());
    assert_eq!(ops[0]["visibility"], "private");

    assert_eq!(e.apply(ME, &p.id).unwrap()["ok"], true);
    let src = e.source("folder").await;
    assert_eq!(
        (
            src["name"].as_str(),
            src["path"].as_str(),
            src["visibility"].as_str()
        ),
        (Some("wiki"), wiki.to_str(), Some("private"))
    );
    let sid = src["id"].as_str().unwrap().to_string();
    // the first scan runs right after the commit (woken, not the 10-minute tick)
    wait_docs(&e, &sid, 1).await;
    let hits = e
        .get(ME, "/kb/search?q=%E4%BB%98%E6%AC%BE%E5%91%A8%E6%9C%9F")
        .await;
    assert_eq!(hits["hits"][0]["title"], "Acme");
    let other = e
        .get(OTHER, "/kb/search?q=%E4%BB%98%E6%AC%BE%E5%91%A8%E6%9C%9F")
        .await;
    assert_eq!(other["hits"], json!([]));

    // drafting the same folder again: overlap is refused while planning
    let again = e
        .draft(
            ME,
            "knowledge",
            None,
            "再加一次",
            json!([{"op": "kb_source.create", "path": wiki.join("客户")}]),
        )
        .await;
    assert_eq!(
        again.changes,
        vec![line(
            &format!("新增知识来源 {}", wiki.join("客户").display()),
            "—",
            "这个目录不能加入知识库：和已有的文件夹来源“wiki”重叠"
        )]
    );
    assert!(e.ops(&again.id).is_empty());

    e.undo(ME, &p.id);
    assert_eq!(e.source("folder").await, Value::Null);
    assert_eq!(
        e.count(&format!(
            "SELECT COUNT(*) FROM kb_docs WHERE source_id = '{sid}'"
        )),
        0
    );
    assert_eq!(
        e.count("SELECT COUNT(*) FROM kb_chunks WHERE source_kind = 'folder'"),
        0
    );
    scans.abort();
}

#[tokio::test]
async fn refused_paths_give_one_line_and_no_change() {
    let e = setup();
    let h = e.home.display().to_string();
    let cases = [
        (
            "~/.ssh",
            format!("新增知识来源 {h}/.ssh"),
            "这个目录不能加入知识库：不能是 ~/.ssh 或其中的目录".to_string(),
        ),
        (
            "~/nope",
            format!("新增知识来源 {h}/nope"),
            "这个目录不能加入知识库：目录不存在".into(),
        ),
        (
            "~",
            format!("新增知识来源 {h}"),
            "这个目录不能加入知识库：不能是整个主目录".into(),
        ),
        (
            "~/.steve",
            format!("新增知识来源 {h}/.steve"),
            "这个目录不能加入知识库：不能是 Steve 自己的数据目录".into(),
        ),
        (
            "wiki",
            "新增知识来源 wiki".into(),
            "这个目录不能加入知识库：需要绝对路径".into(),
        ),
        (
            "/etc",
            "新增知识来源 /etc".into(),
            "只能添加你主目录或外接磁盘里的文件夹".into(),
        ),
    ];
    for (path, label, reason) in cases {
        let p = e
            .draft(
                ME,
                "kb_source",
                None,
                &format!("把 {path} 加进知识库"),
                json!([{"op": "kb_source.create", "kind": "folder", "path": path, "visibility": "private"}]),
            )
            .await;
        assert_eq!(p.changes, vec![line(&label, "—", &reason)], "{path}");
        assert!(e.ops(&p.id).is_empty(), "{path}");
        // applying it changes nothing
        assert_eq!(e.apply(ME, &p.id).unwrap()["ok"], true);
        assert_eq!(
            e.count("SELECT COUNT(*) FROM kb_sources WHERE kind = 'folder'"),
            0
        );
    }
    // no path at all
    let p = e
        .draft(
            ME,
            "knowledge",
            None,
            "加个文件夹",
            json!([{"op": "kb_source.create"}]),
        )
        .await;
    assert_eq!(p.changes, vec![line("（没说清楚）", "—", "缺少文件夹路径")]);
    assert!(e.ops(&p.id).is_empty());
}

#[tokio::test]
async fn source_patch_owner_rule_staleness_and_undo() {
    let e = setup();
    e.get(ME, "/kb/sources").await; // creates the built-ins
    let notes = e.source("notes").await;
    let nid = notes["id"].as_str().unwrap();
    // candidates for pickTarget
    let cands = changes::candidates(&e.env.modules, &caller(WS, ME)).unwrap();
    let codes: Vec<String> = cands.iter().map(|c| c.line()).collect();
    assert!(codes.contains(&"knowledge:new — 记一条记忆或笔记".to_string()));
    assert!(codes.contains(&format!("kb_source:{nid} — 知识来源 我的笔记")));

    // a member may pause a built-in source but not rescope it
    let p = e
        .draft(
            OTHER,
            "kb_source",
            Some(nid),
            "笔记改成工作空间可见、仅用于推理，先暂停",
            json!([
                {"op": "set", "key": "visibility", "value": "工作空间"},
                {"op": "set", "key": "level", "value": "仅用于推理"},
                {"op": "set", "key": "paused", "value": "已暂停"},
            ]),
        )
        .await;
    assert_eq!(
        p.changes,
        vec![
            line(
                "可见范围",
                "只有我",
                "（没有改：只有工作空间的所有者能改内置来源）"
            ),
            line(
                "披露级别",
                "可转述结论",
                "（没有改：只有工作空间的所有者能改内置来源）"
            ),
            line("状态", "同步中", "已暂停"),
        ]
    );
    assert_eq!(
        e.ops(&p.id),
        vec![json!({"op": "kb.source.patch", "id": nid, "patch": {"paused": true}})]
    );

    // the owner may; the preview shows labels
    let p = e
        .draft(
            ME,
            "kb_source",
            Some(nid),
            "笔记仅用于推理",
            json!([{"op": "set", "key": "level", "value": "reasoning"}, {"op": "set", "key": "include", "value": ["**/*.md"]}]),
        )
        .await;
    assert_eq!(p.title, "知识来源「我的笔记」");
    assert_eq!(
        p.changes,
        vec![
            line("披露级别", "可转述结论", "仅用于推理"),
            line("（没认出）", "—", "include: [\"**/*.md\"]"),
        ]
    );
    assert_eq!(e.apply(ME, &p.id).unwrap()["ok"], true);
    assert_eq!(e.source("notes").await["level"], "reasoning");
    e.undo(ME, &p.id);
    assert_eq!(e.source("notes").await["level"], "conclusion");

    // the target moved after drafting → 409
    let p = e
        .draft(
            ME,
            "kb_source",
            Some(nid),
            "改名",
            json!([{"op": "set", "key": "name", "value": "随手记"}]),
        )
        .await;
    assert_eq!(p.changes, vec![line("名字", "我的笔记", "随手记")]);
    let (s, _) = call(
        &e.app,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/kb/sources/{nid}"),
        Some(&json!({"paused": true})),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(
        e.apply(ME, &p.id),
        Err((409, "这份草稿已经失效，请重新说一次".into()))
    );
    assert_eq!(e.source("notes").await["name"], "我的笔记");
}

#[tokio::test]
async fn note_create_and_doc_patch() {
    let e = setup();
    let p = e
        .draft(
            ME,
            "knowledge",
            None,
            "记一篇笔记：发布流程",
            json!([{"op": "note.create", "title": "发布流程", "body": "先跑 e2e，再切流量。"}]),
        )
        .await;
    assert_eq!(p.changes, vec![line("新建笔记", "—", "发布流程")]);
    e.apply(ME, &p.id).unwrap();
    let note_cs = p.id.clone();
    let docs = e.get(ME, "/kb/docs").await;
    assert_eq!(docs[0]["title"], "发布流程");
    let doc = docs[0]["id"].as_str().unwrap().to_string();

    let p = e
        .draft(
            ME,
            "knowledge",
            None,
            "发布流程那篇不要用",
            json!([{"op": "doc.patch", "doc": "发布流程", "level": "不可使用"}, {"op": "doc.patch", "doc": "不存在的文档", "level": "public"}]),
        )
        .await;
    assert_eq!(
        p.changes,
        vec![
            line("「发布流程」的披露级别", "可转述结论", "不可使用"),
            line("（没认出：不存在的文档）", "—", "没有这篇文档"),
        ]
    );
    e.apply(ME, &p.id).unwrap();
    let d = e.get(ME, &format!("/kb/docs/{doc}")).await;
    assert_eq!(d["level"], "never");
    let hits = e.get(ME, "/kb/search?q=e2e").await;
    assert_eq!(hits["hits"], json!([]), "never is not retrieved");
    assert_eq!(
        e.count(&format!(
            "SELECT COUNT(*) FROM kb_chunks WHERE obj_id = '{doc}' AND level = 'never'"
        )),
        1
    );
    e.undo(ME, &p.id);
    let d = e.get(ME, &format!("/kb/docs/{doc}")).await;
    assert_eq!(
        (d["level"].as_str(), d["inherited"]["level"].as_bool()),
        (Some("conclusion"), Some(true))
    );
    assert_eq!(
        e.get(ME, "/kb/search?q=e2e").await["hits"][0]["id"],
        doc.as_str()
    );
    // undoing the note removes it from the index
    e.undo(ME, &note_cs);
    assert_eq!(e.get(ME, "/kb/docs").await, json!([]));
    assert_eq!(e.get(ME, "/kb/search?q=e2e").await["hits"], json!([]));
}
