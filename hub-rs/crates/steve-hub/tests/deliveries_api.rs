//! Isolated, fully wired Hub HTTP/MCP with real temporary storage and local Node on/off.
//! No model, installed Hub, credentials or external services are used.
use serde_json::{Value, json};
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc};
use steve_hub::{api::AppState, bus::Bus, config::Config, deliveries::Service};
use steve_store::{Store, StoreError};

struct Env {
    _home: tempfile::TempDir,
    base: String,
    store: Store,
    svc: Service,
    session: steve_store::Session,
    cookie: String,
    http: reqwest::Client,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Env {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Env {
    fn request(&self, method: &str, path: &str, login: bool) -> reqwest::RequestBuilder {
        let mut r = self.http.request(
            method.parse::<reqwest::Method>().unwrap(),
            format!("{}{path}", self.base),
        );
        if login {
            r = r
                .header("cookie", &self.cookie)
                .header("x-steve-ws", "default");
        }
        r
    }
    async fn tool(&self, name: &str, args: Value) -> Value {
        let r=self.request("POST","/mcp",false).header("authorization","Bearer delivery-bot")
            .header("accept","application/json, text/event-stream").header("mcp-protocol-version","2025-06-18")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}})).send().await.unwrap();
        assert_eq!(r.status(), 200);
        r.json().await.unwrap()
    }
}

