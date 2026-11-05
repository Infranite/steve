//! K2 acceptance: every refusal of the folder path check, links out of the
//! folder are not read, changed files are re-chunked and vanished ones become `deleted`, front
//! matter `steve.level: never` takes effect, tokens are stored as “［已拦下的机密］”; plus the
//! source writes and `POST /kb/files`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use serde_json::{Value, json};
use steve_knowledge::repo;
use steve_knowledge::sources::files::blob_path;
use steve_knowledge::sources::folder::{self, FolderPolicy, OUTSIDE, check_path};
use steve_knowledge::{FilesLoader, Knowledge, TokenGuard, index};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, caller};
use steve_store::base::Visibility;

const TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

struct Env {
    env: TestEnv,
    kb: Knowledge,
    app: Router,
    /// Plays `$HOME`; `STEVE_HOME` is `<home>/.steve`.
    home: PathBuf,
    _home_dir: tempfile::TempDir,
}

fn setup() -> Env {
    let env = TestEnv::new(|_| {});
    let home_dir = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(home_dir.path()).unwrap();
    for d in [".steve/data", ".ssh/keys", ".gnupg", "Library/Mail", "wiki"] {
        fs::create_dir_all(home.join(d)).unwrap();
    }
    let kb = Knowledge::with_loaders(
        Arc::new(TokenGuard),
        |_| {},
        vec![Arc::new(FilesLoader::new(env.dir.path().join("data")))],
    );
    assert!(kb.set_folder_policy(FolderPolicy::new(&home, home.join(".steve"))));
    let app = steve_knowledge::routes(kb.clone()).with_state(env.modules.clone());
    Env {
        env,
        kb,
        app,
        home,
        _home_dir: home_dir,
    }
}

