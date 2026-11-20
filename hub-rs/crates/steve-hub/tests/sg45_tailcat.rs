//! SG45: the tunnel reports addresses and stop ends only its own scripted child.
#![cfg(unix)]
mod node_harness;
use node_harness::Hub;
use std::os::unix::fs::PermissionsExt;
use steve_hub::nodes::tailcat::Tailcat;
use steve_runtime::testkit::eventually_sync;
#[tokio::test]
async fn sg45_tailcat_reads_the_saved_key_and_running_address_then_resets_on_stop() {
    let h = Hub::start().await;
    let script = h.dir.path().join("tunnel-fixture.py");
    std::fs::write(
        &script,
        r#"#!/usr/bin/env python3
import pathlib, signal, sys
if sys.argv[1] == 'genkey':
    pathlib.Path(sys.argv[2].removeprefix('--key=')).write_text('{}')
    print('tcFAKEaddress0123456789_-x')
else:
    print('diagnostic before address', file=sys.stderr, flush=True)
    print('# Server listening: tcFAKEaddress0123456789_-x', file=sys.stderr, flush=True)
    signal.pause()
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: this binary has one test and owns this process-local fixture setting.
    unsafe {
        std::env::set_var("STEVE_TAILCAT", &script);
    }
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let t = Tailcat::new(h.dir.path(), port, &h.svc);
    t.start().await.unwrap();
    eventually_sync("tunnel address reached running state", || {
        t.status()["running"] == true
    })
    .await;
    assert_eq!(t.status()["address"], "tcFAKEaddress0123456789_-x");
    assert!(h.dir.path().join("tailcat/hub.private.json").exists());
    t.stop();
    assert_eq!(t.status()["enabled"], false);
    assert_eq!(t.status()["running"], false);
    assert!(t.status()["address"].is_null());
    t.start().await.unwrap();
    eventually_sync("saved key reconnect", || t.status()["running"] == true).await;
    t.stop();
}
