//! Installing a Node on another machine over tailcat's SSH (A6b), with stand-ins: `ssh` runs the
//! command locally with a temporary home as the target's, and the installer comes from a local
//! release directory (`STEVE_NODE_INSTALLERS`) whose `steve` records what it was asked.

mod node_harness;

use std::path::Path;
use std::time::Duration;

use node_harness::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_hub::nodes::install::Installs;
use steve_node_wire::Identity;

const ADDR: &str = "tcFAKEtarget0123456789_-x";

fn script(p: &Path, body: &str) {
    std::fs::write(p, format!("#!/bin/sh\n{body}")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A release directory for version 9.9.9 whose installer's `steve` records its arguments and the
/// pairing code it was given, then waits until the test has paired the Node as it would have.
fn release(dir: &Path) {
    let pkg = dir.join("pkg");
    std::fs::create_dir_all(pkg.join("bin")).unwrap();
    script(
        &pkg.join("bin/steve"),
        "case \"$1\" in\n setup) echo \"$@\" > \"$HOME/setup-args\"; echo \"$STEVE_PAIR_CODE\" > \"$HOME/code\"; \
         for i in $(seq 1 200); do [ -f \"$HOME/paired\" ] && exit 0; sleep 0.1; done; exit 1 ;;\n \
         node) echo \"$@\" > \"$HOME/started\" ;;\nesac\n",
    );
    let name = "steve-9.9.9-installer.tar.gz";
    let ok = std::process::Command::new("tar")
        .args(["czf"])
        .arg(dir.join(name))
        .args(["-C"])
        .arg(&pkg)
        .arg("bin")
        .status()
        .unwrap();
    assert!(ok.success());
    let bytes = std::fs::read(dir.join(name)).unwrap();
    let asset = |target: &str| json!({"target": target, "name": name, "sha256": hex::encode(Sha256::digest(&bytes)), "size": bytes.len()});
    std::fs::write(
        dir.join("steve-installer.json"),
        json!({"version": "9.9.9", "assets": [asset("aarch64-apple-darwin"), asset("x86_64-unknown-linux-gnu")]})
            .to_string(),
    )
    .unwrap();
}

async fn call(
    h: &Hub,
    method: reqwest::Method,
    user: &str,
    path: &str,
    body: Value,
) -> (u16, Value) {
    let r = reqwest::Client::new()
        .request(method, format!("{}/api/nodes/installs{path}", h.url()))
        .header("x-test-user", user)
        .json(&body)
        .send()
        .await
        .unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hub_installs_a_node_that_is_active_once_paired() {
    let tools = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let releases = tempfile::tempdir().unwrap();
    release(releases.path());
    script(&tools.path().join("tailcat"), "exit 0\n");
    // the last argument is the remote command; the target is this machine with another home
    script(
        &tools.path().join("ssh"),
        &format!(
            "echo \"$@\" >> '{log}'\nfor a; do cmd=\"$a\"; done\nHOME='{home}' exec sh -c \"$cmd\"\n",
            log = tools.path().join("ssh.log").display(),
            home = target.path().display()
        ),
    );
    // SAFETY: this test binary has this one test; nothing else reads the environment meanwhile
    unsafe {
        std::env::set_var("STEVE_TAILCAT", tools.path().join("tailcat"));
        std::env::set_var("STEVE_SSH", tools.path().join("ssh"));
        std::env::set_var("STEVE_NODE_INSTALLERS", releases.path());
    }
    let h = Hub::start().await;
    assert!(
        h.svc
            .installs
            .set(Installs::new(h.dir.path(), "9.9.9", "https://hub.example"))
            .is_ok()
    );
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

    let (s, _) = call(&h, reqwest::Method::POST, "u2", "", json!({})).await;
    assert_eq!(s, 403, "only the owner installs");
    let (s, op) = call(&h, reqwest::Method::POST, "u1", "", json!({})).await;
    assert_eq!(s, 200, "{op}");
    let id = op["id"].as_str().unwrap().to_string();
    let command = op["command"].as_str().unwrap();
    assert!(
        command.starts_with("tailcat serve --ssh-authorized-keys='ssh-ed25519 ")
            && command.ends_with(" steve-install' ssh"),
        "{command}"
    );
    let (s, _) = call(&h, reqwest::Method::GET, "u2", &format!("/{id}"), json!({})).await;
    assert_eq!(s, 404, "another user's install is not theirs to see");
    let (s, _) = call(
        &h,
        reqwest::Method::POST,
        "u1",
        &format!("/{id}/run"),
        json!({"address": "not an address"}),
    )
    .await;
    assert_eq!(s, 400);

    let (s, v) = call(
        &h,
        reqwest::Method::POST,
        "u1",
        &format!("/{id}/run"),
        json!({"address": format!(" {ADDR}\n")}),
    )
    .await;
    assert_eq!((s, v["state"].as_str()), (200, Some("running")), "{v}");

    // what the Node would do with the code it was given: pair with it
    let code_file = target.path().join("code");
    for _ in 0..200 {
        if code_file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let code = std::fs::read_to_string(&code_file)
        .unwrap()
        .trim()
        .to_string();
    assert!(code.starts_with("stv1-"));
    assert_eq!(h.claim(&code, &Identity::generate().unwrap()).await.0, 200);
    std::fs::write(target.path().join("paired"), "").unwrap();

    let mut done = Value::Null;
    for _ in 0..200 {
        let (_, v) = call(&h, reqwest::Method::GET, "u1", &format!("/{id}"), json!({})).await;
        if v["state"] != "running" {
            done = v;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(done["state"], "done", "{done}");
    let node = done["node_id"].as_str().unwrap();
    // confirmed by the install: active without a fingerprint check
    let state: String = h.q(&format!("SELECT state FROM nodes WHERE node_id='{node}'"));
    assert_eq!(state, "active");
    assert_eq!(done["steps"].as_array().unwrap().len(), 5, "{done}");

    let args = std::fs::read_to_string(target.path().join("setup-args")).unwrap();
    assert_eq!(args.trim(), "setup node --hub https://hub.example");
    assert_eq!(
        std::fs::read_to_string(target.path().join("started"))
            .unwrap()
            .trim(),
        "node start"
    );
    // the code went over stdin, never into an argument list; the unpacked installer is gone
    let log = std::fs::read_to_string(tools.path().join("ssh.log")).unwrap();
    assert!(!log.contains(&code), "{log}");
    assert!(
        log.contains(&format!(
            "ProxyCommand='{}' {ADDR} 22",
            tools.path().join("tailcat").display()
        )),
        "{log}"
    );
    assert!(!target.path().join(".steve-node-install").exists());
    // and the install's key with it
    assert!(!h.dir.path().join("tailcat/installs").join(&id).exists());
}