fn write(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Env {
    async fn req(&self, who: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
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
        self.req(ME, method, path, body).await
    }
    fn wiki(&self) -> PathBuf {
        self.home.join("wiki")
    }
    /// Adds a folder source through the API and waits for its first scan.
    async fn add_folder(&self, path: &Path, extra: Value) -> Value {
        let mut b = json!({"kind": "folder", "name": "我的 Wiki", "path": path});
        if let Value::Object(o) = extra {
            for (k, v) in o {
                b[k] = v;
            }
        }
        let (s, v) = self.me("POST", "/kb/sources", Some(b)).await;
        assert_eq!(s, 200, "{v}");
        self.wait(v["id"].as_str().unwrap()).await
    }
    /// The source once no scan is running.
    async fn wait(&self, id: &str) -> Value {
        for _ in 0..200 {
            let (_, list) = self.me("GET", "/kb/sources", None).await;
            if let Some(s) = list.as_array().unwrap().iter().find(|s| s["id"] == id)
                && s["status"] != "syncing"
            {
                return s.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("source {id} still syncing");
    }
    async fn sync(&self, id: &str) -> folder::SyncReport {
        folder::sync_now(&self.env.modules, &self.kb, id)
            .await
            .unwrap()
            .expect("claimed")
    }
    async fn search(&self, who: &str, q: &str) -> Vec<Value> {
        let (s, v) = self
            .req(who, "GET", &format!("/kb/search?q={}", enc(q)), None)
            .await;
        assert_eq!(s, 200, "{v}");
        v["hits"].as_array().cloned().unwrap_or_default()
    }
    fn doc_by_ref(&self, source: &str, r: &str) -> repo::DocRow {
        self.env
            .store()
            .reader(|c| repo::doc_by_ref(c, source, r))
            .unwrap()
            .unwrap_or_else(|| panic!("no doc {r}"))
    }
    fn chunk_texts(&self, doc: &str) -> Vec<String> {
        self.env
            .store()
            .reader(|c| repo::doc_chunks(c, doc, 100))
            .unwrap()
            .into_iter()
            .map(|(_, _, t)| t)
            .collect()
    }
}

fn refusal(v: &Value) -> String {
    v["error"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn every_path_refusal() {
    let e = setup();
    let h = &e.home;
    write(&h.join("wiki/a.md"), "x");
    fs::create_dir_all(h.join("docs/wiki/sub")).unwrap();
    let cases: Vec<(String, &str)> = vec![
        ("wiki".into(), "需要绝对路径"),
        (format!("{}/wiki/../wiki", h.display()), "路径里不能有 .."),
        (format!("{}/nope", h.display()), "目录不存在"),
        (format!("{}/wiki/a.md", h.display()), "这不是一个目录"),
        ("/".into(), "不能是根目录"),
        (h.display().to_string(), "不能是整个主目录"),
        (
            format!("{}/.steve", h.display()),
            "不能是 Steve 自己的数据目录",
        ),
        (
            format!("{}/.steve/data", h.display()),
            "不能是 Steve 自己的数据目录",
        ),
        (
            format!("{}/.ssh", h.display()),
            "不能是 ~/.ssh 或其中的目录",
        ),
        (
            format!("{}/.ssh/keys", h.display()),
            "不能是 ~/.ssh 或其中的目录",
        ),
        (
            format!("{}/.gnupg", h.display()),
            "不能是 ~/.gnupg 或其中的目录",
        ),
        (
            format!("{}/Library/Mail", h.display()),
            "不能是 ~/Library 或其中的目录",
        ),
    ];
    for (path, reason) in &cases {
        let (s, v) = e
            .me(
                "POST",
                "/kb/sources",
                Some(json!({"kind": "folder", "name": "x", "path": path})),
            )
            .await;
        assert_eq!(s, 400, "{path}: {v}");
        assert_eq!(
            refusal(&v),
            format!("这个目录不能加入知识库：{reason}"),
            "{path}"
        );
    }
    // Only folders under $HOME or /Volumes (the message stands alone, without the prefix).
    for path in ["/etc", "/System", "/usr/share"] {
        if !Path::new(path).is_dir() {
            continue;
        }
        let (s, v) = e
            .me(
                "POST",
                "/kb/sources",
                Some(json!({"kind": "folder", "name": "x", "path": path})),
            )
            .await;
        assert_eq!(
            (s, refusal(&v).as_str()),
            (400, "只能添加你主目录或外接磁盘里的文件夹"),
            "{path}"
        );
    }
    // A link from inside the home to outside it is judged by where it points.
    std::os::unix::fs::symlink("/etc", h.join("etc-link")).unwrap();
    assert_eq!(
        check_path(e.kb.folder_policy(), &h.join("etc-link").to_string_lossy()),
        Err("只能添加你主目录或外接磁盘里的文件夹".into())
    );
    // A link to a forbidden place is judged by where it points.
    std::os::unix::fs::symlink(h.join(".ssh"), h.join("innocent")).unwrap();
    let (s, v) = e
        .me(
            "POST",
            "/kb/sources",
            Some(json!({"kind": "folder", "name": "x", "path": h.join("innocent")})),
        )
        .await;
    assert_eq!(
        (s, refusal(&v).as_str()),
        (400, "这个目录不能加入知识库：不能是 ~/.ssh 或其中的目录")
    );
    // A directory that contains STEVE_HOME (when it is not under the home dot-dirs).
    let p = FolderPolicy::new(h, h.join("docs/wiki/sub"));
    assert_eq!(
        check_path(&p, &h.join("docs").to_string_lossy()),
        Err("里面包含 Steve 自己的数据目录".into())
    );

    // Overlap with an existing folder source, both ways.
    e.add_folder(&h.join("docs/wiki"), json!({"name": "Wiki"}))
        .await;
    for p in [h.join("docs"), h.join("docs/wiki/sub"), h.join("docs/wiki")] {
        let (s, v) = e
            .me(
                "POST",
                "/kb/sources",
                Some(json!({"kind": "folder", "name": "x", "path": p})),
            )
            .await;
        assert_eq!(s, 400, "{v}");
        assert_eq!(
            refusal(&v),
            "这个目录不能加入知识库：和已有的文件夹来源“Wiki”重叠"
        );
    }
    // Another workspace may add the same folder (workspaces are separate data boundaries).
    let (s, _) = call(
        &e.app,
        Some(&caller(WS2, ME)),
        "POST",
        "/kb/sources",
        Some(&json!({"kind": "folder", "name": "x", "path": h.join("docs/wiki")})),
    )
    .await;
    assert_eq!(s, 200);

    // Field validation.
    for (b, field) in [
        (
            json!({"kind": "notes", "name": "x", "path": e.wiki()}),
            "kind",
        ),
        (
            json!({"kind": "folder", "name": "", "path": e.wiki()}),
            "name",
        ),
        (
            json!({"kind": "folder", "name": "x".repeat(41), "path": e.wiki()}),
            "name",
        ),
        (
            json!({"kind": "folder", "name": "x", "path": e.wiki(), "include": ["../*.md"]}),
            "include.0",
        ),
        (
            json!({"kind": "folder", "name": "x", "path": e.wiki(), "exclude": ["/etc/**"]}),
            "exclude.0",
        ),
    ] {
        let (s, v) = e.me("POST", "/kb/sources", Some(b)).await;
        assert_eq!(s, 400);
        assert!(refusal(&v).contains(field), "{v}");
    }
}

#[tokio::test]
async fn links_out_of_the_folder_are_not_read() {
    let e = setup();
    let outside = e.home.join("outside");
    write(&outside.join("secret.md"), "外面的秘密：工资表");
    write(&e.wiki().join("inside.md"), "里面的内容：部署脚本");
    write(&e.wiki().join("real/linked.md"), "链接到里面的页");
    let ln = |to: &Path, at: &str| std::os::unix::fs::symlink(to, e.wiki().join(at)).unwrap();
    ln(&outside.join("secret.md"), "escape.md");
    ln(&outside, "escape-dir");
    ln(Path::new("../outside/secret.md"), "relative.md");
    ln(&e.wiki().join("real/linked.md"), "alias.md");

    let s = e.add_folder(&e.wiki(), json!({})).await;
    let id = s["id"].as_str().unwrap();
    assert_eq!(s["status"], "ok", "{s}");
    assert_eq!(s["path"], e.wiki().to_string_lossy().as_ref());
    let (_, docs) = e.me("GET", &format!("/kb/docs?source={id}"), None).await;
    let mut refs: Vec<&str> = docs
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["ref"].as_str().unwrap())
        .collect();
    refs.sort();
    assert_eq!(refs, ["alias.md", "inside.md", "real/linked.md"]);
    assert!(e.search(ME, "工资表").await.is_empty());
    assert_eq!(e.search(ME, "部署").await.len(), 1);
    assert_eq!(e.sync(id).await.links_refused, 3);

    // A file swapped for an outside link between scan and read is refused by the loader.
    let doc = e.doc_by_ref(id, "inside.md");
    fs::remove_file(e.wiki().join("inside.md")).unwrap();
    ln(&outside.join("secret.md"), "inside.md");
    index::run_batch(&e.env.modules, &e.kb, std::slice::from_ref(&doc.id))
        .await
        .unwrap();
    let doc = e.doc_by_ref(id, "inside.md");
    assert_eq!(doc.status, "failed");
    assert_eq!(doc.error.as_deref(), Some(OUTSIDE));
    assert!(e.chunk_texts(&doc.id).is_empty());
    assert!(e.search(ME, "工资表").await.is_empty());
    assert!(e.search(ME, "部署").await.is_empty());
}

#[tokio::test]
async fn changed_files_are_rechunked_and_vanished_ones_deleted() {
    let e = setup();
    let a = e.wiki().join("运维/a.md");
    write(&a, "# 部署\n\n部署脚本在 ops 仓库。");
    write(&e.wiki().join("b.md"), "# 其他\n\n不相关的一页。");
    write(&e.wiki().join(".obsidian/w.md"), "工作区配置 部署");
    write(&e.wiki().join("big.md"), &"长".repeat(800_000));
    let s = e.add_folder(&e.wiki(), json!({})).await;
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(
        s["stats"],
        json!({"docs": 2, "chunks": 2, "skipped": 1}),
        "{s}"
    );
    let big = e.doc_by_ref(&id, "big.md");
    assert_eq!(
        (big.status.as_str(), big.error.as_deref()),
        ("skipped", Some(folder::TOO_BIG))
    );
    let hits = e.search(ME, "部署").await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["label"], "我的 Wiki · a");
    assert_eq!(hits[0]["source_kind"], "folder");
    let doc_a = e.doc_by_ref(&id, "运维/a.md");
    let b_indexed = e.doc_by_ref(&id, "b.md").indexed;

    // Nothing changed: nothing is read again.
    let r = e.sync(&id).await;
    assert_eq!((r.files, r.changed, r.deleted), (2, 0, 0));

    // Changed content (other size): re-chunked; the untouched file is not.
    write(&a, "# 回滚\n\n回滚步骤：先停流量，再切回上一个版本。");
    let r = e.sync(&id).await;
    assert_eq!(r.changed, 1);
    let texts = e.chunk_texts(&doc_a.id);
    assert!(texts.iter().any(|t| t.contains("回滚步骤")), "{texts:?}");
    assert!(e.search(ME, "部署").await.is_empty());
    assert_eq!(e.search(ME, "回滚").await.len(), 1);
    assert_eq!(e.doc_by_ref(&id, "b.md").indexed, b_indexed);
    assert_ne!(e.doc_by_ref(&id, "运维/a.md").sha256, doc_a.sha256);

    // Deleted: status deleted, chunks gone, no longer listed or readable.
    fs::remove_file(&a).unwrap();
    let r = e.sync(&id).await;
    assert_eq!(r.deleted, 1);
    let gone = e.doc_by_ref(&id, "运维/a.md");
    assert_eq!(gone.status, "deleted");
    assert!(e.chunk_texts(&gone.id).is_empty());
    assert!(e.search(ME, "回滚").await.is_empty());
    let (st, _) = e.me("GET", &format!("/kb/docs/{}", gone.id), None).await;
    assert_eq!(st, 404);
    // A folder document cannot be deleted from the knowledge base.
    let b = e.doc_by_ref(&id, "b.md");
    let (st, v) = e.me("DELETE", &format!("/kb/docs/{}", b.id), None).await;
    assert_eq!(
        (st, refusal(&v).as_str()),
        (400, "这篇来自文件夹，请在原处删除或在来源里排除")
    );

    // Back again: the same document is active and searchable.
    write(&a, "# 回滚\n\n回滚步骤又回来了。");
    e.sync(&id).await;
    let back = e.doc_by_ref(&id, "运维/a.md");
    assert_eq!(
        (back.id.as_str(), back.status.as_str()),
        (gone.id.as_str(), "active")
    );
    assert_eq!(e.search(ME, "回滚").await.len(), 1);

    // The whole folder gone: the source shows an error, the documents stay.
    let moved = e.home.join("wiki-moved");
    fs::rename(e.wiki(), &moved).unwrap();
    assert!(folder::sync_now(&e.env.modules, &e.kb, &id).await.is_err());
    let s = e.wait(&id).await;
    assert_eq!(s["status"], "error");
    assert!(s["error"].as_str().unwrap().contains("目录不存在"), "{s}");
    assert_eq!(e.doc_by_ref(&id, "b.md").status, "active");
}

#[tokio::test]
async fn front_matter_level_never_takes_effect() {
    let e = setup();
    let page = e.wiki().join("客户/Acme.md");
    write(
        &page,
        "---\ntitle: Acme 合同说明\ntags: [客户, 合同]\nsteve:\n  level: never\n---\n# 续签\n\nAcme 的合同每年三月续签。",
    );
    write(
        &e.wiki().join("p.md"),
        "---\nsteve.level: reasoning\nsteve.visibility: workspace\n---\n价格底线是八折。",
    );
    let s = e.add_folder(&e.wiki(), json!({})).await;
    let id = s["id"].as_str().unwrap().to_string();
    let d = e.doc_by_ref(&id, "客户/Acme.md");
    assert_eq!(d.title, "Acme 合同说明");
    assert_eq!(d.tags, ["客户", "合同"]);
    assert_eq!(d.level.map(|l| l.code()), Some("never"));
    assert!(e.search(ME, "续签").await.is_empty());
    assert!(e.search(ME, "合同说明").await.is_empty());
    let (_, brief) = e.me("GET", &format!("/kb/docs/{}", d.id), None).await;
    assert_eq!(brief["level"], "never");
    assert_eq!(
        brief["inherited"],
        json!({"visibility": true, "level": false})
    );
    // Front matter cannot widen a private source: the other member still does not see the page;
    // its level applies.
    assert!(e.search(OTHER, "底线").await.is_empty());
    let hits = e.search(ME, "底线").await;
    assert_eq!(hits.len(), 1);
    assert_eq!(
        (hits[0]["level"].as_str(), hits[0]["visibility"].as_str()),
        (Some("reasoning"), Some("private"))
    );

    // Changing the source's level does not lift the page's own `never`.
    let (st, v) = e
        .me(
            "PATCH",
            &format!("/kb/sources/{id}"),
            Some(json!({"level": "public"})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert!(e.search(ME, "续签").await.is_empty());

    // Front matter removed: the page follows the source again.
    write(&page, "# 续签\n\nAcme 的合同每年三月续签，提前一个月提醒。");
    e.sync(&id).await;
    let d = e.doc_by_ref(&id, "客户/Acme.md");
    assert_eq!((d.level, d.title.as_str()), (None, "Acme"));
    let hits = e.search(ME, "续签").await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["level"], "public");
}

#[tokio::test]
async fn front_matter_visibility_only_narrows() {
    let e = setup();
    write(&e.wiki().join("open.md"), "# 开放\n\n周会改到周四下午。");
    write(
        &e.wiki().join("closed.md"),
        "---\nsteve.visibility: private\n---\n# 私事\n\n周会前要先对齐预算。",
    );
    let s = e
        .add_folder(&e.wiki(), json!({"visibility": "workspace"}))
        .await;
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(e.search(OTHER, "周四").await.len(), 1);
    // narrowed: only the source's owner sees the page
    assert!(e.search(OTHER, "预算").await.is_empty());
    assert_eq!(e.search(ME, "预算").await.len(), 1);
    assert_eq!(
        e.doc_by_ref(&id, "closed.md").visibility.map(|v| v.code()),
        Some("private".into())
    );
    assert!(folder::narrows(
        &Visibility::Workspace,
        &Visibility::Private
    ));
    assert!(folder::narrows(&Visibility::Private, &Visibility::Private));
    assert!(!folder::narrows(
        &Visibility::Private,
        &Visibility::Workspace
    ));
    assert!(!folder::narrows(
        &Visibility::Private,
        &Visibility::Team("t1".into())
    ));
    assert!(folder::narrows(
        &Visibility::Team("t1".into()),
        &Visibility::Private
    ));
    assert!(!folder::narrows(
        &Visibility::Team("t1".into()),
        &Visibility::Team("t2".into())
    ));
    assert!(!folder::narrows(
        &Visibility::Team("t1".into()),
        &Visibility::Workspace
    ));
}

#[tokio::test]
async fn tokens_are_stored_redacted() {
    let e = setup();
    write(
        &e.wiki().join("ci.md"),
        &format!("# CI\n\n发布用的令牌是 {TOKEN}，别外传。"),
    );
    let s = e.add_folder(&e.wiki(), json!({})).await;
    let d = e.doc_by_ref(s["id"].as_str().unwrap(), "ci.md");
    let texts = e.chunk_texts(&d.id);
    assert_eq!(texts, ["发布用的令牌是 ［已拦下的机密］，别外传。"]);
    let (_, doc) = e.me("GET", &format!("/kb/docs/{}", d.id), None).await;
    assert!(!doc.to_string().contains("ghp_"), "{doc}");
    assert_eq!(e.search(ME, "令牌").await.len(), 1);
    let (st, v) = e.me("GET", &format!("/kb/search?q={TOKEN}"), None).await;
    assert_eq!(
        (st, refusal(&v).as_str()),
        (400, "检索词里像是有密码或令牌")
    );
}

/// An upload as files.ts `store` leaves it: bytes under `data/files`, a `files` row.
fn upload(e: &Env, ws: &str, id: &str, name: &str, mime: &str, bytes: &[u8]) {
    let sha = steve_store::sha(&String::from_utf8_lossy(bytes));
    let p = blob_path(&e.env.dir.path().join("data"), ws, &sha, id);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, bytes).unwrap();
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO files (id, workspace_id, sha256, name, mime, size, created_by, created) VALUES (?, ?, ?, ?, ?, ?, ?, 1)",
                rusqlite::params![id, ws, sha, name, mime, bytes.len() as i64, ME],
            )?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn files_are_added_indexed_and_removed() {
    let e = setup();
    upload(
        &e,
        WS,
        "fil_0000000000000001",
        "交接.md",
        "text/plain; charset=utf-8",
        format!("---\nsteve.level: reasoning\n---\n# 交接\n\n服务器令牌 {TOKEN}，值班表在共享盘。")
            .as_bytes(),
    );
    upload(
        &e,
        WS,
        "fil_0000000000000002",
        "图.png",
        "image/png",
        b"\x89PNG\r\n\x1a\n....",
    );
    upload(
        &e,
        WS2,
        "fil_0000000000000003",
        "别处.txt",
        "text/plain; charset=utf-8",
        b"x",
    );

    let (st, doc) = e
        .me(
            "POST",
            "/kb/files",
            Some(json!({"file_id": "fil_0000000000000001"})),
        )
        .await;
    assert_eq!(st, 200, "{doc}");
    let id = doc["id"].as_str().unwrap().to_string();
    assert_eq!(doc["source"]["kind"], "files");
    assert_eq!(doc["title"], "交接.md");
    assert_eq!(doc["level"], "reasoning");
    assert_eq!(doc["status"], "active");
    assert_eq!(
        doc["preview"][0]["text"],
        "服务器令牌 ［已拦下的机密］，值班表在共享盘。"
    );
    let hits = e.search(ME, "值班表").await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["label"], "文件 · 交接.md");
    assert!(
        e.search(OTHER, "值班表").await.is_empty(),
        "private by default"
    );

    // Same attachment again: the existing document.
    let (st, again) = e
        .me(
            "POST",
            "/kb/files",
            Some(json!({"file_id": "fil_0000000000000001", "title": "别的名字"})),
        )
        .await;
    assert_eq!((st, again["id"].as_str()), (200, Some(id.as_str())));

    // Not text: kept, findable by title only.
    let (st, img) = e
        .me(
            "POST",
            "/kb/files",
            Some(json!({"file_id": "fil_0000000000000002", "title": "架构图"})),
        )
        .await;
    assert_eq!(st, 200, "{img}");
    assert_eq!(img["status"], "skipped");
    assert_eq!(img["error"], "这种文件暂不支持全文检索");
    assert_eq!(e.search(ME, "架构图").await.len(), 1);

    // Another workspace's attachment, an unknown one.
    for f in ["fil_0000000000000003", "fil_nope"] {
        let (st, v) = e.me("POST", "/kb/files", Some(json!({"file_id": f}))).await;
        assert_eq!((st, refusal(&v).as_str()), (404, "没有这个附件"));
    }

    // Removed from the knowledge base; the attachment stays.
    let (st, _) = e.me("DELETE", &format!("/kb/docs/{id}"), None).await;
    assert_eq!(st, 200);
    assert!(e.search(ME, "值班表").await.is_empty());
    let left: i64 = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM files WHERE id = 'fil_0000000000000001'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(left, 1);
}

#[tokio::test]
async fn source_writes() {
    let e = setup();
    write(&e.wiki().join("a.md"), "# 甲\n\n部署脚本。");
    write(&e.wiki().join("草稿/b.md"), "# 乙\n\n草稿里的部署想法。");
    let s = e
        .add_folder(&e.wiki(), json!({"visibility": "private"}))
        .await;
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["builtin"], false);
    assert_eq!(
        s["include"],
        json!(["**/*.md", "**/*.markdown", "**/*.txt"])
    );
    assert_eq!(s["revision"], 1);
    assert!(s["last_sync"].as_i64().is_some());
    assert_eq!(e.search(ME, "部署").await.len(), 2);
    assert!(e.search(OTHER, "部署").await.is_empty());

    // Scope change reaches the index; a revision is written.
    let (st, v) = e
        .me(
            "PATCH",
            &format!("/kb/sources/{id}"),
            Some(json!({"name": "团队 Wiki", "visibility": "workspace", "level": "reasoning"})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        (v["name"].as_str(), v["revision"].as_i64()),
        (Some("团队 Wiki"), Some(2))
    );
    let hits = e.search(OTHER, "部署").await;
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|h| h["level"] == "reasoning"));
    let revs: i64 = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM revisions WHERE object_type = 'kb_source' AND object_id = ?",
                [&id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(revs, 1);

    // Excluding a directory rescans: its file is deleted from the index.
    let (st, v) = e
        .me(
            "PATCH",
            &format!("/kb/sources/{id}"),
            Some(json!({"exclude": ["草稿/**"]})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    e.wait(&id).await;
    assert_eq!(e.search(ME, "部署").await.len(), 1);
    assert_eq!(e.doc_by_ref(&id, "草稿/b.md").status, "deleted");

    // Paused: not searchable, not syncable.
    e.me(
        "PATCH",
        &format!("/kb/sources/{id}"),
        Some(json!({"paused": true})),
    )
    .await;
    assert!(e.search(ME, "部署").await.is_empty());
    let (st, v) = e.me("POST", &format!("/kb/sources/{id}/sync"), None).await;
    assert_eq!(
        (st, refusal(&v).as_str()),
        (400, "这个来源已暂停，恢复后才能同步")
    );
    e.me(
        "PATCH",
        &format!("/kb/sources/{id}"),
        Some(json!({"paused": false})),
    )
    .await;
    e.wait(&id).await;

    // Manual sync: 202, and 409 while one runs.
    let (st, v) = e.me("POST", &format!("/kb/sources/{id}/sync"), None).await;
    assert_eq!(st, 202, "{v}");
    assert!(v["job_id"].as_str().unwrap().starts_with("job_"));
    e.wait(&id).await;
    let claimed = folder::claim(&e.env.modules, &id).unwrap();
    assert!(claimed.is_some());
    let (st, v) = e.me("POST", &format!("/kb/sources/{id}/sync"), None).await;
    assert_eq!((st, refusal(&v).as_str()), (409, "正在同步"));
    folder::sync(&e.env.modules, &e.kb, claimed.unwrap())
        .await
        .unwrap();

    // include / exclude only on folders; built-ins cannot be deleted.
    let (_, list) = e.me("GET", "/kb/sources", None).await;
    let notes = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "notes")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, v) = e
        .me(
            "PATCH",
            &format!("/kb/sources/{notes}"),
            Some(json!({"include": ["*.md"]})),
        )
        .await;
    assert_eq!((st, refusal(&v).as_str()), (400, "参数不对：include"));
    let (st, v) = e.me("DELETE", &format!("/kb/sources/{notes}"), None).await;
    assert_eq!(
        (st, refusal(&v).as_str()),
        (400, "内置来源不能删除，可以暂停")
    );

    // Another workspace cannot touch it.
    let (st, _) = call(
        &e.app,
        Some(&caller(WS2, ME)),
        "DELETE",
        &format!("/kb/sources/{id}"),
        None,
    )
    .await;
    assert_eq!(st, 404);

    // Deleted: documents and index rows go, the files stay.
    let (st, v) = e.me("DELETE", &format!("/kb/sources/{id}"), None).await;
    assert_eq!((st, v), (200, json!({"ok": true})));
    assert!(e.search(ME, "部署").await.is_empty());
    let left: i64 = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM kb_docs WHERE source_id = ?",
                [&id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(left, 0);
    assert!(e.wiki().join("a.md").exists());
    let (st, _) = e
        .me(
            "PATCH",
            &format!("/kb/sources/{id}"),
            Some(json!({"name": "x"})),
        )
        .await;
    assert_eq!(st, 404);
}

#[tokio::test]
async fn private_folder_sources_are_hidden_from_others() {
    let e = setup();
    write(&e.wiki().join("a.md"), "私人笔记");
    let s = e.add_folder(&e.wiki(), json!({})).await;
    let id = s["id"].as_str().unwrap();
    for (m, b) in [
        ("PATCH", Some(json!({"visibility": "workspace"}))),
        ("DELETE", None),
    ] {
        let (st, v) = e.req(OTHER, m, &format!("/kb/sources/{id}"), b).await;
        assert_eq!((st, refusal(&v).as_str()), (404, "没有这个知识来源"));
    }
    let (st, _) = e
        .req(OTHER, "POST", &format!("/kb/sources/{id}/sync"), None)
        .await;
    assert_eq!(st, 404);
}

#[tokio::test]
async fn only_the_owner_rescopes_builtin_sources() {
    let e = setup();
    let (st, list) = e.req(OTHER, "GET", "/kb/sources", None).await;
    assert_eq!(st, 200, "{list}");
    fn list_of(v: &Value) -> Vec<Value> {
        v.as_array()
            .or_else(|| v["sources"].as_array())
            .cloned()
            .unwrap_or_default()
    }
    let id_of = |kind: &str| {
        list_of(&list)
            .into_iter()
            .find(|s| s["kind"] == kind)
            .and_then(|s| s["id"].as_str().map(str::to_string))
            .unwrap_or_else(|| panic!("no {kind} source: {list}"))
    };
    for kind in ["notes", "files", "items", "chat", "mems", "bot_mems"] {
        let id = id_of(kind);
        let path = format!("/kb/sources/{id}");
        let cur = list_of(&list)
            .into_iter()
            .find(|s| s["id"] == id.as_str())
            .unwrap();
        let vis = if cur["visibility"] == "private" {
            "workspace"
        } else {
            "private"
        };
        for b in [json!({"visibility": vis}), json!({"level": "public"})] {
            let (st, v) = e.req(OTHER, "PATCH", &path, Some(b.clone())).await;
            assert_eq!(
                (st, refusal(&v).as_str()),
                (403, "只有工作空间的所有者能改内置来源"),
                "{kind} {b}"
            );
        }
        // other changes, and the same values again, stay open to members
        let (st, v) = e
            .req(OTHER, "PATCH", &path, Some(json!({"paused": true})))
            .await;
        assert_eq!(st, 200, "{kind}: {v}");
        let same = json!({"level": v["level"].clone(), "paused": false});
        assert_eq!(e.req(OTHER, "PATCH", &path, Some(same)).await.0, 200);
        let (st, v) = e
            .me("PATCH", &path, Some(json!({"level": "reasoning"})))
            .await;
        assert_eq!((st, v["level"].as_str()), (200, Some("reasoning")), "{v}");
    }
}

/// Is `dir` really unreadable after `chmod 000` (not when the tests run as root)?
fn lock(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o000)).unwrap();
    fs::read_dir(dir).is_err()
}

