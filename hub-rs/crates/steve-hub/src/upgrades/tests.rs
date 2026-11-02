use super::*;
#[test]
fn manifest_rejects_mixed_versions() {
    let mut m = fixture();
    assert!(client::validate_manifest(&m, "0.2.0").is_ok());
    m.web_version = "0.1.0".into();
    assert!(client::validate_manifest(&m, "0.2.0").is_err());
}
fn fixture() -> Manifest {
    Manifest {
        format_version: 1,
        version: "0.2.0".into(),
        commit: "a".repeat(40),
        hub_version: "0.2.0".into(),
        web_version: "0.2.0".into(),
        api_protocol: 1,
        schema: Schema {
            min_read: 1,
            max_read: 1,
            target: 1,
        },
        assets: vec![],
    }
}
#[test]
fn archive_links_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o777);
    header.set_cksum();
    tar.append_link(&mut header, "escape", "/tmp").unwrap();
    let bytes = tar.into_inner().unwrap().finish().unwrap();
    assert!(client::unpack(&bytes, dir.path(), &fixture()).is_err());
}
#[test]
fn durable_request_is_idempotent_and_permission_separated() {
    let dir = tempfile::tempdir().unwrap();
    let u = Updates {
        home: dir.path().into(),
        repository: "Infranite/steve".into(),
        current_hub: "0.1.0".into(),
        current_web: "0.1.0".into(),
        managed: true,
        blocked_reason: None,
        client: reqwest::Client::new(),
        api_base: String::new(),
    };
    let op = u.request("0.2.0", "one", "agent:s", false).unwrap();
    assert_eq!(op.phase, "awaiting_approval");
    assert_eq!(
        u.request("0.2.0", "one", "agent:s", false).unwrap().id,
        op.id
    );
    assert!(u.request("0.3.0", "one", "agent:s", false).is_err());
    assert!(u.request("0.2.0", "two", "agent:other", false).is_err());
    assert_eq!(u.approve(&op.id, "owner").unwrap().phase, "staging");
}

