//! Remote Nodes over tailcat (A6): the owner's switch, the Node-only listener behind the tunnel,
//! and the setting that brings it back after a restart. A script stands in for the program.

mod node_harness;

use std::time::Duration;

use node_harness::*;
use serde_json::{Value, json};
use steve_hub::nodes::tailcat::{self, Tailcat};
use steve_node_wire::Identity;

const ADDR: &str = "tcFAKEaddress0123456789_-x";

/// `genkey` writes the key and prints the address; `serve` says it listens and stays up;
/// `forward` reports a local end that is the Hub's own Node-only port (no tunnel in a test).
fn fake_tailcat(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("tailcat");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n genkey) echo '{{}}' > \"${{2#--key=}}\"; echo '# wrote file' >&2; echo {ADDR} ;;\n forward) echo \"# forwarding 127.0.0.1:${{3#0:}} -> remote localhost:${{3#0:}}\" >&2; exec sleep 600 ;;\n *) echo \"$@\" > '{args}'; echo '# 🐈 Server listening with saved key \"k\": {ADDR}' >&2; exec sleep 600 ;;\nesac\n",
            args = dir.join("args").display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn call(h: &Hub, method: reqwest::Method, user: &str, body: Option<Value>) -> (u16, Value) {
    let mut r = reqwest::Client::new()
        .request(method, format!("{}/api/nodes/tailcat", h.url()))
        .header("x-test-user", user);
    if let Some(b) = body {
        r = r.json(&b);
    }
    let r = r.send().await.unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owner_puts_the_node_routes_and_only_them_behind_tailcat() {
    let bin = tempfile::tempdir().unwrap();
    let program = fake_tailcat(bin.path());
    // SAFETY: this test binary has this one test; nothing else reads the environment meanwhile
    unsafe { std::env::set_var("STEVE_TAILCAT", &program) };
    let h = Hub::start().await;
    let port = free_port();
    let t = Tailcat::new(h.dir.path(), port, &h.svc);
    assert!(h.svc.tailcat.set(t.clone()).is_ok());

    let (s, v) = call(&h, reqwest::Method::GET, "u1", None).await;
    assert_eq!(s, 200);
    assert_eq!(
        (v["available"].clone(), v["enabled"].clone()),
        (json!(true), json!(false))
    );

    // only the instance's owner turns it on
    h.owns_default("u1");
    h.store
        .transaction::<_, steve_store::StoreError>(|tx| {
            tx.execute(
                "INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('u2','default','u2','u2','member',1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let (s, _) = call(
        &h,
        reqwest::Method::PUT,
        "u2",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(s, 403);

    let (s, v) = call(
        &h,
        reqwest::Method::PUT,
        "u1",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["running"], true, "{v}");
    assert_eq!(v["hub_url"], json!(format!("tailcat:{ADDR}:{port}")));
    assert!(h.dir.path().join("tailcat/hub.private.json").exists());
    let args = std::fs::read_to_string(bin.path().join("args")).unwrap();
    assert!(args.ends_with(&format!("serve {port}\n")), "{args}");
    let saved: bool = h.q("SELECT value='true' FROM settings WHERE key='nodes.tailcat'");
    assert!(saved);

    // the tunnelled port answers the Node routes and nothing else
    let local = format!("http://127.0.0.1:{port}");
    let api = reqwest::get(format!("{local}/api/nodes")).await.unwrap();
    assert_eq!(api.status().as_u16(), 404);
    let code = h.code("u1").await;
    let id = Identity::generate().unwrap();
    let pk = id.public();
    let r = reqwest::Client::new()
        .post(format!("{local}/node/pair"))
        .json(&json!({"code": code, "pubkey": hex::encode(pk), "name": "far",
                      "sig": hex::encode(id.sign(&steve_node_wire::identity::pair_message(&code, &pk)))}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);

    // a Node given the tailcat address pairs through its tunnel and keeps the address
    let node_home = tempfile::tempdir().unwrap();
    let home = steve_node_cli::config::Home(node_home.path().to_path_buf());
    let code = h.code("u1").await;
    let hub = format!("tailcat:{ADDR}:{port}");
    let p = steve_node_cli::dial::pair(&home, &hub, &code, None, "far")
        .await
        .unwrap();
    assert_eq!((p.hub_url.as_str(), p.loopback), (hub.as_str(), false));
    assert_eq!(
        steve_node_cli::dial::trust(&p).unwrap(),
        steve_node_wire::tls::Trust::LoopbackPlaintext
    );
    let mut tunnel = steve_node_cli::tunnel::Tunnel::open(ADDR, port)
        .await
        .unwrap();
    assert_eq!(tunnel.url, local);
    assert!(tunnel.alive());
    drop(tunnel);

    // off: nothing listens; on again after a restart from the setting
    let (s, v) = call(
        &h,
        reqwest::Method::PUT,
        "u1",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!((s, v["enabled"].clone()), (200, json!(false)));
    let mut closed = false;
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(closed, "the Node-only listener outlived the switch");
    let (s, _) = call(
        &h,
        reqwest::Method::PUT,
        "u1",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(s, 200);
    t.stop();
    let again = Tailcat::new(h.dir.path(), free_port(), &h.svc);
    tailcat::restore(again.clone(), h.store.clone()).await;
    assert_eq!(again.status()["enabled"], true);
    again.stop();
    tailcat::shutdown();
}
