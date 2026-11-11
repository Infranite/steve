//! `GET /api/projects/:id/lane` on the wired Hub: the profile of the target a git project merges into,
//! fetched first, with a fake `gh`. Temp STEVE_HOME, no agent, no real host.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

fn g(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Env {
    _home: tempfile::TempDir,
    home: std::path::PathBuf,
    db: std::path::PathBuf,
    /// The remote Nodes' coordinators (`listen`).
    fleet: Option<Arc<steve_hub::nodes::NodeFleet>>,
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
}

async fn start() -> Env {
    start_with(false, false).await
}

/// `node`: with the local Node on (`STEVE_NODE_MODE=local`, so not a `STEVE_NO_RESUME` copy);
/// `listen`: remote Nodes are listened for (`STEVE_NODE_LISTEN=1`).
async fn start_with(node: bool, listen: bool) -> Env {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().unwrap();
    let h = home.path().to_path_buf();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    /* a gh that knows one repository: a ruleset asking for pull requests */
    let gh = h.join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\ncase \"$4\" in\n  repos/acme/app/rules/branches/main) printf '[{\"type\":\"pull_request\"}]' ;;\n  repos/acme/app/branches/main) printf '{\"protected\":false}' ;;\n  graphql) printf '{\"data\":{\"repository\":{\"mergeQueue\":null}}}' ;;\n  *) echo 'Not Found (HTTP 404)' >&2; exit 1 ;;\nesac\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        ("STEVE_GH", gh.display().to_string()),
    ] {
        vars.insert(k.into(), v);
    }
    if node {
        vars.remove("STEVE_NO_RESUME");
        vars.insert("STEVE_NODE_MODE".into(), "local".into());
    }
    if listen {
        // This fixture owns fresh temporary state and deliberately runs remote coordinators.
        // NO_RESUME copies never accept remote Nodes, even with an explicit listener switch.
        vars.remove("STEVE_NO_RESUME");
        vars.insert("STEVE_NODE_LISTEN".into(), "1".into());
        if !node {
            vars.insert("STEVE_NODE_MODE".into(), "off".into());
        }
    } else {
        vars.insert("STEVE_NODE_LISTEN".into(), "0".into());
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
    let fleet = w.nodes.as_ref().map(|n| n.fleet.clone());
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_modules(w.modules)
        .with_workspaces(Some(w.workspaces));
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
        fleet,
        base,
        http,
        cookie,
    }
}