fn unlock(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test]
async fn an_unreadable_subfolder_deletes_nothing() {
    let e = setup();
    write(&e.wiki().join("a.md"), "# 甲\n\n第一页。");
    write(&e.wiki().join("gone.md"), "# 乙\n\n马上要删掉的一页。");
    write(&e.wiki().join("私密/c.md"), "# 丙\n\n锁起来的一页。");
    let s = e.add_folder(&e.wiki(), json!({})).await;
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(
        s["stats"],
        json!({"docs": 3, "chunks": 3, "skipped": 0}),
        "{s}"
    );

    // A file really removed while a subfolder cannot be read: nothing is deleted.
    fs::remove_file(e.wiki().join("gone.md")).unwrap();
    let locked = e.wiki().join("私密");
    if !lock(&locked) {
        unlock(&locked);
        return; // running as root
    }
    let r = e.sync(&id).await;
    unlock(&locked);
    assert_eq!((r.files, r.deleted, r.kept), (1, 0, 2), "{r:?}");
    assert_eq!(r.incomplete, ["unreadable"]);
    for rel in ["a.md", "gone.md", "私密/c.md"] {
        assert_eq!(e.doc_by_ref(&id, rel).status, "active", "{rel}");
    }
    assert_eq!(e.search(ME, "锁起来").await.len(), 1);
    let s = e.wait(&id).await;
    assert_eq!(s["status"], "ok", "{s}");
    assert_eq!(s["stats"]["kept"], 2, "{s}");
    assert_eq!(s["stats"]["incomplete"], json!(["unreadable"]), "{s}");

    // Readable again: the complete walk deletes what is really gone, and the note goes away.
    let r = e.sync(&id).await;
    assert_eq!((r.files, r.deleted, r.kept), (2, 1, 0), "{r:?}");
    assert!(r.incomplete.is_empty());
    assert_eq!(e.doc_by_ref(&id, "gone.md").status, "deleted");
    assert_eq!(e.doc_by_ref(&id, "私密/c.md").status, "active");
    let s = e.wait(&id).await;
    assert_eq!(
        s["stats"],
        json!({"docs": 2, "chunks": 2, "skipped": 0}),
        "{s}"
    );
}