async fn start(node: bool) -> Env {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().unwrap();
    let h = home.path().canonicalize().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let mut vars: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("HOME", h.display().to_string()),
        ("STEVE_HOME", h.join("steve").display().to_string()),
        ("STEVE_PORT", port.to_string()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PUBLIC_URL", base.clone()),
        ("STEVE_DEV_LOGIN", "1".into()),
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        ("STEVE_NO_RESUME", "1".into()),
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
    let get = move |k: &str| vars.get(k).cloned();
    let cfg = Arc::new(Config::from_vars(&get).unwrap());
    for p in [&cfg.home, &cfg.data_dir, &cfg.scratch, &cfg.worktrees] {
        std::fs::create_dir_all(p).unwrap();
    }
    let bus = Bus::new();
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get)).unwrap();
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_modules(w.modules)
        .with_workspaces(Some(w.workspaces.clone()));
    let svc = st.deliveries.clone().unwrap();
    let store = svc.store.clone();
    steve_hub::app::boot(&st).await.unwrap();
    let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
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
        .find(|s| s.starts_with("steve_session="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    store.transaction(|t| {
        t.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES('ST-1','default',1,'delivery','run','work',1,1)",[])?;
        t.execute("INSERT INTO workspaces(id,name,created) VALUES('foreign','Foreign',1)",[])?;
        t.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES('OTHER-1','foreign',1,'other','run','work',1,1)",[])?;
        Ok::<_,StoreError>(())
    }).unwrap();
    let cwd = cfg.scratch.join("ST-1");
    std::fs::create_dir_all(&cwd).unwrap();
    let session = store
        .sessions()
        .create(&steve_store::NewSession {
            workspace_id: "default".into(),
            item_id: Some("ST-1".into()),
            cwd: cwd.display().to_string(),
            harness: "test".into(),
            title: "delivery".into(),
            token: "delivery-bot".into(),
            ..Default::default()
        })
        .unwrap();
    Env {
        _home: home,
        base,
        store,
        svc,
        session,
        cookie,
        http,
        server,
    }
}

#[tokio::test]
async fn mcp_receives_files_on_local_node_and_local_off_and_retains_every_version() {
    for node in [false, true] {
        let e = start(node).await;
        let path = PathBuf::from(&e.session.cwd).join("report.md");
        std::fs::write(&path, b"first version").unwrap();
        let result = e
            .tool("steve_deliver_file", json!({"path":path,"title":"Report"}))
            .await;
        assert_ne!(result["result"]["isError"], true, "{result}");
        let meta: Value =
            serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        let id = meta["id"].as_str().unwrap();
        assert_eq!(meta["url"], format!("{}/deliveries/{id}", e.base));
        assert_eq!(meta["size"], 13);
        assert_eq!(meta["item_id"], "ST-1");
        std::fs::write(&path, b"second").unwrap();
        let second = e
            .tool("steve_deliver_file", json!({"path":"report.md"}))
            .await;
        assert_ne!(second["result"]["isError"], true, "{second}");
        let delivery=e.tool("steve_deliver",json!({"summary":"Files received","outputs":[{"kind":"file","title":"Report","location":meta["url"]}],"checks":[{"name":"test","passed":true}]})).await;
        assert_ne!(delivery["result"]["isError"], true, "{delivery}");
        assert!(
            e.store
                .artifacts()
                .list("ST-1")
                .unwrap()
                .iter()
                .any(|a| a.location.as_deref() == meta["url"].as_str()),
            "the item delivery/card references the exact received version"
        );
        std::fs::remove_dir_all(&e.session.cwd).unwrap();
        let response = e
            .request("GET", &format!("/api/deliveries/{id}/raw"), true)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"first version");
        assert_eq!(
            e.request("GET", &format!("/api/deliveries/{id}"), false)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            e.request("GET", &format!("/api/deliveries/{id}/raw"), false)
                .header("authorization", "Bearer delivery-bot")
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            e.request("POST", &format!("/api/deliveries/{id}/shares"), false)
                .header("authorization", "Bearer delivery-bot")
                .json(&json!({"expires_at":steve_store::now()+60000}))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            e.request("GET", &format!("/api/deliveries/{id}"), false)
                .header("cookie", &e.cookie)
                .header("x-steve-ws", "foreign")
                .send()
                .await
                .unwrap()
                .status(),
            409
        );
        let other = e
            .store
            .sessions()
            .create(&steve_store::NewSession {
                workspace_id: "foreign".into(),
                item_id: Some("OTHER-1".into()),
                cwd: e.session.cwd.clone(),
                harness: "test".into(),
                title: "foreign".into(),
                token: "other-bot".into(),
                ..Default::default()
            })
            .unwrap();
        let foreign = e
            .svc
            .accept(&other, "foreign", "secret.md", b"secret", None)
            .unwrap();
        assert_eq!(
            e.request("GET", &format!("/api/deliveries/{}", foreign.id), true)
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        // Opening a stable link from another selected workspace still checks membership in
        // the delivery workspace; explicit selectors never silently change workspace.
        let owner = e.store.access().owner().unwrap().unwrap();
        e.store.transaction(|t|{t.execute("INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('foreign',?,'owner',1)",[owner.id])?;Ok::<_,StoreError>(())}).unwrap();
        let cookies = format!("{}; steve_ws=foreign", e.cookie);
        for tail in ["", "/raw", "/content/report.md"] {
            assert_eq!(
                e.request("GET", &format!("/api/deliveries/{id}{tail}"), false)
                    .header("cookie", &cookies)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                200
            );
        }
        assert_eq!(
            e.request("GET", &format!("/api/deliveries/{id}?ws=foreign"), false)
                .header("cookie", &cookies)
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        assert_eq!(
            e.request("GET", &format!("/api/deliveries/{id}/raw"), true)
                .header("range", "bytes=1-3")
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            b"irs"
        );
    }
}

fn html_zip() -> Vec<u8> {
    use std::io::Write;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in [
        (
            "site/index.html",
            "<link rel=stylesheet href=assets/site.css><a href=next.html>next</a><script>fetch('/api/me')</script>",
        ),
        ("site/assets/site.css", "body{color:red}"),
        ("site/next.html", "next"),
    ] {
        w.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(data.as_bytes()).unwrap();
    }
    w.finish().unwrap().into_inner()
}

#[tokio::test]
async fn share_authorization_is_independent_expires_and_revokes_every_resource() {
    let e = start(false).await;
    let d = e
        .svc
        .accept(
            &e.session,
            "Design",
            "design.zip",
            &html_zip(),
            Some("site/index.html"),
        )
        .unwrap();
    let path = format!("/api/deliveries/{}/shares", d.id);
    assert_eq!(
        e.request("POST", &path, false)
            .json(&json!({"expires_at":steve_store::now()+60000}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        e.request("POST", &path, true)
            .header("origin", "null")
            .json(&json!({"expires_at":steve_store::now()+60000}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        e.request("POST", &path, true)
            .json(&json!({"expires_at":steve_store::now()-1}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let expires = steve_store::now() + 60000;
    let r = e
        .request("POST", &path, true)
        .json(&json!({"expires_at":expires}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let share: Value = r.json().await.unwrap();
    let token = share["token"].as_str().unwrap();
    assert_eq!(share["expires_at"], expires);
    assert_eq!(share["url"], format!("{}/shares/{token}", e.base));
    let list: Value = e
        .request("GET", &path, true)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    let anon = format!("/api/delivery-shares/{token}");
    let meta: Value = e
        .request("GET", &anon, false)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(meta["entrypoint"], "site/index.html");
    assert!(meta["download_url"].as_str().unwrap().contains(&anon));
    for (prefix, login) in [
        (format!("/api/deliveries/{}", d.id), true),
        (anon.clone(), false),
    ] {
        let r = e
            .request("GET", &format!("{prefix}/content/site/index.html"), login)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let csp = r.headers()["content-security-policy"].to_str().unwrap();
        assert!(
            csp.contains("sandbox allow-same-origin")
                && csp.contains("script-src 'none'")
                && csp.contains("connect-src 'none'")
        );
        assert_eq!(r.headers()["referrer-policy"], "no-referrer");
        assert_eq!(r.headers()["cache-control"], "no-store");
        let css = e
            .request(
                "GET",
                &format!("{prefix}/content/site/assets/site.css"),
                login,
            )
            .send()
            .await
            .unwrap();
        assert_eq!(css.headers()["content-type"], "text/css");
        assert_eq!(css.text().await.unwrap(), "body{color:red}");
        assert_eq!(
            e.request("GET", &format!("{prefix}/content/site/next.html"), login)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "next"
        );
        assert_eq!(
            e.request(
                "GET",
                &format!("{prefix}/content/site/%252e%252e/raw"),
                login
            )
            .send()
            .await
            .unwrap()
            .status(),
            404
        );
    }
    assert_eq!(
        e.request("DELETE", &format!("{path}/{token}"), true)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    for tail in [
        "",
        "/raw",
        "/content/site/index.html",
        "/content/site/assets/site.css",
    ] {
        assert_eq!(
            e.request("GET", &format!("{anon}{tail}"), false)
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    assert_eq!(
        e.request("GET", &format!("/api/deliveries/{}", d.id), true)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let short = e
        .svc
        .create_share(
            &d,
            &e.store.access().owner().unwrap().unwrap().id,
            steve_store::now() + 60000,
        )
        .unwrap();
    // Clock-independent boundary: equality is expired. This mutates only this isolated fixture.
    e.store
        .transaction(|t| {
            t.execute(
                "UPDATE delivery_shares SET expires_at=? WHERE token=?",
                rusqlite::params![steve_store::now(), short.token],
            )?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
    for tail in ["", "/raw", "/content/site/next.html"] {
        assert_eq!(
            e.request(
                "GET",
                &format!("/api/delivery-shares/{}{tail}", short.token),
                false
            )
            .send()
            .await
            .unwrap()
            .status(),
            404
        );
    }
}
