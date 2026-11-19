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

    async fn switch_workspace(&self, workspace: &str) -> String {
        let switched = self
            .request("POST", &format!("/api/workspaces/{workspace}/switch"), true)
            .header("sec-fetch-site", "same-origin")
            .send()
            .await
            .unwrap();
        assert_eq!(switched.status(), 200);
        // Use the REAL port-scoped Set-Cookie; steve_ws=foreign does not switch this Hub.
        let ws_cookie = switched
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with("steve_ws"))
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        format!("{}; {ws_cookie}", self.cookie)
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
async fn malformed_bound_workspace_never_falls_back_to_the_hub_session_directory() {
    for node in [false, true] {
        let e = start(node).await;
        let path = PathBuf::from(&e.session.cwd).join("report.md");
        std::fs::write(&path, b"Hub placeholder is not a bound workspace").unwrap();
        e.store.transaction(|tx| {
            tx.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?1,'local','{}',?1,'r','active',0)", [&e.session.id])?;
            Ok::<_, StoreError>(())
        }).unwrap();
        for input in [path.display().to_string(), "report.md".into()] {
            let result = e.tool("steve_deliver_file", json!({"path":input})).await;
            assert_eq!(result["result"]["isError"], true, "{result}");
        }
        assert!(e.store.artifacts().list("ST-1").unwrap().is_empty());
    }
}