#[tokio::test]
async fn over_the_file_limit_deletes_nothing() {
    let e = setup();
    write(&e.wiki().join("old/a.md"), "# 旧\n\n很早以前的一页。");
    let s = e.add_folder(&e.wiki(), json!({})).await;
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(e.doc_by_ref(&id, "old/a.md").status, "active");

    // The old page is gone, and the folder now holds one file more than a source may have.
    fs::remove_dir_all(e.wiki().join("old")).unwrap();
    let many = e.wiki().join("many");
    fs::create_dir_all(&many).unwrap();
    for i in 0..=steve_knowledge::scan::MAX_FILES {
        fs::write(many.join(format!("{i:05}.txt")), "").unwrap();
    }
    let r = e.sync(&id).await;
    assert_eq!(
        (r.files, r.skipped, r.deleted, r.kept),
        (steve_knowledge::scan::MAX_FILES, 1, 0, 1),
        "{r:?}"
    );
    assert_eq!(r.incomplete, ["over_limit"]);
    assert_eq!(e.doc_by_ref(&id, "old/a.md").status, "active");
}

#[tokio::test]
async fn a_file_turned_into_an_outside_link_is_kept_and_counted() {
    let e = setup();
    let outside = e.home.join("outside");
    write(&outside.join("secret.md"), "外面的秘密：工资表");
    write(&e.wiki().join("a.md"), "# 甲\n\n部署脚本在 ops 仓库。");
    write(&e.wiki().join("b.md"), "# 乙\n\n会被删掉的一页。");
    let s = e.add_folder(&e.wiki(), json!({})).await;
    let id = s["id"].as_str().unwrap().to_string();

    fs::remove_file(e.wiki().join("a.md")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret.md"), e.wiki().join("a.md")).unwrap();
    fs::remove_file(e.wiki().join("b.md")).unwrap();
    let r = e.sync(&id).await;
    // The walk itself was complete: the really vanished file is deleted, the link is not read.
    assert_eq!((r.links_refused, r.kept, r.deleted), (1, 1, 1), "{r:?}");
    assert!(r.incomplete.is_empty());
    assert_eq!(e.doc_by_ref(&id, "a.md").status, "active");
    assert_eq!(e.doc_by_ref(&id, "b.md").status, "deleted");
    assert!(e.search(ME, "工资表").await.is_empty());
    let s = e.wait(&id).await;
    assert_eq!(s["stats"]["kept"], 1, "{s}");
}
