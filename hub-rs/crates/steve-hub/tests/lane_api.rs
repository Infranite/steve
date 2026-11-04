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
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus).with_modules(w.modules);
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