#[tokio::test]
async fn mcp_requires_a_node_and_retains_every_received_version() {
    for node in [false, true] {
        let e = start(node).await;
        let path = PathBuf::from(&e.session.cwd).join("report.md");
        std::fs::write(&path, b"first version").unwrap();
        let result = e
            .tool("steve_deliver_file", json!({"path":path,"title":"Report"}))
            .await;
        if !node {
            assert_eq!(result["result"]["isError"], true, "{result}");
            assert!(
                result["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("delivery Node unavailable"),
                "{result}"
            );
            assert!(e.store.artifacts().list("ST-1").unwrap().is_empty());
            let count: i64 = e
                .store
                .reader::<_, StoreError>(|c| {
                    Ok(c.query_row("SELECT COUNT(*) FROM deliveries", [], |r| r.get(0))?)
                })
                .unwrap();
            assert_eq!(count, 0);
            continue;
        }
        assert_ne!(result["result"]["isError"], true, "{result}");
        let meta: Value =
            serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        let id = meta["id"].as_str().unwrap();
        assert_eq!(
            meta["url"],
            format!("{}/deliveries/{id}?ws=default", e.base)
        );
        assert_eq!(meta["size"], 13);
        assert_eq!(meta["item_id"], "ST-1");
        assert!(
            meta["access_warning"]
                .as_str()
                .unwrap()
                .contains("loopback or local-only")
        );
        std::fs::write(&path, b"second").unwrap();
        let second = e
            .tool("steve_deliver_file", json!({"path":"report.md"}))
            .await;
        assert_ne!(second["result"]["isError"], true, "{second}");
        let delivery=e.tool("steve_deliver",json!({"summary":"Files received","outputs":[{"kind":"file","title":"Report","location":meta["url"]},{"kind":"file","title":"Automatic receipt","location":"report.md"},{"kind":"link","title":"External URL","location":format!("https://outside.example/deliveries/{id}?ws=default")}],"checks":[{"name":"test","passed":true}]})).await;
        assert_ne!(delivery["result"]["isError"], true, "{delivery}");
        assert!(
            e.store
                .artifacts()
                .list("ST-1")
                .unwrap()
                .iter()
                .any(|a| a.location.as_deref() == meta["url"].as_str()
                    && a.delivery_id.as_deref() == Some(id)),
            "the item delivery/card references the exact received version"
        );
        // Item details are the actual contract consumed by both item outputs and chat cards.
        // Testing only fixture tuples with a synthetic delivery_id misses the production gap.
        let detail: Value = e
            .request("GET", "/api/items/ST-1", true)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let artifacts = detail["artifacts"].as_array().unwrap();
        assert_eq!(artifacts.len(), 3, "{detail}");
        assert!(
            artifacts
                .iter()
                .find(|a| a["title"] == "External URL")
                .unwrap()["delivery_id"]
                .is_null()
        );
        let report = artifacts.iter().find(|a| a["title"] == "Report").unwrap();
        assert_eq!(report["delivery_id"], id);
        assert_eq!(report["location"], meta["url"]);
        let automatic = artifacts
            .iter()
            .find(|a| a["title"] == "Automatic receipt")
            .unwrap();
        assert!(
            automatic["delivery_id"]
                .as_str()
                .unwrap()
                .starts_with("dlv_")
        );
        assert_ne!(automatic["delivery_id"], id);
        std::fs::remove_dir_all(&e.session.cwd).unwrap();
        let response = e
            .request("GET", &format!("/api/file-deliveries/{id}/raw"), true)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"first version");
        assert_eq!(
            e.request("GET", &format!("/api/file-deliveries/{id}"), false)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            e.request("GET", &format!("/api/file-deliveries/{id}/raw"), false)
                .header("authorization", "Bearer delivery-bot")
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            e.request("POST", &format!("/api/file-deliveries/{id}/shares"), false)
                .header("authorization", "Bearer delivery-bot")
                .json(&json!({"expires_at":steve_store::now()+60000}))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            e.request("GET", &format!("/api/file-deliveries/{id}"), false)
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
            e.request("GET", &format!("/api/file-deliveries/{}", foreign.id), true)
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
        let cookies = e.switch_workspace("foreign").await;
        for tail in ["", "/raw", "/content/report.md"] {
            assert_eq!(
                e.request("GET", &format!("/api/file-deliveries/{id}{tail}"), false)
                    .header("cookie", &cookies)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                200
            );
        }
        assert_eq!(
            e.request(
                "GET",
                &format!("/api/file-deliveries/{id}?ws=foreign"),
                false
            )
            .header("cookie", &cookies)
            .send()
            .await
            .unwrap()
            .status(),
            404
        );
        assert_eq!(
            e.request("GET", &format!("/api/file-deliveries/{id}/raw"), true)
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
async fn artifact_contract_only_links_registered_urls_in_the_same_item_and_workspace() {
    let e = start(false).await;
    let d = e
        .svc
        .accept(&e.session, "Report", "report.md", b"report", None)
        .unwrap();
    let url = e.svc.url(&d.id, &d.workspace_id);
    // Simulate a pre-fix receipt: old registered full URLs must gain the safe association too.
    let old = e
        .svc
        .accept(&e.session, "Old", "old.md", b"old", None)
        .unwrap();
    let old_url = format!("{}/deliveries/{}", e.base, old.id);
    e.store.transaction(|t| {
        t.execute("UPDATE deliveries SET url=? WHERE id=?", rusqlite::params![old_url, old.id])?;
        t.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES('ST-2','default',2,'another item','run','work',1,1)", [])?;
        Ok::<_, StoreError>(())
    }).unwrap();
    for (title, location) in [
        ("Registered", url.clone()),
        ("Old receipt", old_url),
        (
            "External",
            format!("https://outside.example/deliveries/{}?ws=default", d.id),
        ),
        (
            "Wrong selector",
            format!("{}/deliveries/{}?ws=foreign", e.base, d.id),
        ),
        (
            "Foreign origin suffix",
            format!(
                "https://127.0.0.1.outside.example/deliveries/{}?ws=default",
                d.id
            ),
        ),
        ("Relative unregistered URL", format!("/deliveries/{}", d.id)),
        ("Legacy file", "outputs/old-report.md".into()),
    ] {
        e.store
            .artifacts()
            .add("ST-1", "file", title, Some(&location))
            .unwrap();
    }
    e.store
        .artifacts()
        .add("ST-2", "link", "Other item's version", Some(&url))
        .unwrap();
    let detail: Value = e
        .request("GET", "/api/items/ST-1", true)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let artifacts = detail["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 7);
    for artifact in artifacts {
        match artifact["title"].as_str().unwrap() {
            "Registered" => assert_eq!(artifact["delivery_id"], d.id),
            "Old receipt" => assert_eq!(artifact["delivery_id"], old.id),
            _ => assert!(artifact["delivery_id"].is_null(), "{artifact}"),
        }
    }
    assert!(
        e.store.artifacts().list("ST-2").unwrap()[0]
            .delivery_id
            .is_none()
    );
    // Moving the owning item must not keep a live association across workspace boundaries.
    e.store
        .transaction(|t| {
            t.execute(
                "UPDATE items SET workspace_id='foreign',no=2 WHERE id='ST-1'",
                [],
            )?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
    assert!(
        e.store
            .artifacts()
            .list("ST-1")
            .unwrap()
            .iter()
            .all(|a| a.delivery_id.is_none())
    );
    assert_eq!(
        e.request("GET", &format!("/api/file-deliveries/{}", d.id), true)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test]
async fn original_tool_page_url_binds_workspace_after_real_switch_and_still_checks_membership() {
    let e = start(true).await;
    std::fs::write(PathBuf::from(&e.session.cwd).join("design.zip"), html_zip()).unwrap();
    let result = e
        .tool("steve_deliver_file", json!({"path":"design.zip"}))
        .await;
    assert_ne!(result["result"]["isError"], true, "{result}");
    let meta: Value =
        serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let warning = meta["access_warning"].as_str().unwrap();
    assert!(warning.contains("loopback or local-only") && warning.contains("static sandbox"));
    let page = reqwest::Url::parse(meta["url"].as_str().unwrap()).unwrap();
    let id = meta["id"].as_str().unwrap();
    let selector = page
        .query_pairs()
        .find(|(key, _)| key == "ws")
        .unwrap()
        .1
        .into_owned();
    assert_eq!(page.path(), format!("/deliveries/{id}"));
    assert_eq!(selector, "default");
    let owner = e.store.access().owner().unwrap().unwrap();
    e.store.transaction(|t| {
        t.execute("INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('foreign',?,'owner',1)", [&owner.id])?;
        Ok::<_, StoreError>(())
    }).unwrap();
    let cookies = e.switch_workspace("foreign").await;
    let state: Value = e
        .request("GET", "/api/state", false)
        .header("cookie", &cookies)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["workspace"]["id"], "foreign");
    // The delivery frontend binds the page's ws before bootstrap. Emulate that exact header,
    // then assert the cookie really would select the other workspace without it.
    let bound: Value = e
        .request("GET", "/api/state", false)
        .header("cookie", &cookies)
        .header("x-steve-ws", &selector)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bound["workspace"]["id"], "default");
    for tail in [
        "",
        "/raw",
        "/content/site/index.html",
        "/content/site/assets/site.css",
        "/content/site/next.html",
    ] {
        let path = format!("/api/file-deliveries/{id}{tail}");
        assert_eq!(
            e.request("GET", &path, false)
                .header("cookie", &cookies)
                .header("x-steve-ws", &selector)
                .send()
                .await
                .unwrap()
                .status(),
            200,
            "{tail}"
        );
        // Relative resources have no selector, but retain ordinary membership enforcement.
        assert_eq!(
            e.request("GET", &path, false)
                .header("cookie", &cookies)
                .send()
                .await
                .unwrap()
                .status(),
            200,
            "{tail}"
        );
        for request in [
            e.request("GET", &path, false)
                .header("x-steve-ws", "foreign"),
            e.request("GET", &format!("{path}?ws=foreign"), false),
        ] {
            assert_eq!(
                request
                    .header("cookie", &cookies)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                404,
                "{tail}"
            );
        }
        assert_eq!(
            e.request("GET", &format!("{path}?ws=default"), false)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    e.store
        .transaction(|t| {
            t.execute(
                "DELETE FROM workspace_members WHERE workspace_id='default' AND user_id=?",
                [&owner.id],
            )?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
    for tail in [
        "",
        "/raw",
        "/content/site/index.html",
        "/content/site/assets/site.css",
        "/content/site/next.html",
    ] {
        let path = format!("/api/file-deliveries/{id}{tail}");
        for request in [
            e.request("GET", &path, false)
                .header("x-steve-ws", &selector),
            e.request("GET", &path, false),
        ] {
            assert_eq!(
                request
                    .header("cookie", &cookies)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                409,
                "{tail}"
            );
        }
    }
}

#[tokio::test]
async fn both_delivery_tools_reject_dependency_build_and_checkout_paths() {
    let e = start(false).await;
    let cwd = PathBuf::from(&e.session.cwd);
    for relative in [
        "outputs/node_modules/package/report.md",
        "outputs/review-source/report.md",
        "target/debug/report.md",
        ".git/report.md",
        "dist/report.md",
    ] {
        let path = cwd.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not an independent deliverable").unwrap();
        for (tool, args) in [
            ("steve_deliver_file", json!({"path":path})),
            (
                "steve_deliver",
                json!({"summary":"blocked output", "outputs":[{"kind":"file","title":"report","location":path}], "checks":[], "unfinished":[]}),
            ),
        ] {
            let response = e.tool(tool, args).await;
            assert_eq!(response["result"]["isError"], true, "{tool}: {response}");
            assert!(
                response["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("repository metadata, dependencies or build outputs"),
                "{response}"
            );
        }
    }
    let count = e
        .store
        .reader::<_, StoreError>(|c| {
            Ok(c.query_row("SELECT count(*) FROM deliveries", [], |r| {
                r.get::<_, i64>(0)
            })?)
        })
        .unwrap();
    assert_eq!(count, 0, "neither tool may publish rejected paths");
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
    let path = format!("/api/file-deliveries/{}/shares", d.id);
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
        (format!("/api/file-deliveries/{}", d.id), true),
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
        assert_eq!(css.headers()["content-type"], "text/css; charset=utf-8");
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
        e.request("GET", &format!("/api/file-deliveries/{}", d.id), true)
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

/// #205: a signed-in user publishes without an item (CLI: an access client), lists "我的发布",
/// switches the interactive preview, and the frame is served only through its own capability with
/// the isolating CSP, for the signed-in page and a share alike.
#[tokio::test]
async fn users_publish_and_interactive_frames_are_isolated_capabilities() {
    let e = start(false).await;
    let owner = e.store.access().owner().unwrap().unwrap();
    let (_, token) = e
        .store
        .access()
        .token_create(&owner.id, "cli", None)
        .unwrap();
    let signed: Value = e
        .request("POST", "/auth/token", false)
        .json(&json!({"user": owner.name, "token": token, "client": {"kind": "cli", "name": "steve publish"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bearer = format!("Bearer {}", signed["credential"].as_str().unwrap());
    let upload = |auth: Option<&str>, ws: Option<&str>, query: &str| {
        let mut r = e
            .request("POST", &format!("/api/file-deliveries?{query}"), false)
            .header("content-type", "application/octet-stream")
            .body(html_zip());
        if let Some(a) = auth {
            r = r.header("authorization", a);
        }
        if let Some(ws) = ws {
            r = r.header("x-steve-ws", ws);
        }
        r
    };
    // Bot session tokens and anonymous callers cannot upload; a user must name the workspace.
    for (auth, ws, status) in [
        (Some("Bearer delivery-bot"), Some("default"), 401),
        (None, Some("default"), 401),
        (Some(bearer.as_str()), None, 403),
        (Some(bearer.as_str()), Some("foreign"), 409),
    ] {
        let r = upload(auth, ws, "name=site.zip").send().await.unwrap();
        assert_eq!(r.status(), status, "{auth:?} {ws:?}");
    }
    let r = upload(
        Some(&bearer),
        Some("default"),
        "name=site.zip&title=Prototype&entrypoint=site/index.html",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let d: Value = r.json().await.unwrap();
    let id = d["id"].as_str().unwrap().to_string();
    assert!(d["item_id"].is_null());
    assert_eq!(d["title"], "Prototype");
    assert_eq!(d["interactive"], false);
    assert_eq!(d["can_manage"], true);
    assert_eq!(d["scripts"], true, "the static page needs scripts");
    assert!(d["frame_url"].is_null());
    assert_eq!(d["reachability"], "local");
    let list: Value = e
        .request("GET", "/api/file-deliveries", true)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list[0]["id"], id.as_str(), "{list}");
    // Bots cannot switch it; the managing user can.
    let set = |on: bool, login: bool| {
        let mut r = e
            .request("PATCH", &format!("/api/file-deliveries/{id}"), login)
            .json(&json!({"interactive": on}));
        if !login {
            r = r.header("authorization", "Bearer delivery-bot");
        }
        r
    };
    assert_eq!(set(true, false).send().await.unwrap().status(), 401);
    let on: Value = set(true, true).send().await.unwrap().json().await.unwrap();
    assert_eq!(on["interactive"], true);
    let frame = on["frame_url"].as_str().unwrap().to_string();
    assert!(
        frame.starts_with(&format!("{}/api/delivery-frames/", e.base)),
        "{frame}"
    );
    // The frame is served with no cookie at all, with the isolating policy; the static content URL
    // keeps scripts off whatever the switch says.
    let anonymous = reqwest::Client::new();
    let r = anonymous.get(&frame).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let csp = r.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    for part in [
        "sandbox allow-scripts;",
        "connect-src 'none'",
        "form-action 'none'",
        "frame-ancestors 'self' ",
    ] {
        assert!(csp.contains(part), "{part}: {csp}");
    }
    assert!(!csp.contains("allow-same-origin"));
    let css = frame.replace("index.html", "assets/site.css");
    assert_eq!(anonymous.get(&css).send().await.unwrap().status(), 200);
    let content = on["content_url"].as_str().unwrap();
    let r = e
        .request("GET", content.strip_prefix(&e.base).unwrap(), true)
        .send()
        .await
        .unwrap();
    assert!(
        r.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'none'")
    );
    // A share honours the same switch, rechecked on every request.
    let share: Value = e
        .request("POST", &format!("/api/file-deliveries/{id}/shares"), true)
        .json(&json!({"expires_at": steve_store::now() + 60_000}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let t = share["token"].as_str().unwrap();
    let public: Value = anonymous
        .get(format!("{}/api/delivery-shares/{t}", e.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let shared_frame = public["frame_url"].as_str().unwrap().to_string();
    assert!(shared_frame.contains(&format!("/api/delivery-shares/{t}/frame/")));
    assert!(public.get("can_manage").is_none() && public.get("workspace_id").is_none());
    assert_eq!(
        anonymous.get(&shared_frame).send().await.unwrap().status(),
        200
    );
    assert_eq!(set(false, true).send().await.unwrap().status(), 200);
    assert_eq!(
        anonymous.get(&shared_frame).send().await.unwrap().status(),
        404
    );
    assert_eq!(anonymous.get(&frame).send().await.unwrap().status(), 404);
}

/// #205: steve_deliver_file works in a session outside an item, inside its own directory.
#[tokio::test]
async fn sessions_without_an_item_deliver_files() {
    let e = start(true).await;
    let cwd = PathBuf::from(&e.session.cwd).parent().unwrap().join("chat");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(cwd.join("notes.md"), "# Notes").unwrap();
    e.store
        .sessions()
        .create(&steve_store::NewSession {
            workspace_id: "default".into(),
            item_id: None,
            cwd: cwd.display().to_string(),
            harness: "test".into(),
            title: "chat".into(),
            token: "chat-bot".into(),
            ..Default::default()
        })
        .unwrap();
    let call = |path: &str| {
        e.request("POST", "/mcp", false)
            .header("authorization", "Bearer chat-bot")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-06-18")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"steve_deliver_file","arguments":{"path":path,"interactive":true}}}))
    };
    let r: Value = call("notes.md").send().await.unwrap().json().await.unwrap();
    assert_ne!(r["result"]["isError"], true, "{r}");
    let meta: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(meta["item_id"].is_null());
    assert_eq!(meta["mime"], "text/markdown");
    let r: Value = call("../ST-1/x.md")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        r["result"]["isError"], true,
        "the session directory still bounds it: {r}"
    );
}
