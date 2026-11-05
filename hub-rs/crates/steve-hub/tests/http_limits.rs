//! Request bodies on the wired Hub: a sane limit for JSON bodies, large multipart uploads still allowed,
//! a malformed body answered as the client's error, attachments streamed back whole.
//! Temp STEVE_HOME, adapters `/usr/bin/false`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    _home: tempfile::TempDir,
    base: String,
    http: reqwest::Client,
    cookie: String,
}

impl Env {
    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}{path}", self.base))
            .header("cookie", &self.cookie)
    }
}

async fn start() -> Env {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().expect("tmp");
    let h = home.path().to_path_buf();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    assert_ne!(port, 8440);
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
    let vars = Arc::new(vars);
    let get = {
        let vars = vars.clone();
        move |k: &str| vars.get(k).cloned()
    };
    let cfg = Arc::new(Config::from_vars(&get).expect("config"));
    for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(d).expect("dirs");
    }
    let bus = Bus::new();
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus).with_modules(w.modules);
    steve_hub::app::boot(&st).await.expect("boot");
    for job in w.jobs {
        tokio::spawn(job);
    }
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
        .expect("client");
    let r = http
        .get(format!("{base}/auth/dev"))
        .send()
        .await
        .expect("dev login");
    let cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_session="))
        .map(|c| c.split(';').next().unwrap_or_default().to_string())
        .expect("session cookie");
    Env {
        _home: home,
        base,
        http,
        cookie,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bodies_are_limited_except_uploads() {
    let e = start().await;

    // malformed JSON is the client's mistake
    let r = e
        .post("/api/workspaces")
        .header("content-type", "application/json")
        .body("{nope")
        .send()
        .await
        .expect("send");
    assert_eq!(r.status().as_u16(), 400);
    let v: serde_json::Value = r.json().await.expect("json");
    assert!(
        v["error"]
            .as_str()
            .is_some_and(|m| m.contains("not valid JSON")),
        "{v}"
    );

    // a JSON body beyond the API's limit is refused, not buffered
    let r = e
        .post("/api/workspaces")
        .header("content-type", "application/json")
        .body(vec![b' '; 17 * 1024 * 1024])
        .send()
        .await
        .expect("send");
    assert_eq!(r.status().as_u16(), 413);

    // a multipart upload larger than that still goes through /files, and comes back whole
    let data: Vec<u8> = (0..17 * 1024 * 1024)
        .map(|i| {
            if i % 64 == 63 {
                b'\n'
            } else {
                b'a' + (i % 26) as u8
            }
        })
        .collect();
    let mut form = b"--XB\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.log\"\r\nContent-Type: text/plain\r\n\r\n".to_vec();
    form.extend_from_slice(&data);
    form.extend_from_slice(b"\r\n--XB--\r\n");
    let r = e
        .post("/api/files")
        .header("content-type", "multipart/form-data; boundary=XB")
        .body(form)
        .send()
        .await
        .expect("send");
    assert_eq!(r.status().as_u16(), 200);
    let v: serde_json::Value = r.json().await.expect("json");
    let id = v["id"].as_str().expect("id").to_string();
    let r = e
        .http
        .get(format!("{}/api/files/{id}", e.base))
        .header("cookie", &e.cookie)
        .send()
        .await
        .expect("send");
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(
        r.headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok()),
        Some(data.len().to_string().as_str())
    );
    let back = r.bytes().await.expect("bytes");
    assert!(
        back[..] == data[..],
        "the attachment comes back as uploaded"
    );

    // a malformed form is a 400, not a 500
    let r = e
        .post("/api/files")
        .header("content-type", "multipart/form-data")
        .body("x")
        .send()
        .await
        .expect("send");
    assert_eq!(r.status().as_u16(), 400);
}