#[tokio::test]
async fn github_checks_validate_assets_and_bind_cache_to_repository_and_both_versions() {
    use axum::response::IntoResponse;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    let home = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let fetch_calls = calls.clone();
    let outage = Arc::new(AtomicBool::new(true));
    let fail = outage.clone();
    let url = base.clone();
    let app=axum::Router::new().route("/repos/test/{repo}/releases/latest",axum::routing::get(move|axum::extract::Path(repo):axum::extract::Path<String>|{
        let url=url.clone();let calls=fetch_calls.clone();let fail=fail.clone();async move{
            calls.fetch_add(1,Ordering::SeqCst);
            if repo=="absent"{return axum::http::StatusCode::NOT_FOUND.into_response()}
            if repo=="limited"{return axum::http::StatusCode::TOO_MANY_REQUESTS.into_response()}
            if repo=="flaky"&&fail.load(Ordering::SeqCst){return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response()}
            let mut assets=vec![];
            if repo!="no-manifest"{assets.push(json!({"name":"steve-release.json","browser_download_url":format!("{url}/manifest/{repo}")}));}
            if repo!="missing-bundle"{assets.push(json!({"name":"bundle.tar.gz","browser_download_url":format!("{url}/bundle")}));}
            axum::Json(json!({"tag_name":"v0.2.0","html_url":"https://github.com/test/release","body":"changes","draft":false,"prerelease":false,"assets":assets})).into_response()
        }
    })).route("/manifest/{repo}",axum::routing::get(|axum::extract::Path(repo):axum::extract::Path<String>|async move{
        if repo=="broken"{return axum::Json(json!({"invalid":"manifest"})).into_response()}
        let mut manifest=fixture();manifest.assets.push(Asset{target:if repo=="unsupported"{"other-platform".into()}else{platform()},name:"bundle.tar.gz".into(),size:4,sha256:"a".repeat(64)});
        axum::Json(manifest).into_response()
    }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut u = Updates {
        home: home.path().into(),
        repository: "test/good".into(),
        current_hub: "0.1.0".into(),
        current_web: "0.1.0".into(),
        managed: true,
        blocked_reason: None,
        client: reqwest::Client::new(),
        api_base: base,
    };
    assert_eq!(u.check().await.unwrap().status, "available");
    let first = calls.load(Ordering::SeqCst);
    assert_eq!(u.check().await.unwrap().status, "available");
    assert_eq!(calls.load(Ordering::SeqCst), first);
    u.current_hub = "0.2.0".into();
    assert!(u.status().unwrap()["check"].is_null());
    assert_eq!(u.check().await.unwrap().status, "available");
    assert_eq!(calls.load(Ordering::SeqCst), first + 1);
    u.current_web = "0.2.0".into();
    assert_eq!(u.check().await.unwrap().status, "up_to_date");
    assert_eq!(calls.load(Ordering::SeqCst), first + 2);
    u.current_hub = "0.10.0".into();
    assert_eq!(u.check().await.unwrap().status, "ahead");
    u.current_hub = "dev".into();
    assert_eq!(u.check().await.unwrap().status, "unknown_current");
    for (repo, status) in [
        ("absent", "no_release"),
        ("no-manifest", "error"),
        ("broken", "error"),
        ("unsupported", "unsupported_platform"),
        ("missing-bundle", "unsupported_platform"),
    ] {
        u.repository = format!("test/{repo}");
        assert_eq!(u.check().await.unwrap().status, status, "{repo}");
    }
    u.repository = "test/flaky".into();
    assert_eq!(u.check().await.unwrap().status, "error");
    outage.store(false, Ordering::SeqCst);
    let before = calls.load(Ordering::SeqCst);
    assert_eq!(u.check().await.unwrap().status, "error");
    assert_eq!(calls.load(Ordering::SeqCst), before);
    let mut cached = u.cached_check().unwrap();
    cached.result.checked_at = now() - 61_000;
    atomic(&u.dir().join("check.json"), &cached).unwrap();
    assert_eq!(u.check().await.unwrap().status, "unknown_current");
    assert_eq!(calls.load(Ordering::SeqCst), before + 1);
    u.repository = "test/limited".into();
    assert!(
        u.check()
            .await
            .unwrap()
            .error
            .unwrap()
            .contains("rate limited")
    );
    let before = calls.load(Ordering::SeqCst);
    let mut cached = u.cached_check().unwrap();
    cached.result.checked_at = now() - 61_000;
    atomic(&u.dir().join("check.json"), &cached).unwrap();
    assert_eq!(u.check().await.unwrap().status, "error");
    assert_eq!(calls.load(Ordering::SeqCst), before);
    server.abort();
}

#[tokio::test]
async fn github_bundle_stages_only_after_hash_and_pair_validation() {
    use sha2::{Digest, Sha256};
    let home = tempfile::tempdir().unwrap();
    let embedded = fixture();
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    for (name, bytes) in [
        ("release.json", serde_json::to_vec(&embedded).unwrap()),
        ("web/dist/release.json", br#"{"version":"0.2.0"}"#.to_vec()),
        ("web/dist/index.html", b"<html>release</html>".to_vec()),
        ("bin/steve-hub", b"#!/bin/sh\nexit 0\n".to_vec()),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_size(bytes.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append_data(&mut h, name, bytes.as_slice()).unwrap();
    }
    let bytes = tar.into_inner().unwrap().finish().unwrap();
    let mut manifest = fixture();
    manifest.assets.push(Asset {
        target: platform(),
        name: "bundle.tar.gz".into(),
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let release = json!({"tag_name":"v0.2.0","html_url":"https://github.com/test/good/releases/tag/v0.2.0","body":"changes","draft":false,"prerelease":false,"assets":[{"name":"steve-release.json","browser_download_url":format!("{base}/manifest")},{"name":"bundle.tar.gz","browser_download_url":format!("{base}/bundle")}]});
    let corrupt = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let switch = corrupt.clone();
    let app = axum::Router::new()
        .route(
            "/repos/test/good/releases/tags/v0.2.0",
            axum::routing::get(move || {
                let r = release.clone();
                async move { axum::Json(r) }
            }),
        )
        .route(
            "/manifest",
            axum::routing::get(move || {
                let m = manifest.clone();
                async move { axum::Json(m) }
            }),
        )
        .route(
            "/bundle",
            axum::routing::get(move || {
                let mut b = bytes.clone();
                if switch.load(std::sync::atomic::Ordering::Relaxed) {
                    b[0] ^= 1
                }
                async move { b }
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let u = Updates {
        home: home.path().into(),
        repository: "test/good".into(),
        current_hub: "0.1.0".into(),
        current_web: "0.1.0".into(),
        managed: true,
        blocked_reason: None,
        client: reqwest::Client::new(),
        api_base: base,
    };
    let mut op = u.request("0.2.0", "download", "owner", true).unwrap();
    u.stage(&mut op).await.unwrap();
    assert_eq!(op.phase, "ready");
    assert!(
        PathBuf::from(op.target.clone().unwrap())
            .join("bin/steve-hub")
            .exists()
    );
    assert!(!home.path().join("current").exists());
    u.phase(&mut op, "failed", None).unwrap();
    corrupt.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut bad = u.request("0.2.0", "corrupt", "owner", true).unwrap();
    assert!(
        u.stage(&mut bad)
            .await
            .unwrap_err()
            .to_string()
            .contains("SHA256")
    );
    assert!(bad.target.is_none());
    assert!(!home.path().join("current").exists());
    server.abort();
}

#[test]
fn interrupted_journal_pointer_is_repaired_without_stealing_another_operation() {
    let home = tempfile::tempdir().unwrap();
    let u = Updates {
        home: home.path().into(),
        repository: "test/good".into(),
        current_hub: "0.1.0".into(),
        current_web: "0.1.0".into(),
        managed: true,
        blocked_reason: None,
        client: reqwest::Client::new(),
        api_base: String::new(),
    };
    let orphan = u.request("0.2.0", "interrupted", "owner", false).unwrap();
    fs::remove_file(u.dir().join("active.json")).unwrap();
    assert_eq!(
        u.request("0.2.0", "interrupted", "owner", false)
            .unwrap()
            .id,
        orphan.id
    );
    assert_eq!(u.active().unwrap().unwrap().id, orphan.id);
    fs::remove_file(u.dir().join("active.json")).unwrap();
    let active = u.request("0.3.0", "other", "owner", false).unwrap();
    assert!(
        u.request("0.2.0", "interrupted", "owner", false)
            .unwrap_err()
            .to_string()
            .contains("冲突")
    );
    assert_eq!(u.active().unwrap().unwrap().id, active.id);
    assert!(u.approve(&orphan.id, "owner").is_err());
    let mut expired = active;
    expired.created_at = now() - 86_400_001;
    u.save(&expired).unwrap();
    assert!(u.approve(&expired.id, "owner").is_err());
    assert_eq!(u.operation(&expired.id).unwrap().phase, "failed");
}

#[test]
#[allow(clippy::zombie_processes)] // The nested fixture intentionally dies with its child via process-group SIGKILL.
fn managed_orphan_exit_kills_group_descendants() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    const FIXTURE: &str = "STEVE_TEST_ORPHAN_DESCENDANT_FILE";
    if let Ok(path) = std::env::var(FIXTURE) {
        eprintln!(
            "orphan fixture pid={} pgid={}",
            std::process::id(),
            unsafe { libc::getpgrp() }
        );
        let descendant = std::process::Command::new("/bin/sleep")
            .arg("60")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        fs::write(path, descendant.id().to_string()).unwrap();
        exit_orphaned_managed_hub();
    }
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let process = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "upgrades::tests::managed_orphan_exit_kills_group_descendants",
            "--nocapture",
        ])
        .env(FIXTURE, &pid_file)
        .process_group(0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let child_pid = process.id();
    let output = process.wait_with_output().unwrap();
    if output.status.signal() != Some(libc::SIGKILL) {
        unsafe {
            libc::kill(-(child_pid as i32), libc::SIGKILL);
        }
        if let Ok(pid) = fs::read_to_string(&pid_file)
            .unwrap_or_default()
            .parse::<i32>()
        {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGKILL),
        "orphan fixture pid={child_pid}, status={}, code={:?}; stdout={} stderr={}",
        output.status,
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let pid: i32 = fs::read_to_string(pid_file).unwrap().parse().unwrap();
    for _ in 0..40 {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return;
        }
        // An exited orphan awaiting init reaping is also dead, not a surviving worker.
        let out = std::process::Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&out.stdout);
        if state.trim().is_empty() || state.trim().starts_with('Z') {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    panic!("managed Hub descendant survived process-group shutdown");
}