#[tokio::test]
async fn a_git_project_shows_the_lane_it_merges_into() {
    let e = start().await;
    /* a repository whose origin is GitHub's acme/app, served from a bare copy here */
    let repo = e.home.join("app");
    std::fs::create_dir_all(&repo).unwrap();
    g(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
    g(&repo, &["add", "-A"]);
    g(&repo, &["commit", "-q", "-m", "base"]);
    let bare = e.home.join("origin.git");
    g(
        &e.home,
        &[
            "clone",
            "-q",
            "--bare",
            &repo.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    );
    let url = "git@github.com:Acme/App.git";
    g(
        &repo,
        &[
            "config",
            &format!("url.{}.insteadOf", bare.to_string_lossy()),
            url,
        ],
    );
    g(&repo, &["remote", "add", "origin", url]);

    let (s, p) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "App", "path": repo.to_string_lossy()})),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    let id = p["id"].as_str().unwrap().to_string();

    /* never fetched: the lane fetches first, then reads origin/main */
    let (s, v) = e
        .call("GET", &format!("/api/projects/{id}/lane"), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["target"]["upstream"], "github.com/acme/app");
    assert_eq!(v["target"]["branch"], "main");
    assert!(
        v["reads"].as_str().unwrap().starts_with("origin/main "),
        "{v}"
    );
    assert_eq!(v["fetch_error"], Value::Null);
    assert_eq!(v["merge"]["value"], "host_pr");
    assert_eq!(v["merge"]["how"]["state"], "found");
    assert_eq!(v["checks"]["value"][0]["name"], "cargo test --workspace");
    assert_eq!(v["checks"]["value"][0]["from"], "suggested");
    assert_eq!(v["checks"]["how"]["state"], "default");
    assert_eq!(v["ship"]["value"], "merge");

    /* origin gone: said, and the profile is of the branch as last fetched */
    std::fs::rename(&bare, e.home.join("moved.git")).unwrap();
    let (s, v) = e
        .call("GET", &format!("/api/projects/{id}/lane"), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(v["fetch_error"].is_string(), "{v}");
    assert_eq!(v["target"]["upstream"], "github.com/acme/app");

    /* a plain directory has no lane */
    let plain = e.home.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let (_, p) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "Plain", "path": plain.to_string_lossy()})),
        )
        .await;
    let (s, v) = e
        .call(
            "GET",
            &format!("/api/projects/{}/lane", p["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(s, 400, "{v}");
    let (s, _) = e.call("GET", "/api/projects/prj_nope/lane", None).await;
    assert_eq!(s, 404);
}

#[tokio::test]
async fn a_project_on_a_device_that_is_not_there_says_device_offline() {
    let e = start().await;
    let repo = e.home.join("far");
    std::fs::create_dir_all(&repo).unwrap();
    g(&repo, &["init", "-q", "-b", "main"]);
    let (s, p) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "Far", "path": repo.to_string_lossy()})),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    let id = p["id"].as_str().unwrap().to_string();
    /* the project lives on a paired device's Node now; nothing links it */
    let c = rusqlite::Connection::open(&e.db).unwrap();
    c.execute(
        "UPDATE projects SET device_id='n_gone', path='work/far' WHERE id=?",
        [&id],
    )
    .unwrap();
    let (s, v) = e
        .call("GET", &format!("/api/projects/{id}/lane"), None)
        .await;
    assert_eq!((s, v), (503, json!({"error": "设备离线"})));
}

/// With the local Node on, a directory under none of its roots is still taken as a new project,
/// and one under a root is too.
#[tokio::test]
async fn with_the_local_node_a_directory_outside_its_roots_becomes_a_project() {
    let e = start_with(true, false).await;
    std::fs::create_dir_all(e.home.join("private/notes")).unwrap();
    /* a missing directory is said as before */
    let gone = e.home.join("gone").to_string_lossy().to_string();
    for path in ["/api/projects", "/api/projects/describe"] {
        let (s, v) = e
            .call("POST", path, Some(json!({"name": "Gone", "path": gone})))
            .await;
        assert_eq!(s, 400, "{path}: {v}");
        assert_eq!(v["error"], format!("目录不存在：{gone}"), "{path}");
    }
    /* one typed in becomes a project */
    let notes = e.home.join("private/notes").to_string_lossy().to_string();
    let (s, v) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "Notes", "path": notes})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["kind"], "dir");
    /* under a root (here: the scratch directory) it is taken too */
    let dir = e.home.join("steve/scratch/tool");
    std::fs::create_dir_all(&dir).unwrap();
    let (s, v) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "Tool", "path": dir.to_string_lossy()})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["kind"], "dir");
}

