//! `/api/items/:id/artifacts/:art/preview` and `/raw` on the wired Hub, with the local Node off
//! and on (`STEVE_NODE_MODE=local`, N0e-E5b): the same files give the same JSON, the same bytes
//! and the same Range. Temp STEVE_HOME, no agent, no real host.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

fn g(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Env {
    _home: tempfile::TempDir,
    home: std::path::PathBuf,
    db: std::path::PathBuf,
    /// Where files are: its local roots are built again when asked (`follow` is not run here).
    workspaces: Arc<steve_hub::wire::workspaces::Workspaces>,
    base: String,
    http: reqwest::Client,
    cookie: String,
}

impl Env {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut req = self
            .http
            .request(m, format!("{}{path}", self.base))
            .header("cookie", &self.cookie);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.unwrap();
        let status = r.status().as_u16();
        let text = r.text().await.unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    /// Status, the headers that matter and the body of a raw read.
    async fn raw(&self, path: &str, range: Option<&str>) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let mut req = self
            .http
            .get(format!("{}{path}", self.base))
            .header("cookie", &self.cookie);
        if let Some(r) = range {
            req = req.header("range", r);
        }
        let r = req.send().await.unwrap();
        let status = r.status().as_u16();
        let headers = [
            "content-type",
            "content-length",
            "content-range",
            "content-disposition",
            "accept-ranges",
            "content-security-policy",
        ]
        .iter()
        .filter_map(|h| {
            r.headers()
                .get(*h)
                .map(|v| (h.to_string(), v.to_str().unwrap().to_string()))
        })
        .collect();
        (status, headers, r.bytes().await.unwrap().to_vec())
    }
}

async fn start() -> Env {
    start_with(false).await
}

/// `node`: with the local Node on (`STEVE_NODE_MODE=local`, so not a `STEVE_NO_RESUME` copy).
async fn start_with(node: bool) -> Env {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().unwrap();
    let h = home.path().to_path_buf();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut vars: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("HOME", h.display().to_string()),
        ("STEVE_HOME", h.join("steve").display().to_string()),
        ("STEVE_PORT", port.to_string()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PUBLIC_URL", format!("http://127.0.0.1:{port}")),
        ("STEVE_DEV_LOGIN", "1".into()),
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        ("STEVE_NO_RESUME", "1".into()),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
        ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
    ] {
        vars.insert(k.into(), v);
    }
    if node {
        vars.remove("STEVE_NO_RESUME");
        vars.insert("STEVE_NODE_MODE".into(), "local".into());
    }
    let vars = Arc::new(vars);
    let get = {
        let vars = vars.clone();
        move |k: &str| vars.get(k).cloned()
    };
    let cfg = Arc::new(Config::from_vars(&get).unwrap());
    for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(d).unwrap();
    }
    let bus = Bus::new();
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).unwrap();
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_modules(w.modules)
        .with_workspaces(Some(w.workspaces.clone()));
    steve_hub::app::boot(&st).await.unwrap();
    let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = http.get(format!("{base}/auth/dev")).send().await.unwrap();
    let cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_session="))
        .map(|c| c.split(';').next().unwrap_or_default().to_string())
        .unwrap();
    Env {
        _home: home,
        home: h,
        db: cfg.data_dir.join("steve.sqlite3"),
        workspaces: w.workspaces,
        base,
        http,
        cookie,
    }
}

/// The files both Hubs show, outside either Hub's home: a git project with documents, media, a
/// hidden file, a link out of it, and a branch with a document main does not have.
struct Fixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    repo: std::path::PathBuf,
    hash: String,
    tmp: std::path::PathBuf,
}

