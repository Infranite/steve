//! Installing a Node on another machine over tailcat's SSH (A6b), with stand-ins: `ssh` runs the
//! command locally with a temporary home as the target's, and the installer comes from a local
//! release directory (`STEVE_NODE_INSTALLERS`) whose `steve` records what it was asked.

mod node_harness;

use std::path::Path;

use node_harness::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_hub::nodes::install::Installs;
use steve_node_wire::Identity;

const ADDR: &str = "tcFAKEtarget0123456789_-x";
/// An address whose setup fails as an unpaired one did.
const FAIL: &str = "tcFAILtarget0123456789_-x";
/// The screenshot's failure: the probe succeeds, sending the archive exits 255.
const FAIL_SEND: &str = "tcSENDtarget0123456789_-x";

fn script(p: &Path, body: &str) {
    std::fs::write(p, format!("#!/bin/sh\n{body}")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A release directory for version 9.9.9 whose installer's `steve` records its arguments and the
/// pairing code it was given, names its phases as setup does, then waits until the test has paired
/// the Node as it would have.
fn release(dir: &Path) {
    let pkg = dir.join("pkg");
    std::fs::create_dir_all(pkg.join("bin")).unwrap();
    script(
        &pkg.join("bin/steve"),
        "case \"$1\" in\n setup) [ -n \"$STEVE_SETUP_PROGRESS\" ] && echo 'steve-setup-progress install' && echo 'steve-setup-progress pair'; \
         echo \"$@\" > \"$HOME/setup-args\"; echo \"$STEVE_PAIR_CODE\" > \"$HOME/code\"; \
         for i in $(seq 1 200); do [ -f \"$HOME/paired\" ] && echo 'steve-setup-progress start' && exit 0; sleep 0.1; done; exit 1 ;;\n \
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

async fn stopped(h: &Hub, id: &str) -> Value {
    let result = std::sync::Mutex::new(Value::Null);
    steve_runtime::testkit::eventually(
        format!("Install {id} leaves its running state."),
        || async {
            let (s, v) = call(h, reqwest::Method::GET, "u1", &format!("/{id}"), json!({})).await;
            assert_eq!(s, 200);
            if v["state"] != "running" {
                *result.lock().unwrap() = v;
                true
            } else {
                false
            }
        },
    )
    .await;
    result.into_inner().unwrap()
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
            "echo \"$@\" >> '{log}'\nfor a; do cmd=\"$a\"; done\n\
             socket=''\nprev=''\nfor a; do [ \"$prev\" != -S ] || socket=$a; prev=$a; done\n\
             case \" $* \" in *' -N '*) echo '2026/11/18 10:00:00 magicsock: disco: node [peer] d:123 now using 192.0.2.2:1234 mtu=1360' >&2; \
                 touch \"$socket\"; exec sleep 60 ;; esac\n\
             # The real SSH mux identifies the peer by its private control socket.\n\
             d=$(dirname \"$socket\"); grep -F -- \"-S $socket -o ControlPersist=no -M -N\" '{log}' | tail -1 > \"$d/route\"; master=$(cat \"$d/route\");\n\
             case \"$master $*\" in\n\
             *{FAIL_SEND}*) case \"$cmd\" in *\"uname -sm\"*) ;; *) cat >/dev/null; echo 'transfer failed' >&2; exit 255 ;; esac ;;\n\
             *{FAIL}*\"setup node\"*) cat > '{home}/failed-code'; echo 'pairing code: Error: no pairing code' >&2; exit 1 ;; esac\n\
             HOME='{home}' exec sh -c \"$cmd\"\n",
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
    let (s, _) = call(
        &h,
        reqwest::Method::DELETE,
        "u1",
        &format!("/{id}"),
        json!({}),
    )
    .await;
    assert_eq!(s, 409, "a running install is not discarded");
    assert!(
        h.dir
            .path()
            .join("tailcat/installs")
            .join(&id)
            .join("key")
            .exists()
    );

    // a second install to the same address joins the running one
    let (_, other) = call(&h, reqwest::Method::POST, "u1", "", json!({})).await;
    let other = other["id"].as_str().unwrap().to_string();
    let (s, joined) = call(
        &h,
        reqwest::Method::POST,
        "u1",
        &format!("/{other}/run"),
        json!({"address": ADDR}),
    )
    .await;
    assert_eq!(
        (s, joined["id"].as_str()),
        (200, Some(id.as_str())),
        "{joined}"
    );
    let (_, v) = call(
        &h,
        reqwest::Method::GET,
        "u1",
        &format!("/{other}"),
        json!({}),
    )
    .await;
    assert_eq!(v["state"], "waiting", "{v}");
    // a page opened now finds the running one
    let (_, all) = call(&h, reqwest::Method::GET, "u1", "", json!({})).await;
    let running: Vec<&str> = all["installs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["state"] == "running")
        .filter_map(|o| o["id"].as_str())
        .collect();
    assert_eq!(running, [id.as_str()], "{all}");
    let (_, all) = call(&h, reqwest::Method::GET, "u2", "", json!({})).await;
    assert_eq!(all["installs"], json!([]), "nobody else's");

    // what the Node would do with the code it was given: pair with it
    let code_file = target.path().join("code");
    steve_runtime::testkit::eventually_sync("The installer writes its pairing code.", || {
        code_file.exists()
    })
    .await;
    let code = std::fs::read_to_string(&code_file)
        .unwrap()
        .trim()
        .to_string();
    assert!(code.starts_with("stv1-"));
    assert_eq!(h.claim(&code, &Identity::generate().unwrap()).await.0, 200);
    std::fs::write(target.path().join("paired"), "").unwrap();

    let done = stopped(&h, &id).await;
    assert_eq!(done["state"], "done", "{done}");
    let node = done["node_id"].as_str().unwrap();
    // confirmed by the install: active without a fingerprint check
    let state: String = h.q(&format!("SELECT state FROM nodes WHERE node_id='{node}'"));
    assert_eq!(state, "active");
    let steps = done["steps"].as_array().unwrap();
    let kinds: Vec<&str> = steps.iter().filter_map(|s| s["kind"].as_str()).collect();
    assert_eq!(kinds, ["probe", "fetch", "send", "setup", "done"], "{done}");
    assert!(
        steps
            .iter()
            .all(|s| s["ended"].is_i64() && s["elapsed_ms"].as_i64().is_some_and(|e| e >= 0)),
        "{done}"
    );
    // how far each went, kept in the install
    let size = std::fs::metadata(releases.path().join("steve-9.9.9-installer.tar.gz"))
        .unwrap()
        .len();
    assert_eq!(steps[1]["progress"]["phase"], "verify", "{done}");
    assert_eq!(steps[2]["progress"]["phase"], "unpack", "{done}");
    assert_eq!(steps[2]["progress"]["done"], size, "{done}");
    assert_eq!(steps[2]["progress"]["total"], size, "{done}");
    assert!(
        steps[2]["transfer_ms"].is_u64() && steps[2]["unpack_ms"].is_u64(),
        "{done}"
    );
    assert_eq!(done["transfer_route"], "direct_observed");
    assert_eq!(steps[3]["progress"]["phase"], "start", "{done}");
    assert!(done["address"].is_null(), "not kept once installed: {done}");

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
            "ProxyCommand='{}' --verbose {ADDR} 22",
            tools.path().join("tailcat").display()
        )),
        "{log}"
    );
    assert!(!target.path().join(".steve-node-install").exists());
    // and the install's key with it
    assert!(!h.dir.path().join("tailcat/installs").join(&id).exists());

    // a failed install says at which step and why, and keeps the address for a retry
    let (_, failing) = call(&h, reqwest::Method::POST, "u1", "", json!({})).await;
    let failing = failing["id"].as_str().unwrap().to_string();
    call(
        &h,
        reqwest::Method::POST,
        "u1",
        &format!("/{failing}/run"),
        json!({"address": FAIL}),
    )
    .await;
    let failed = stopped(&h, &failing).await;
    assert_eq!(failed["state"], "failed", "{failed}");
    assert_eq!(failed["failed_step"], "setup", "{failed}");
    assert_eq!(failed["address"], FAIL, "{failed}");
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("no pairing code"),
        "{failed}"
    );
    assert!(failed["steps"].as_array().unwrap().last().unwrap()["ended"].is_i64());
    let (_, all) = call(&h, reqwest::Method::GET, "u1", "", json!({})).await;
    assert_eq!(
        all["installs"][0]["id"],
        failing.as_str(),
        "the newest first: {all}"
    );

    // A setup that failed must not leave a still-usable auto-confirming pairing code.
    let failed_code = std::fs::read_to_string(target.path().join("failed-code")).unwrap();
    assert_eq!(
        h.claim(failed_code.trim(), &Identity::generate().unwrap())
            .await
            .0,
        403
    );

    // Only its owner can cancel it, including its SSH key. Repeated cancellation is harmless.
    let failed_dir = h.dir.path().join("tailcat/installs").join(&failing);
    assert!(failed_dir.join("key").exists());
    let (s, _) = call(
        &h,
        reqwest::Method::DELETE,
        "u2",
        &format!("/{failing}"),
        json!({}),
    )
    .await;
    assert_eq!(s, 404);
    assert!(failed_dir.join("key").exists());
    for _ in 0..2 {
        let (s, _) = call(
            &h,
            reqwest::Method::DELETE,
            "u1",
            &format!("/{failing}"),
            json!({}),
        )
        .await;
        assert_eq!(s, 200);
    }
    assert!(!failed_dir.exists());
    let (s, _) = call(
        &h,
        reqwest::Method::GET,
        "u1",
        &format!("/{failing}"),
        json!({}),
    )
    .await;
    assert_eq!(s, 404, "cancelled attempts cannot be resumed");
    let (s, _) = call(
        &h,
        reqwest::Method::POST,
        "u1",
        &format!("/{failing}/run"),
        json!({"address": ADDR}),
    )
    .await;
    assert_eq!(s, 404, "a stale tab cannot restart a cancelled attempt");

    // Waiting installs can be discarded too, but completed installs and their paired Nodes stay.
    let (s, _) = call(
        &h,
        reqwest::Method::DELETE,
        "u1",
        &format!("/{other}"),
        json!({}),
    )
    .await;
    assert_eq!(s, 200);
    assert!(!h.dir.path().join("tailcat/installs").join(&other).exists());
    let (s, _) = call(
        &h,
        reqwest::Method::DELETE,
        "u1",
        &format!("/{id}"),
        json!({}),
    )
    .await;
    assert_eq!(s, 409);
    assert_eq!(
        h.q::<String>(&format!("SELECT state FROM nodes WHERE node_id='{node}'")),
        "active"
    );

    // Reproduce the send/255 failure from the screenshot, then cancel it.
    let (_, send) = call(&h, reqwest::Method::POST, "u1", "", json!({})).await;
    let send_id = send["id"].as_str().unwrap();
    let (s, _) = call(
        &h,
        reqwest::Method::POST,
        "u1",
        &format!("/{send_id}/run"),
        json!({"address": FAIL_SEND}),
    )
    .await;
    assert_eq!(s, 200);
    let send_failed = stopped(&h, send_id).await;
    assert_eq!(send_failed["failed_step"], "send");
    assert!(send_failed["error"].as_str().unwrap().contains("255"));
    let (s, _) = call(
        &h,
        reqwest::Method::DELETE,
        "u1",
        &format!("/{send_id}"),
        json!({}),
    )
    .await;
    assert_eq!(s, 200);
    assert!(!h.dir.path().join("tailcat/installs").join(send_id).exists());
    let (_, all) = call(&h, reqwest::Method::GET, "u1", "", json!({})).await;
    assert_eq!(
        all["installs"].as_array().unwrap().len(),
        1,
        "only the completed install remains"
    );

    // The next target gets a new key and operation, and can really claim its new code.
    std::fs::remove_file(target.path().join("paired")).unwrap();
    std::fs::remove_file(&code_file).unwrap();
    let (_, next) = call(&h, reqwest::Method::POST, "u1", "", json!({})).await;
    let next_id = next["id"].as_str().unwrap();
    assert_ne!(next["id"], send["id"]);
    assert_ne!(next["command"], send["command"]);
    assert_eq!(next["state"], "waiting");
    assert!(next["address"].is_null());
    assert_eq!(next["steps"], json!([]));
    let (s, _) = call(
        &h,
        reqwest::Method::POST,
        "u1",
        &format!("/{next_id}/run"),
        json!({"address": ADDR}),
    )
    .await;
    assert_eq!(s, 200);
    steve_runtime::testkit::eventually_sync(
        "The retried installer writes its next pairing code.",
        || code_file.exists(),
    )
    .await;
    let next_code = std::fs::read_to_string(&code_file).unwrap();
    let (s, claim) = h
        .claim(next_code.trim(), &Identity::generate().unwrap())
        .await;
    assert_eq!(s, 200);
    std::fs::write(target.path().join("paired"), "").unwrap();
    let next_done = stopped(&h, next_id).await;
    assert_eq!(next_done["state"], "done");
    assert_eq!(next_done["node_id"], claim["node_id"]);
    assert_ne!(next_done["node_id"], done["node_id"]);
}