/// The chooser of a new project browses the Hub's machine for the instance owner, whether or not a
/// root holds the directory yet; a project is made on the built-in Node only.
#[tokio::test]
async fn the_owner_browses_the_machine_for_a_new_projects_directory() {
    let e = start_with(true, false).await;
    std::fs::create_dir_all(e.home.join("private/notes/.git")).unwrap();
    std::fs::create_dir_all(e.home.join("private/photos")).unwrap();
    // as the Hub sees it (a temporary directory is often behind a link)
    let private = e.home.join("private").canonicalize().unwrap();
    let private = private.to_string_lossy().to_string();
    let url = format!("/api/fs/browse?path={private}");
    let (s, v) = e.call("GET", &url, None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!((&v["exists"], &v["denied"]), (&json!(true), &json!(false)));
    let dirs: Vec<(&str, bool)> = v["dirs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["name"].as_str().unwrap(), d["git"].as_bool().unwrap()))
        .collect();
    assert_eq!(dirs, [("notes", true), ("photos", false)]);
    assert!(
        v["dirs"][0]["path"].as_str().unwrap().ends_with("/private/notes"),
        "{v}"
    );
    let (s, v) = e.call("GET", &format!("{url}/pho"), None).await;
    assert_eq!((s, &v["exists"]), (200, &json!(false)), "{v}");
    assert!(v["parent"].as_str().unwrap().ends_with("/private"), "{v}");

    /* another Node is named, not browsed and not built on */
    let (s, v) = e.call("GET", &format!("{url}&node=nod_laptop"), None).await;
    assert_eq!(s, 400, "{v}");
    assert!(v["error"].as_str().unwrap().contains("Hub 内置的 Node"), "{v}");
    let notes = format!("{private}/notes");
    let (s, v) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "Notes", "path": notes, "node": "nod_laptop"})),
        )
        .await;
    assert_eq!(s, 400, "{v}");
    assert!(v["error"].as_str().unwrap().contains("Hub 内置的 Node"), "{v}");
    let (s, v) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "Notes", "path": notes, "node": "hub"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!((&v["kind"], &v["device_id"]), (&json!("git"), &json!("hub")));

    /* what is on the disk is the owner's to see */
    let store = steve_store::Store::open(&e.db, steve_store::NoPublisher).unwrap();
    let member = store
        .users()
        .create("local:sam", "Sam", None, "member")
        .unwrap();
    store
        .auth_sessions()
        .create(&member, "member-session", 60_000)
        .unwrap();
    let r = e
        .http
        .get(format!("{}{url}", e.base))
        .header("cookie", "steve_session=member-session")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"], "仅实例管理员可以浏览这台机器的目录");
}

struct Quiet;
#[async_trait::async_trait]
impl steve_node_wire::Handler for Quiet {
    async fn handle(&self, method: &str, _: Value) -> steve_node_api::ApiResult<Value> {
        Err(steve_node_wire::conn::unsupported(method))
    }
}

/// A project on a paired, active and linked remote Node: its lane is refused, and nothing of the
/// Hub's disk is read for it (no `git fetch` in a directory of the same name here).
#[tokio::test]
async fn a_project_on_a_linked_remote_node_is_never_read_off_the_hubs_disk() {
    let e = start_with(false, true).await;
    /* a checkout on the Hub's disk, at the very path the remote project says, never fetched */
    let repo = e.home.join("far");
    std::fs::create_dir_all(&repo).unwrap();
    g(&repo, &["init", "-q", "-b", "main"]);
    g(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let bare = e.home.join("far.git");
    g(
        &e.home,
        &[
            "clone",
            "-q",
            "--bare",
            &repo.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    );
    g(&repo, &["remote", "add", "origin", &bare.to_string_lossy()]);
    let (s, p) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "Far", "path": repo.to_string_lossy()})),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    let id = p["id"].as_str().unwrap().to_string();
    /* the project lives on n_far now, paired by the workspace's owner, active and linked */
    let c = rusqlite::Connection::open(&e.db).unwrap();
    c.execute_batch(&format!(
        "INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created)
           SELECT 'n_far',user_id,'far',zeroblob(32),'ff','active',1 FROM workspace_members
           WHERE workspace_id='default' AND role='owner' LIMIT 1;
         UPDATE projects SET device_id='n_far' WHERE id='{id}';"
    ))
    .unwrap();
    let fleet = e.fleet.as_ref().expect("remote Nodes listened for");
    let (h, n) = steve_node_wire::testkit::pair();
    let opts = |side| steve_node_wire::ConnOptions {
        side,
        ..Default::default()
    };
    let hub = steve_node_wire::Conn::spawn(h.sink, h.stream, 1, Arc::new(Quiet), opts("h"));
    let _peer = steve_node_wire::Conn::spawn(n.sink, n.stream, 1, Arc::new(Quiet), opts("n"));
    let far = steve_node_api::NodeId::new("n_far").unwrap();
    assert!(fleet.remote(&far).attach(hub, false));

    let url = format!("/api/projects/{id}/lane");
    let lane = || e.call("GET", &url, None);
    /* its path as the Node names it (`<root>/<rel>`): placed there, refused here */
    c.execute("UPDATE projects SET path='work/far' WHERE id=?", [&id])
        .unwrap();
    let (s, v) = lane().await;
    assert_eq!(
        (s, v),
        (
            400,
            json!({"error": "只能查看本机项目的合并通道，这个项目在另一台设备上"})
        )
    );
    /* an absolute path means nothing on that device: refused as well, not read here */
    c.execute(
        "UPDATE projects SET path=? WHERE id=?",
        [repo.to_string_lossy().as_ref(), id.as_str()],
    )
    .unwrap();
    let (s, v) = lane().await;
    assert_eq!((s, v), (400, json!({"error": "还没有工作区"})));
    // no fetch ran in the Hub's checkout at that path: origin/main was never written
    let fetched = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "--verify", "-q", "refs/remotes/origin/main"])
        .output()
        .unwrap();
    assert!(
        !fetched.status.success(),
        "the Hub fetched a remote project"
    );
    /* the same once its Node is gone: 设备离线 */
    c.execute_batch(&format!(
        "UPDATE projects SET path='work/far' WHERE id='{id}';
         UPDATE nodes SET state='paused' WHERE node_id='n_far';"
    ))
    .unwrap();
    let (s, v) = lane().await;
    assert_eq!((s, v), (503, json!({"error": "设备离线"})));
}