fn fixture() -> Fixture {
    // not under the temp directory, which the Hub reads anywhere in
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    g(&repo, &["init", "-q", "-b", "main"]);
    g(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    g(&repo, &["add", "."]);
    g(&repo, &["commit", "-q", "-m", "a"]);
    g(&repo, &["checkout", "-q", "-b", "steve/st-1"]);
    std::fs::create_dir_all(repo.join("plan")).unwrap();
    std::fs::write(repo.join("plan/方案.md"), "# 方案\n").unwrap();
    g(&repo, &["add", "."]);
    g(&repo, &["commit", "-q", "-m", "加方案"]);
    let hash = g(&repo, &["rev-parse", "--short", "HEAD"]);
    g(&repo, &["checkout", "-q", "main"]);
    let w = |rel: &str, bytes: &[u8]| {
        let p = repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    };
    w("docs/说明.md", "# 标题\n\n正文".as_bytes());
    w("docs/shots/a.png", &[0x89, b'P', b'N', b'G', 0, 0]);
    w("docs/shots/b.bin", &[1, 0, 2]);
    w("docs/shots/.hidden", b"x");
    w("docs/报告.docx", &[b'P', b'K', 3, 4, 0]);
    w("media/clip.mp4", &(0u8..100).collect::<Vec<_>>());
    std::fs::create_dir_all(root.join("secret")).unwrap();
    std::fs::write(root.join("secret/key"), "s").unwrap();
    std::os::unix::fs::symlink(root.join("secret"), repo.join("link")).unwrap();
    let tmp = std::env::temp_dir().join(format!(
        "steve-e5b-{}.txt",
        root.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&tmp, "临时").unwrap();
    Fixture {
        _dir: dir,
        root,
        repo,
        hash,
        tmp,
    }
}

/// The project at the fixture's repository with item `i1` (branch `steve/st-1`) and its outputs.
async fn outputs(e: &Env, f: &Fixture) {
    let (s, p) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "Repo", "path": f.repo.to_string_lossy()})),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    let project = p["id"].as_str().unwrap().to_string();
    let c = rusqlite::Connection::open(&e.db).unwrap();
    c.execute(
        "INSERT INTO items(id,workspace_id,no,title,status,stage,branch,project_id,created,updated)
         VALUES('i1','default',1,'x','doing','work','steve/st-1',?,1,1)",
        [&project],
    )
    .unwrap();
    let abs = |p: &Path| p.to_string_lossy().into_owned();
    for (id, kind, title, loc) in [
        (
            "doc",
            "文档",
            "说明".to_string(),
            "docs/说明.md".to_string(),
        ),
        ("dir", "文件", "截图".into(), "docs/shots".into()),
        (
            "abs",
            "文件",
            "说明".into(),
            abs(&f.repo.join("docs/说明.md")),
        ),
        (
            "out",
            "文件",
            "钥匙".into(),
            abs(&f.root.join("secret/key")),
        ),
        ("link", "文件", "钥匙".into(), "link/key".into()),
        ("docx", "文档", "报告".into(), "docs/报告.docx".into()),
        ("plan", "文档", "方案".into(), "plan/方案.md".into()),
        ("gone", "文档", "没了".into(), "nope.md".into()),
        ("branch", "分支", "steve/st-1".into(), String::new()),
        (
            "commit",
            "提交",
            format!("{} 加方案", f.hash),
            String::new(),
        ),
        ("clip", "文件", "视频".into(), "media/clip.mp4".into()),
        ("tmp", "文件", "临时".into(), abs(&f.tmp)),
    ] {
        c.execute(
            "INSERT INTO artifacts(id,item_id,kind,title,location,created) VALUES(?,?,?,?,NULLIF(?,''),1)",
            [id, "i1", kind, &title, &loc],
        )
        .unwrap();
    }
}

const PREVIEWS: [&str; 15] = [
    "doc",
    "dir",
    "dir?path=a.png",
    "dir?path=b.bin",
    "dir?path=.hidden",
    "dir?path=../说明.md",
    "abs",
    "out",
    "link",
    "docx",
    "plan",
    "gone",
    "branch",
    "commit",
    "tmp",
];

