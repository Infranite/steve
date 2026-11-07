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
        web_base: String::new(),
        repo: PathBuf::new(),
        kind: None,
        launch: crate::upgrades::runner::Launch::Launchd,
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

#[test]
fn an_unmanaged_install_names_its_kind_and_why_it_is_not_upgraded_here() {
    let dir = tempfile::tempdir().unwrap();
    let u = Updates {
        home: dir.path().into(),
        repository: "Infranite/steve".into(),
        current_hub: "0.1.0".into(),
        current_web: "0.1.0".into(),
        managed: false,
        blocked_reason: None,
        client: reqwest::Client::new(),
        api_base: String::new(),
        web_base: String::new(),
        repo: PathBuf::new(),
        kind: None,
        launch: crate::upgrades::runner::Launch::Launchd,
    };
    // the test binary runs from a cargo target dir: source, or external when that lives elsewhere
    let install = u.install(Path::new(""));
    let reason = install.reason().unwrap();
    let status = u.status().unwrap();
    assert_eq!(status["install"]["kind"], install.kind);
    assert_eq!(status["capability"]["managed"], false);
    assert_eq!(status["capability"]["reason"], reason);
    let refused = u.request("0.2.0", "one", "agent:s", false).unwrap_err();
    assert_eq!(refused.to_string(), reason);
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
        web_base: String::new(),
        repo: PathBuf::new(),
        kind: None,
        launch: crate::upgrades::runner::Launch::Launchd,
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
                // A real streamed body leaves time to observe a durable, partial download.
                async move {
                    axum::body::Body::from_stream(futures::stream::unfold(
                        (b, 0),
                        |(b, at)| async move {
                            if at >= b.len() {
                                return None;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(650)).await;
                            let end = (at + b.len().div_ceil(2)).min(b.len());
                            Some((Ok::<_, std::io::Error>(b[at..end].to_vec()), (b, end)))
                        },
                    ))
                }
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
        web_base: String::new(),
        repo: PathBuf::new(),
        kind: None,
        launch: crate::upgrades::runner::Launch::Launchd,
    };
    let mut op = u.request("0.2.0", "download", "owner", true).unwrap();
    let id = op.id.clone();
    let observe = async {
        for _ in 0..60 {
            let receipt = u.operation(&id).unwrap();
            if let Some(p) = receipt.progress.and_then(|p| p.download)
                && p.received_bytes > 0
                && p.received_bytes < p.total_bytes
            {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        false
    };
    let (staged, partial) = tokio::join!(u.stage(&mut op), observe);
    staged.unwrap();
    assert!(
        partial,
        "the journal exposes measured progress before download completes"
    );
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
        web_base: String::new(),
        repo: PathBuf::new(),
        kind: None,
        launch: crate::upgrades::runner::Launch::Launchd,
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
    // macOS can report the explicit fallback exit before delivering SIGKILL to the fixture itself.
    // Neither status proves its workers died: observe the descendant before any parent cleanup.
    let expected_status =
        output.status.signal() == Some(libc::SIGKILL) || output.status.code() == Some(70);
    let pid = fs::read_to_string(&pid_file)
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|pid| *pid > 0);
    let mut descendant_dead = false;
    if let Some(pid) = pid {
        for _ in 0..40 {
            if unsafe { libc::kill(pid, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                descendant_dead = true;
                break;
            }
            // An exited orphan awaiting init reaping is also dead, not a surviving worker.
            if let Ok(out) = std::process::Command::new("/bin/ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
            {
                let state = String::from_utf8_lossy(&out.stdout);
                if out.status.success() && state.trim().starts_with('Z') {
                    descendant_dead = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    // Preserve the observations above. Cleanup must never make a broken group kill pass this test.
    if !expected_status || !descendant_dead {
        unsafe {
            libc::kill(-(child_pid as i32), libc::SIGKILL);
            if let Some(pid) = pid {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
    assert!(
        descendant_dead,
        "managed Hub descendant survived process-group shutdown (or its PID was unavailable): fixture={child_pid}, descendant={pid:?}, status={}; stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        expected_status,
        "orphan fixture pid={child_pid}, status={}, code={:?}; stdout={} stderr={}",
        output.status,
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn status_names_the_backup_a_failed_upgrade_recovers_from() {
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
        web_base: String::new(),
        repo: PathBuf::new(),
        kind: None,
        launch: crate::upgrades::runner::Launch::Launchd,
    };
    assert!(u.status().unwrap()["backup"].is_null());
    let op = u.request("0.2.0", "one", "owner", true).unwrap();
    let status = u.status().unwrap();
    assert!(
        status["backup"]["database"].is_null(),
        "no backup before the supervisor made one"
    );
    assert_eq!(status["backup"]["dir"], u.dir().to_string_lossy().as_ref());
    let saved = u.dir().join(format!("{}.sqlite3", op.id));
    fs::write(&saved, b"verified copy").unwrap();
    assert_eq!(
        u.status().unwrap()["backup"]["database"],
        saved.to_string_lossy().as_ref()
    );
}

/// The signing keychain's password in these homes (ops/sign-setup.sh's form).
const SIGNING_PASS: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A local_release install as `ops/local-release.py` leaves it: the signing identity and keychain files and
/// a checkout. The runner job is recorded, never started.
/// A Hub ready for a one-click local build. Only on macOS: elsewhere `runner::local_missing` names
/// the platform, so the tests that use this are ignored there.
fn local(home: &Path, repo: &Path, api_base: String) -> (Updates, Arc<Mutex<Vec<Operation>>>) {
    fs::create_dir_all(home.join("signing")).unwrap();
    fs::write(
        home.join("signing/identity.json"),
        r#"{"label":"dev.example.steve","hub_identifier":"h","keyhelper_identifier":"k","vault_service":"v"}"#,
    )
    .unwrap();
    fs::write(home.join("signing/steve-signing.keychain-db"), "").unwrap();
    fs::write(home.join("signing/keychain-pass"), SIGNING_PASS).unwrap();
    fs::create_dir_all(repo.join(".git")).unwrap();
    let started = Arc::new(Mutex::new(vec![]));
    let u = Updates {
        home: home.into(),
        repository: "test/good".into(),
        current_hub: "0.2.0".into(),
        current_web: "0.2.0".into(),
        managed: false,
        blocked_reason: None,
        client: reqwest::Client::new(),
        api_base: api_base.clone(),
        web_base: api_base,
        repo: repo.into(),
        kind: Some("local_release"),
        launch: runner::Launch::Fake(started.clone()),
    };
    (u, started)
}
use std::sync::{Arc, Mutex};

fn release_server_app(base: &str) -> axum::Router {
    let mut manifest = fixture();
    for v in [
        &mut manifest.version,
        &mut manifest.hub_version,
        &mut manifest.web_version,
    ] {
        *v = "0.3.0".into();
    }
    manifest.commit = "b".repeat(40);
    let stable = json!({"tag_name":"v0.3.0","html_url":"https://github.com/test/good/releases/tag/v0.3.0","body":"","draft":false,"prerelease":false,"assets":[{"name":"steve-release.json","browser_download_url":format!("{base}/manifest")}]});
    let draft = json!({"tag_name":"v0.4.0","html_url":"https://github.com/test/good/releases/tag/v0.4.0","body":"","draft":true,"prerelease":false,"assets":[]});
    axum::Router::new()
        .route(
            "/repos/test/good/releases/tags/v0.3.0",
            axum::routing::get(move || {
                let r = stable.clone();
                async move { axum::Json(r) }
            }),
        )
        .route(
            "/repos/test/good/releases/tags/v0.4.0",
            axum::routing::get(move || {
                let r = draft.clone();
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
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "a local_release upgrade needs macOS (codesign, launchd)"
)]
async fn local_release_upgrades_only_to_a_newer_stable_tag_one_at_a_time_outside_the_hub() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = release_server_app(&base);
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (u, started) = local(home.path(), repo.path(), base);
    fs::remove_dir_all(repo.path().join(".git")).unwrap();

    // An installed release remains upgradable without a source checkout.
    assert_eq!(u.execution_reason(), None);
    assert_eq!(
        u.status().unwrap()["capability"]["reason"],
        "local_release runner"
    );
    fs::remove_file(home.path().join("signing/keychain-pass")).unwrap();
    let reason = u.execution_reason().unwrap();
    assert!(reason.contains("signing/keychain-pass"), "{reason}");
    assert!(u.request("0.3.0", "k0", "owner", true).is_err());
    fs::write(home.path().join("signing/keychain-pass"), SIGNING_PASS).unwrap();

    // tags only: stable, strictly newer than what runs
    for refused in ["0.2.0", "0.1.9", "0.3.0-rc.1", "main", "v0.3.0+local"] {
        assert!(
            u.request(refused, &format!("k-{refused}"), "owner", true)
                .is_err(),
            "{refused}"
        );
    }
    let mut op = u.request("v0.3.0", "k1", "owner", true).unwrap();
    assert_eq!(op.phase, "staging");
    let spec = op.runner.clone().unwrap();
    assert_eq!(spec.kind, "release_bundle");
    assert!(spec.repo.is_empty() && spec.cargo.is_empty());
    assert!(spec.source.ends_with("/test/good.git"), "{}", spec.source);
    assert_eq!(
        spec.commit, None,
        "the commit comes from the release, not the request"
    );

    // one at a time
    let busy = u.request("0.3.0", "k2", "owner", true).unwrap_err();
    assert!(busy.to_string().contains("进行中"), "{busy}");

    // resolved from the configured repository's release, then handed to the runner job
    runner::launch(&u, &mut op).await.unwrap();
    let launched = started.lock().unwrap().clone();
    assert_eq!(launched.len(), 1);
    let spec = launched[0].runner.clone().unwrap();
    assert_eq!(spec.commit.as_deref(), Some("b".repeat(40).as_str()));
    assert!(spec.log.unwrap().contains("logs/release/upgrade-0.3.0-"));
    assert!(spec.launched_at.is_some());
    assert_eq!(u.operation(&op.id).unwrap().runner, launched[0].runner);
    // launching again (a retried approve) starts nothing new
    runner::launch(&u, &mut op).await.unwrap();
    assert_eq!(started.lock().unwrap().len(), 1);

    // the runner holds its lock: the operation is in progress however long the build takes
    let mut old = u.operation(&op.id).unwrap();
    old.updated_at = now() - 3_600_000;
    old.runner.as_mut().unwrap().launched_at = Some(now() - 3_600_000);
    u.save(&old).unwrap();
    let held = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(u.dir().join("runner.lock"))
        .unwrap();
    held.lock().unwrap();
    assert_eq!(u.status().unwrap()["operation"]["phase"], "staging");
    // the runner is gone without a result (a reboot): failed, saying the version was not switched
    drop(held);
    // a process another test forks keeps a copy of the descriptor (and the lock) until it execs
    let mut status = u.status().unwrap();
    for _ in 0..50 {
        if status["operation"]["phase"] == "failed" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        status = u.status().unwrap();
    }
    assert_eq!(status["operation"]["phase"], "failed");
    let error = status["operation"]["error"].as_str().unwrap();
    assert!(
        error.contains("没有切换") && error.contains("upgrade-0.3.0-"),
        "{error}"
    );
    assert_eq!(
        status["backup"]["dir"],
        home.path().join("backups").to_string_lossy().as_ref()
    );

    // a draft is no target, even when someone asks for it
    let mut draft = u.request("0.4.0", "k3", "owner", true).unwrap();
    let e = runner::launch(&u, &mut draft).await.unwrap_err();
    assert!(e.to_string().contains("stable"), "{e}");
    assert_eq!(started.lock().unwrap().len(), 1);
    server.abort();
}

#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "a local_release upgrade needs macOS (codesign, launchd)"
)]
fn a_release_script_or_local_release_running_refuses_a_local_upgrade() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let (u, _) = local(home.path(), repo.path(), String::new());
    let lock = home.path().join("releases/.release.lock");
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, "").unwrap();
    // another process holds the fcntl lock, the way ops/release.sh and local-release.py do
    let Ok(mut holder) = std::process::Command::new("python3")
        .args([
            "-c",
            "import fcntl,sys,time; f=open(sys.argv[1],'a'); fcntl.lockf(f,fcntl.LOCK_EX); print('held',flush=True); time.sleep(30)",
        ])
        .arg(&lock)
        .stdout(std::process::Stdio::piped())
        .spawn()
    else {
        eprintln!("no python3: skipped");
        return;
    };
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(holder.stdout.take().unwrap()),
        &mut line,
    )
    .unwrap();
    assert_eq!(line.trim(), "held");
    let refused = u
        .request("0.3.0", "while-releasing", "owner", true)
        .unwrap_err();
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert!(refused.to_string().contains(".release.lock"), "{refused}");
    assert!(u.request("0.3.0", "after", "owner", true).is_ok());
}

/// A python that has what local-release.py needs (the runner checks the same).
fn python() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    ["python3.13", "python3.12", "python3"]
        .into_iter()
        .flat_map(|n| std::env::split_paths(&path).map(move |d| d.join(n)))
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .find(|p| {
            std::process::Command::new(p)
                .args(["-I", "-c", "import tarfile; tarfile.data_filter"])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

fn sh(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A checkout whose tag v0.3.0 carries a fake ops/local-release.py: `build` makes the release directory,
/// `deploy` does what `$STEVE_HOME/fake-deploy` says (journal steps, exit code). Nothing real is built or
/// switched.
fn fake_checkout(home: &Path, repo: &Path) -> String {
    sh(repo, &["init", "-q"]);
    fs::create_dir_all(repo.join("ops")).unwrap();
    let script = r#"import json, os, pathlib, sys, time
HOME = pathlib.Path(__HOME__)
cmd = sys.argv[1]
if cmd == "build":
    commit, version = sys.argv[2], sys.argv[4]
    if "--keychain-pass-stdin" in sys.argv:
        (HOME / "got-pass").write_text(sys.stdin.readline())
    d = HOME / "releases" / f"{version}-local-{commit[:7]}"
    if (HOME / "fake-build-raced").exists():
        # another local-release.py took the lock first and is building this directory: refused untouched
        (d / "bin").mkdir(parents=True)
        print("12:00:00 ✗ another release is running", flush=True)
        sys.exit(1)
    print(f"12:00:00 build {version} from {commit} into {d}", flush=True)
    (d / "bin").mkdir(parents=True)
    (d / "web/dist").mkdir(parents=True)
    (d / "bin/steve-hub").write_text("")
    (d / "web/dist/index.html").write_text("")
    if (HOME / "fake-build-fails").exists():
        sys.exit(2)
    (d / "release.json").write_text(json.dumps({"version": version, "commit": commit}))
    sys.exit(0)
assert cmd == "deploy" and sys.argv[3] == "--operation"
op_path = HOME / "updates/operations" / f"{sys.argv[4]}.json"
mode = (HOME / "fake-deploy").read_text()
def phase(p, **kw):
    op = json.loads(op_path.read_text()); op.update(kw, phase=p, updated_at=int(time.time() * 1000)); op_path.write_text(json.dumps(op))
for p in ["draining", "stopping", "backing_up"]:
    phase(p)
phase("backing_up", backup=str(HOME / "backups/pre.sqlite3"))
if mode == "rollback":
    sys.exit(1)
phase("restarting", previous="old")
phase("verifying")
if mode == "committed":
    sys.exit(3)
phase("committed")
phase("succeeded", error=None)
"#
    .replace("__HOME__", &format!("{:?}", home.to_string_lossy()));
    fs::write(repo.join("ops/local-release.py"), script).unwrap();
    sh(repo, &["add", "ops"]);
    sh(repo, &["commit", "-qm", "release"]);
    sh(repo, &["tag", "v0.3.0"]);
    sh(repo, &["rev-parse", "HEAD"])
}

fn runner_op(u: &Updates, key: &str, commit: &str, python: &str) -> Operation {
    let mut op = u.request("0.3.0", key, "owner", true).unwrap();
    let spec = op.runner.as_mut().unwrap();
    // Compatibility fixture: only already-persisted source requests use the legacy runner.
    spec.kind = "local_release".into();
    spec.repo = u.repo.to_string_lossy().into_owned();
    spec.commit = Some(commit.into());
    spec.python = python.into();
    spec.source = spec.repo.clone();
    spec.log = Some(
        u.home
            .join("logs/release/test.log")
            .to_string_lossy()
            .into(),
    );
    spec.launched_at = Some(now());
    u.save(&op).unwrap();
    op
}

/// Signing an artifact passes only the installation password over stdin. No build or checkout
/// command runs, and a changed release identity is rejected before the installer gets that secret.
#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "local signing needs macOS")]
fn verified_bundle_preparation_needs_no_checkout_and_rechecks_trust() {
    let python = python().expect("Python is an installer prerequisite");
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let (u, _) = local(home.path(), repo.path(), String::new());
    fs::remove_dir_all(repo.path().join(".git")).unwrap();
    let mut op = u.request("0.3.0", "artifact", "owner", true).unwrap();
    let candidate = home
        .path()
        .join("releases")
        .join(format!("0.3.0-{}", op.id));
    fs::create_dir_all(candidate.join("ops")).unwrap();
    for name in ["schema_gate.py", "run.sh"] {
        fs::write(candidate.join("ops").join(name), "").unwrap();
    }
    fs::write(
        candidate.join("ops/local-release.py"),
        r#"import sys,pathlib,os
assert sys.argv[1]=='prepare-bundle'
assert sys.argv[3:]==['--version','0.3.0','--commit','b'*40,'--keychain-pass-stdin']
assert sys.stdin.readline().strip()=='0123456789abcdef'*4
pathlib.Path(os.environ['STEVE_HOME'],'prepared').write_text(sys.argv[2])
"#,
    )
    .unwrap();
    let mut spec = op.runner.clone().unwrap();
    spec.commit = Some("b".repeat(40));
    spec.python = python.clone();
    let source = spec.source.clone();
    let trust = runner::Trust {
        home: Some(home.path().into()),
        python_dirs: vec![Path::new(&python).parent().unwrap().into()],
        release: Box::new(move |_| Ok((source.clone(), "b".repeat(40)))),
    };
    runner::prepare_bundle(&u, &mut op, &spec, &candidate, &trust).unwrap();
    assert_eq!(
        fs::read_to_string(home.path().join("prepared")).unwrap(),
        candidate.to_string_lossy()
    );
    fs::remove_file(home.path().join("prepared")).unwrap();
    spec.commit = Some("a".repeat(40));
    assert!(runner::prepare_bundle(&u, &mut op, &spec, &candidate, &trust).is_err());
    assert!(!home.path().join("prepared").exists());
}

#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "a local_release upgrade needs macOS (codesign, launchd)"
)]
fn the_runner_builds_the_tagged_commit_with_its_own_scripts_and_reports_each_result() {
    let Some(python) = python() else {
        eprintln!("no python with tarfile.data_filter: skipped");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let (u, _) = local(home.path(), repo.path(), String::new());
    fs::remove_dir_all(repo.path().join(".git")).unwrap();
    let commit = fake_checkout(home.path(), repo.path());
    let candidate = home
        .path()
        .join(format!("releases/0.3.0-local-{}", &commit[..7]));
    // what the Hub would build itself: this home, this python's directory, the tag's commit
    let trusted = |commit: String| runner::Trust {
        home: Some(home.path().into()),
        python_dirs: vec![Path::new(&python).parent().unwrap().into()],
        release: Box::new({
            let source = repo.path().to_string_lossy().into_owned();
            move |v: &str| {
                assert_eq!(v, "0.3.0");
                Ok((source.clone(), commit.clone()))
            }
        }),
    };
    let trust = trusted(commit.clone());
    let finish = |op: &Operation| {
        let code = runner::main_with(home.path(), &op.id, &trust);
        (code, u.operation(&op.id).unwrap())
    };

    // upgraded: built from the tag, deployed with the journal, the backup named
    fs::write(home.path().join("fake-deploy"), "ok").unwrap();
    let op = runner_op(&u, "ok", &commit, &python);
    let (code, done) = finish(&op);
    assert_eq!(
        (code, done.phase.as_str(), done.error.as_deref()),
        (0, "succeeded", None)
    );
    assert_eq!(
        done.target.as_deref(),
        Some(candidate.to_string_lossy().as_ref())
    );
    assert_eq!(done.previous.as_deref(), Some("old"));
    assert!(done.backup.unwrap().ends_with("pre.sqlite3"));
    assert!(
        !u.dir().join(format!("runner/{}-ops", op.id)).exists(),
        "the tag's scripts are cleaned up"
    );
    // the signing password went to the build over its stdin, never as an argument
    assert_eq!(
        fs::read_to_string(home.path().join("got-pass")).unwrap(),
        format!("{SIGNING_PASS}\n")
    );
    assert_eq!(
        sh(repo.path(), &["status", "--porcelain"]),
        "",
        "the checkout is not touched"
    );

    // the password goes only to a build the Hub would make itself: a journal or a caller naming
    // another home, another python or another release gets nothing, and nothing is built
    fs::remove_dir_all(&candidate).unwrap();
    fs::remove_file(home.path().join("got-pass")).unwrap();
    let refused = |trust: runner::Trust, python: &str, key: &str| {
        let op = runner_op(&u, key, &commit, python);
        let code = runner::main_with(home.path(), &op.id, &trust);
        let done = u.operation(&op.id).unwrap();
        assert_eq!((code, done.phase.as_str()), (1, "failed"), "{key}");
        assert!(
            !home.path().join("got-pass").exists(),
            "{key}: no password handed out"
        );
        assert!(!candidate.exists(), "{key}: nothing built");
        done.error.unwrap()
    };
    let other_home = tempfile::tempdir().unwrap();
    let mut t = trusted(commit.clone());
    t.home = Some(other_home.path().into());
    // Installation trust is checked before reading a password; keep the refusal's context,
    // without tying this boundary test to translated password wording.
    let e = refused(t, &python, "other-home");
    assert!(e.contains(&*home.path().to_string_lossy()), "{e}");
    assert!(
        e.contains(
            &*fs::canonicalize(other_home.path())
                .unwrap()
                .to_string_lossy()
        ),
        "{e}"
    );
    let mut t = trusted(commit.clone());
    t.home = None;
    let e = refused(t, &python, "no-home");
    assert!(e.contains(&*home.path().to_string_lossy()), "{e}");
    // a python of the caller's: a script of the same name elsewhere is not one of the trusted dirs
    let elsewhere = tempfile::tempdir().unwrap();
    let fake = elsewhere.path().join("python3.13");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&python, &fake).unwrap();
    let e = refused(
        trusted(commit.clone()),
        &fake.to_string_lossy(),
        "other-python",
    );
    assert!(e.contains(&*fake.to_string_lossy()), "{e}");
    // the release manifest names another source or commit than the journal, or can't be read
    let mut t = trusted(commit.clone());
    t.release = Box::new(|_| Ok(("https://github.com/someone/else.git".into(), "d".repeat(40))));
    assert!(refused(t, &python, "other-release").contains("someone/else"));
    let mut t = trusted(commit.clone());
    t.release = Box::new(|_| anyhow::bail!("offline"));
    assert!(refused(t, &python, "unconfirmed").contains("offline"));
    // and the trusted build goes through again
    fs::write(home.path().join("fake-deploy"), "ok").unwrap();
    let op = runner_op(&u, "ok-again", &commit, &python);
    let (code, done) = finish(&op);
    assert_eq!((code, done.phase.as_str()), (0, "succeeded"));

    // failed and rolled back: the existing build is reused, the deploy's exit says what happened
    fs::write(home.path().join("fake-deploy"), "rollback").unwrap();
    let op = runner_op(&u, "rollback", &commit, &python);
    let (code, done) = finish(&op);
    assert_eq!((code, done.phase.as_str()), (0, "rolled_back"));
    assert!(done.error.unwrap().contains("test.log"));

    // failed after the switch: not undone, said so
    fs::write(home.path().join("fake-deploy"), "committed").unwrap();
    let op = runner_op(&u, "committed", &commit, &python);
    let (_, done) = finish(&op);
    assert_eq!(done.phase, "failed");
    assert!(done.error.unwrap().contains("升级已提交"));

    // a tag that names another commit than the release manifest: refused before anything is built
    fs::remove_dir_all(&candidate).unwrap();
    let op = runner_op(&u, "other-commit", &"c".repeat(40), &python);
    let (code, done) = finish(&op);
    assert_eq!((code, done.phase.as_str()), (1, "failed"));
    assert!(done.error.unwrap().contains("v0.3.0"));
    assert!(!home.path().join("releases/0.3.0-local-ccccccc").exists());

    // a failed build changes nothing and leaves no half-built directory behind
    fs::write(home.path().join("fake-build-fails"), "").unwrap();
    let op = runner_op(&u, "build-fails", &commit, &python);
    let (code, done) = finish(&op);
    assert_eq!((code, done.phase.as_str()), (1, "failed"));
    assert!(done.error.unwrap().contains("没有改动"));
    assert!(!candidate.exists());

    // a build another process started after the runner's check is not this run's to remove
    fs::remove_file(home.path().join("fake-build-fails")).unwrap();
    fs::write(home.path().join("fake-build-raced"), "").unwrap();
    let raced = runner_op(&u, "build-raced", &commit, &python);
    let (code, done) = finish(&raced);
    assert_eq!((code, done.phase.as_str()), (1, "failed"));
    assert!(
        candidate.join("bin").is_dir(),
        "the other build's directory is left alone"
    );

    // a runner started for an operation that is no longer staging does nothing
    let (code, again) = finish(&op);
    assert_eq!((code, again.phase.as_str()), (0, "failed"));
}

#[test]
fn the_page_notice_reads_only_an_available_check_for_this_hub() {
    let home = tempfile::tempdir().unwrap();
    let cfg = crate::config::Config::from_vars(|k: &str| match k {
        "HOME" | "STEVE_HOME" => Some(home.path().to_string_lossy().into_owned()),
        "STEVE_VERSION" => Some("0.2.0".into()),
        "STEVE_WEB_DIR" => Some(home.path().join("web").to_string_lossy().into_owned()),
        "STEVE_ADAPTER_CLAUDE" | "STEVE_ADAPTER_CODEX" => Some("unused".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(available(&cfg), None);
    let cached = |status: &str, hub: &str| CachedCheck {
        repository: "Infranite/steve".into(),
        current_hub: hub.into(),
        current_web: crate::web::web_version(&cfg),
        result: Check {
            status: status.into(),
            checked_at: now(),
            version: Some("0.3.0".into()),
            release_url: None,
            notes: None,
            error: None,
        },
    };
    atomic(
        &home.path().join("updates/check.json"),
        &cached("available", &cfg.version),
    )
    .unwrap();
    assert_eq!(available(&cfg).as_deref(), Some("0.3.0"));
    atomic(
        &home.path().join("updates/check.json"),
        &cached("up_to_date", &cfg.version),
    )
    .unwrap();
    assert_eq!(available(&cfg), None);
    // a check made by the Hub before an upgrade says nothing about this one
    atomic(
        &home.path().join("updates/check.json"),
        &cached("available", "0.0.1"),
    )
    .unwrap();
    assert_eq!(available(&cfg), None);
}

#[tokio::test]
async fn a_rate_limited_api_falls_back_to_the_github_release_redirect() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut manifest = fixture();
    manifest.assets.push(Asset {
        target: platform(),
        name: "bundle.tar.gz".into(),
        size: 4,
        sha256: "a".repeat(64),
    });
    let url = base.clone();
    let app = axum::Router::new()
        .route(
            "/repos/test/good/releases/latest",
            axum::routing::get(|| async { axum::http::StatusCode::FORBIDDEN }),
        )
        .route(
            "/test/good/releases/latest",
            axum::routing::get(move || {
                let url = url.clone();
                async move {
                    axum::response::Redirect::to(&format!("{url}/test/good/releases/tag/v0.2.0"))
                }
            }),
        )
        .route(
            "/test/good/releases/download/v0.2.0/steve-release.json",
            axum::routing::get(move || {
                let m = manifest.clone();
                async move { axum::Json(m) }
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = tempfile::tempdir().unwrap();
    let u = Updates {
        home: home.path().into(),
        repository: "test/good".into(),
        current_hub: "0.1.0".into(),
        current_web: "0.1.0".into(),
        managed: true,
        blocked_reason: None,
        client: reqwest::Client::new(),
        api_base: base.clone(),
        web_base: base,
        repo: PathBuf::new(),
        kind: None,
        launch: runner::Launch::Launchd,
    };
    let c = u.check().await.unwrap();
    assert_eq!(
        (c.status.as_str(), c.version.as_deref()),
        ("available", Some("0.2.0")),
        "{c:?}"
    );
    server.abort();
}

/// A fake interpreter: exits `code` for anything, the data_filter probe included.
#[cfg(unix)]
fn fake_python(dir: &Path, name: &str, code: i32) {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    fs::write(&p, format!("#!/bin/sh\nexit {code}\n")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn an_unusable_python_is_not_offered_for_an_artifact_upgrade() {
    let old = tempfile::tempdir().unwrap();
    let brew = tempfile::tempdir().unwrap();
    // An interpreter failing the installer prerequisites is never selected.
    fake_python(old.path(), "python3", 1);
    let path = Some(old.path().as_os_str().to_owned());
    assert_eq!(runner::find_python(None, path.clone(), &[]), None);
    // nor through STEVE_PYTHON
    assert_eq!(
        runner::find_python(Some(old.path().join("python3")), path.clone(), &[]),
        None
    );
    // Homebrew's python3.13 with no python3 link, outside the PATH: found and preferred
    fake_python(brew.path(), "python3.13", 0);
    assert_eq!(
        runner::find_python(None, path.clone(), &[brew.path().into()]),
        Some(brew.path().join("python3.13"))
    );
    // a versioned python on PATH comes before a bare python3 there
    fake_python(old.path(), "python3.12", 0);
    assert_eq!(
        runner::find_python(None, path, &[]),
        Some(old.path().join("python3.12"))
    );
}

#[cfg(unix)]
#[test]
fn the_runner_trusts_the_accounts_own_steve_home_not_the_environment() {
    let t = runner::Trust::installed();
    let home = t.home.expect("the account has a home directory");
    assert!(home.is_absolute() && home.ends_with(".steve"), "{home:?}");
    assert!(
        runner::PYTHON_DIRS
            .iter()
            .all(|d| t.python_dirs.contains(&PathBuf::from(d)))
    );
}

/// The supervisor asks the candidate's own `--check-config` before it drains, in the environment the
/// child would get; a candidate from before the check is not asked (it would start a Hub).
#[cfg(unix)]
#[tokio::test]
async fn the_supervisor_asks_the_candidate_about_this_environment_before_draining() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let cfg = crate::config::Config::from_vars(|k: &str| match k {
        "HOME" | "STEVE_HOME" => Some(home.path().to_string_lossy().into_owned()),
        "STEVE_HOST" => Some("127.0.0.1,::1".into()),
        "STEVE_PORT" => Some("9123".into()),
        "STEVE_ADAPTER_CLAUDE" | "STEVE_ADAPTER_CODEX" => Some("unused".into()),
        _ => None,
    })
    .unwrap();
    let bundle = |name: &str, check: bool| {
        let d = home.path().join(name);
        fs::create_dir_all(d.join("bin")).unwrap();
        let asked = d.join("asked");
        let info = if check { r#","check_config":true"# } else { "" };
        let hub = d.join("bin/steve-hub");
        fs::write(
            &hub,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  --build-info) echo '{{\"version\":\"0.2.0\"{info}}}' ;;\n  \
                 --check-config) echo \"$STEVE_HOST|$STEVE_PORT|$STEVE_HOME\" > '{}'\n    \
                 echo '{{\"ok\":false,\"errors\":[\"STEVE_X: refused\"],\"listen\":[]}}'; exit 2 ;;\n  \
                 *) exit 9 ;;\nesac\n",
                asked.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&hub, fs::Permissions::from_mode(0o755)).unwrap();
        (d, asked)
    };
    let (d, asked) = bundle("new", true);
    let e = supervisor::check_candidate(&cfg, &d, &fixture())
        .await
        .expect_err("refused");
    let e = format!("{e:#}");
    assert!(
        e.contains("STEVE_X: refused") && e.contains("没有排空"),
        "{e}"
    );
    assert_eq!(
        fs::read_to_string(&asked).unwrap().trim(),
        format!("127.0.0.1,::1|9123|{}", home.path().display())
    );

    let (d, asked) = bundle("old", false);
    supervisor::check_candidate(&cfg, &d, &fixture())
        .await
        .expect("an older candidate is not asked");
    assert!(!asked.exists());
}