/// With the local Node on, a directory under one of its roots is read by the Node (its files face),
/// not the Hub: a link out of the root is no directory there, and its files are not shown.
#[tokio::test]
async fn with_the_local_node_a_link_out_of_a_root_is_not_read() {
    let e = start_with(true, false).await;
    let scratch = e.home.join("steve/scratch");
    let outside = e.home.join("private/secret");
    std::fs::create_dir_all(outside.join("inner")).unwrap();
    std::fs::write(outside.join("README.md"), "secret").unwrap();
    std::fs::create_dir_all(scratch.join("real")).unwrap();
    std::os::unix::fs::symlink(&outside, scratch.join("link")).unwrap();
    let link = scratch.join("link").to_string_lossy().to_string();
    /* the Hub's disk follows the link; the Node does not, so it is no directory to make a project
    of or to describe (and no model is asked) */
    for path in ["/api/projects", "/api/projects/describe"] {
        let (s, v) = e
            .call("POST", path, Some(json!({"name": "Link", "path": link})))
            .await;
        assert_eq!(s, 400, "{path}: {v}");
        assert_eq!(v["error"], format!("目录不存在：{link}"), "{path}");
    }
}

#[tokio::test]
async fn the_lane_limits_are_yours_to_set_and_now_has_nothing_to_close_on_an_idle_lane() {
    let e = start().await;
    let repo = e.home.join("solo");
    std::fs::create_dir_all(&repo).unwrap();
    g(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    g(&repo, &["add", "-A"]);
    g(&repo, &["commit", "-q", "-m", "base"]);
    let (s, p) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "Solo", "path": repo.to_string_lossy()})),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    let id = p["id"].as_str().unwrap().to_string();
    let path = format!("/api/projects/{id}/lane");
    let (s, v) = e.call("GET", &path, None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["lane"], Value::Null, "nothing went into it yet");

    let (s, v) = e
        .call(
            "PATCH",
            &path,
            Some(json!({"window_min": 15, "max_members": 4})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["lane"]["policy"]["window_ms"], 15 * 60_000);
    assert_eq!(v["lane"]["set"]["max_members"], 4);
    let (_, v) = e.call("GET", &path, None).await;
    assert_eq!(v["lane"]["policy"]["max_members"], 4, "kept");
    // back to the defaults
    let (s, v) = e
        .call(
            "PATCH",
            &path,
            Some(json!({"window_min": null, "max_members": null})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["lane"]["set"]["window_ms"], Value::Null);
    assert_eq!(
        v["lane"]["policy"]["window_ms"],
        v["lane"]["defaults"]["window_ms"]
    );
    for bad in [
        json!({"window_min": 0, "max_members": null}),
        json!({"window_min": 2000, "max_members": null}),
        json!({"window_min": null, "max_members": 0}),
        json!({"window_min": 1.5, "max_members": null}),
    ] {
        let (s, v) = e.call("PATCH", &path, Some(bad.clone())).await;
        assert_eq!(s, 400, "{bad} {v}");
    }

    let (s, v) = e.call("POST", &format!("{path}/now"), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["closed"], false);
}

/// Lanes are one per target, shared by every workspace merging into it: another workspace's
/// requests are counted, never named, and its lane's limits and "merge now" are not yours to change
/// while they are in it.
#[tokio::test]
async fn another_workspaces_requests_in_the_lane_are_counted_not_shown_or_steered() {
    let e = start().await;
    let repo = e.home.join("shared");
    std::fs::create_dir_all(&repo).unwrap();
    g(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    g(&repo, &["add", "-A"]);
    g(&repo, &["commit", "-q", "-m", "base"]);
    let (s, p) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "Shared", "path": repo.to_string_lossy()})),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    let path = format!("/api/projects/{}/lane", p["id"].as_str().unwrap());
    let (s, v) = e.call("GET", &path, None).await;
    assert_eq!(s, 200, "{v}");
    let (up, br) = (
        v["target"]["upstream"].as_str().unwrap().to_string(),
        v["target"]["branch"].as_str().unwrap().to_string(),
    );
    /* a request from another workspace whose project merges into the same target */
    let store = steve_store::Store::open(&e.db, steve_store::NoPublisher).unwrap();
    let req = |ws: &'static str, item: &'static str| steve_store::lanes::NewRequest {
        lane_id: "",
        workspace_id: Some(ws),
        item_id: Some(item),
        source: "item",
        dir: "/nonexistent",
        branch: item,
        head: "abc",
        repo: None,
    };
    steve_core::lane::store::request(&store, &up, &br, &req("ws_other", "ST-90")).unwrap();
    let (s, v) = e.call("GET", &path, None).await;
    assert_eq!(s, 200, "{v}");
    let running = &v["lane"]["running"];
    assert_eq!(running["members"], json!([]), "{v}");
    assert_eq!(running["others"], 1, "{v}");
    assert!(!v.to_string().contains("ST-90"), "{v}");
    assert!(!v.to_string().contains("ws_other"), "{v}");

    /* its limits and "merge now" are refused while the other workspace's request is in it */
    let (s, v) = e
        .call(
            "PATCH",
            &path,
            Some(json!({"window_min": 1, "max_members": 1})),
        )
        .await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(
        v["error"],
        "这个合并通道里还有其他工作区的合并，不能在这里改它的设置或立即合并"
    );
    let (s, _) = e.call("POST", &format!("{path}/now"), None).await;
    assert_eq!(s, 409);
    let row = store
        .reader(steve_store::lanes::lanes_all)
        .unwrap()
        .into_iter()
        .find(|l| l.upstream == up)
        .unwrap();
    assert_eq!((row.window_ms, row.max_members), (None, None), "unchanged");

    /* your own request in the same lane is shown */
    steve_core::lane::store::request(&store, &up, &br, &req("default", "ST-91")).unwrap();
    let (_, v) = e.call("GET", &path, None).await;
    let running = &v["lane"]["running"];
    assert_eq!(running["members"][0]["item_id"], "ST-91", "{v}");
    assert_eq!(running["members"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(running["others"], 1, "{v}");
}