/// Every preview, every raw read (whole, a span, the end) and the refusals, as one Hub answers.
async fn answers(e: &Env) -> Vec<Value> {
    let mut all = vec![];
    for p in PREVIEWS {
        let (art, q) = p.split_once('?').map_or((p, ""), |(a, q)| (a, q));
        let path = format!("/api/items/i1/artifacts/{art}/preview?{q}");
        let (s, v) = e.call("GET", &path, None).await;
        all.push(json!({"preview": p, "status": s, "body": v}));
    }
    for (art, q, range) in [
        ("clip", "", None),
        ("clip", "", Some("bytes=10-19")),
        ("clip", "", Some("bytes=-5")),
        ("dir", "path=a.png", None),
        ("dir", "path=a.png&download=1", None),
        ("dir", "", None),
        ("out", "", None),
        ("link", "", None),
        ("tmp", "", None),
    ] {
        let path = format!("/api/items/i1/artifacts/{art}/raw?{q}");
        let (s, h, b) = e.raw(&path, range).await;
        all.push(json!({"raw": path, "range": range, "status": s, "headers": h, "body": b}));
    }
    all
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_local_node_shows_outputs_as_the_hub_did() {
    let f = fixture();
    let off = start().await;
    outputs(&off, &f).await;
    let on = start_with(true).await;
    outputs(&on, &f).await;
    /* the new project is a root of the local Node (as its roots follow the project event) */
    on.workspaces.refresh_roots().unwrap();
    let want = answers(&off).await;
    let got = answers(&on).await;
    assert_eq!(want.len(), got.len());
    for (w, g) in want.iter().zip(&got) {
        assert_eq!(g, w);
    }
    /* what was compared is what the Hub has always shown */
    assert_eq!(want[0]["body"]["text"], "# 标题\n\n正文");
    assert_eq!(want[1]["body"]["type"], "dir");
    assert_eq!(want[7]["body"]["type"], "missing");
    assert_eq!(want[8]["body"]["type"], "missing");
    assert_eq!(want[10]["body"]["from"], "git steve/st-1");
    assert_eq!(want[12]["body"]["type"], "branch");
    assert_eq!(want[13]["body"]["type"], "commit");
    assert_eq!(want[14]["body"]["text"], "临时");
    let span = &want[PREVIEWS.len() + 1];
    assert_eq!(span["status"], 206);
    assert_eq!(span["body"], json!((10u8..20).collect::<Vec<_>>()));
    assert_eq!(want[PREVIEWS.len() + 5]["status"], 404);
    /* the one place they differ: a link from the project into the Hub's temp directory was read
    by the Hub, the Node keeps to the project */
    std::os::unix::fs::symlink(&f.tmp, f.repo.join("tmp-link.txt")).unwrap();
    for e in [&off, &on] {
        let c = rusqlite::Connection::open(&e.db).unwrap();
        c.execute(
            "INSERT INTO artifacts(id,item_id,kind,title,location,created) VALUES('tl','i1','文件','x','tmp-link.txt',1)",
            [],
        )
        .unwrap();
    }
    let path = "/api/items/i1/artifacts/tl/preview";
    assert_eq!(off.call("GET", path, None).await.1["text"], "临时");
    assert_eq!(on.call("GET", path, None).await.1["type"], "missing");
    let _ = std::fs::remove_file(&f.tmp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_output_on_a_device_that_is_not_there_says_device_offline() {
    let f = fixture();
    let e = start_with(true).await;
    outputs(&e, &f).await;
    let c = rusqlite::Connection::open(&e.db).unwrap();
    c.execute(
        "UPDATE projects SET device_id='n_gone', path='work/repo'",
        [],
    )
    .unwrap();
    let (s, v) = e
        .call("GET", "/api/items/i1/artifacts/doc/preview", None)
        .await;
    assert_eq!((s, v), (503, json!({"error": "设备离线"})));
    let (s, _, b) = e.raw("/api/items/i1/artifacts/clip/raw", None).await;
    assert_eq!(s, 503);
    assert_eq!(
        serde_json::from_slice::<Value>(&b).unwrap()["error"],
        "设备离线"
    );
    let _ = std::fs::remove_file(&f.tmp);
    let _ = &e.home;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_recording_plays_by_range_and_only_images_and_office_say_large() {
    let f = fixture();
    let e = start().await;
    outputs(&e, &f).await;
    // sparse files over the inline limit (64 MiB): nothing is written to disk
    let big = 65 * 1024 * 1024u64;
    for rel in ["media/long.mov", "media/long.png", "media/long.docx"] {
        let file = std::fs::File::create(f.repo.join(rel)).unwrap();
        file.set_len(big).unwrap();
    }
    let c = rusqlite::Connection::open(&e.db).unwrap();
    for (id, loc) in [
        ("mov", "media/long.mov"),
        ("png", "media/long.png"),
        ("bigdoc", "media/long.docx"),
    ] {
        c.execute(
            "INSERT INTO artifacts(id,item_id,kind,title,location,created) VALUES(?,'i1','文件','x',?,1)",
            [id, loc],
        )
        .unwrap();
    }
    let large = |v: &Value| v["large"].as_bool();
    let (_, v) = e
        .call("GET", "/api/items/i1/artifacts/mov/preview", None)
        .await;
    assert_eq!(
        (v["type"].as_str(), large(&v)),
        (Some("video"), Some(false)),
        "{v}"
    );
    let (_, v) = e
        .call("GET", "/api/items/i1/artifacts/png/preview", None)
        .await;
    assert_eq!(
        (v["type"].as_str(), large(&v)),
        (Some("image"), Some(true)),
        "{v}"
    );
    let (_, v) = e
        .call("GET", "/api/items/i1/artifacts/bigdoc/preview", None)
        .await;
    assert_eq!(
        (v["type"].as_str(), large(&v)),
        (Some("office"), Some(true)),
        "{v}"
    );
    // the recording plays: a span of it, then the whole of it (no 413)
    let (s, h, b) = e
        .raw("/api/items/i1/artifacts/mov/raw", Some("bytes=0-9"))
        .await;
    assert_eq!(s, 206);
    assert_eq!(b.len(), 10);
    assert!(
        h.contains(&("content-range".into(), format!("bytes 0-9/{big}"))),
        "{h:?}"
    );
    let r = e
        .http
        .get(format!(
            "{}/api/items/i1/artifacts/mov/raw?download=1",
            e.base
        ))
        .header("cookie", &e.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(r.content_length(), Some(big));
    drop(r);
    let _ = std::fs::remove_file(&f.tmp);
}
