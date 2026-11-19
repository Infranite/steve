//! In-process isolated Hub assembly; never invoke the binary or the production home.
use std::{
    collections::HashMap,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use steve_hub::{access, bus::Bus, config::Config};
use steve_store::{Store, access::NewClient};

struct Hub {
    _temp: tempfile::TempDir,
    cfg: Arc<Config>,
    store: Store,
    jobs: Vec<tokio::task::JoinHandle<()>>,
    _lock: std::fs::File,
}
fn config(home: &std::path::Path) -> (Arc<Config>, steve_hub::wire::Vars) {
    let vars: HashMap<String, String> = [
        ("HOME", home.display().to_string()),
        ("STEVE_HOME", home.display().to_string()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PORT", "1".into()),
        ("STEVE_PUBLIC_URL", "http://127.0.0.1:1".into()),
        ("STEVE_DEV_LOGIN", "1".into()),
        ("STEVE_NODE_MODE", "off".into()),
        ("STEVE_NO_RESUME", "1".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_WEB_DIR", home.join("no-web").display().to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v))
    .collect();
    let vars = Arc::new(vars);
    let get: steve_hub::wire::Vars = Arc::new(move |key| vars.get(key).cloned());
    (Arc::new(Config::from_vars(|key| get(key)).unwrap()), get)
}
impl Hub {
    fn start(long: bool) -> Self {
        steve_modules::net::forbid_in_this_process();
        let temp = tempfile::tempdir().unwrap();
        let home = if long {
            temp.path().join("long-home".repeat(10))
        } else {
            temp.path().join("hub")
        };
        let (cfg, vars) = config(&home);
        for dir in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let lock = steve_hub::upgrades::instance_lock(&cfg).unwrap();
        let wired = steve_hub::wire::wire_with(&cfg, &Bus::new(), vars).unwrap();
        let store = wired.modules.as_ref().unwrap().store.clone();
        store.ensure_workspace().unwrap();
        let jobs = wired.jobs.into_iter().map(tokio::spawn).collect();
        Self {
            _temp: temp,
            cfg,
            store,
            jobs,
            _lock: lock,
        }
    }
    async fn cli(&self, args: &[&str]) -> access::CliOutput {
        let cfg = self.cfg.clone();
        let args: Vec<_> = args.iter().map(|s| (*s).to_owned()).collect();
        tokio::task::spawn_blocking(move || access::command(&cfg, &args))
            .await
            .unwrap()
    }
    fn direct(&self, args: &[&str]) -> String {
        access::command_on(
            &self.store,
            &args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap()
    }
    async fn stop(mut self) {
        for job in &self.jobs {
            job.abort();
        }
        for job in self.jobs.drain(..) {
            let _ = job.await;
        }
    }
}
impl Drop for Hub {
    fn drop(&mut self) {
        for job in &self.jobs {
            job.abort();
        }
    }
}

#[tokio::test]
async fn commands_keep_the_exact_legacy_output_and_exit_codes_on_the_live_store() {
    let hub = Hub::start(false);
    let created = hub
        .cli(&[
            "token",
            "create",
            "--user",
            "管理用户",
            "--name",
            "测试😀",
            "--json",
        ])
        .await;
    assert_eq!(created.code, 0, "{}", created.stderr);
    assert_eq!(created.stderr, "");
    let created: serde_json::Value = serde_json::from_str(&created.stdout).unwrap();
    assert_eq!(created["user"], "管理用户");
    assert!(created["token"].as_str().unwrap().starts_with("stt_"));
    let id = created["id"].as_str().unwrap();
    let owner = hub.store.access().owner().unwrap().unwrap();
    let (_, client, _) = hub
        .store
        .access()
        .client_create(
            &owner.id,
            Some(id),
            &NewClient {
                kind: "cli",
                name: "test",
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    for args in [
        ["token", "list"].as_slice(),
        &["token", "list", "--json"],
        &["client", "list"],
        &["client", "list", "--json"],
    ] {
        let output = hub.cli(args).await;
        assert_eq!(output.code, 0, "{}", output.stderr);
        assert_eq!(output.stdout, hub.direct(args));
    }
    let revoke = hub.cli(&["client", "revoke", &client.id]).await;
    assert_eq!(revoke.stdout, format!("revoked {}\n", client.id));
    assert_eq!(revoke.code, 0);
    assert!(
        hub.store
            .access()
            .client(&client.id)
            .unwrap()
            .unwrap()
            .revoked
            .is_some()
    );
    let revoke = hub.cli(&["token", "revoke", id]).await;
    assert_eq!(
        revoke.stdout,
        format!("revoked {id} and the clients signed in with it\n")
    );
    assert_eq!(revoke.code, 0);
    let missing = hub.cli(&["token", "revoke", "missing"]).await;
    assert_eq!(
        (
            missing.code,
            missing.stdout.as_str(),
            missing.stderr.as_str()
        ),
        (2, "", "no live token missing\n")
    );
    let invalid = hub.cli(&["token", "create", "--days", "invalid"]).await;
    assert_eq!(
        (invalid.code, invalid.stderr.as_str()),
        (2, "invalid digit found in string\n")
    );
    let plain = hub.cli(&["token", "create", "--name", "plain"]).await;
    assert_eq!(plain.code, 0);
    assert!(plain.stdout.starts_with("user:  管理用户\ntoken: stt_"));
    assert!(
        plain
            .stdout
            .ends_with("\n(shown once; sign a client in with the user and this token)\n")
    );
    let unusual = hub
        .cli(&["token", "create", "--name", "--days", "1", "--json"])
        .await;
    assert_eq!(unusual.code, 0, "{}", unusual.stderr);
    let unusual: serde_json::Value = serde_json::from_str(&unusual.stdout).unwrap();
    let record = hub
        .store
        .access()
        .token(unusual["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(record.name, "--days");
    assert!(record.expires.is_some());
    hub.stop().await;
}

#[tokio::test]
async fn no_resume_hub_listens_with_private_permissions_and_removes_socket_on_shutdown() {
    let hub = Hub::start(false);
    assert!(hub.cfg.no_resume);
    let path = access::admin_socket_path(&hub.cfg.home).unwrap();
    assert_eq!(
        std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let path_copy = path.clone();
    let body = tokio::task::spawn_blocking(move || {
        let mut socket = UnixStream::connect(path_copy).unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        socket.write_all(b"GET /admin/v1/storage/status HTTP/1.0\r\nOrigin: https://unrelated.invalid\r\n\r\n").unwrap();
        let mut response = String::new(); socket.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.0 200"));
        serde_json::from_str::<serde_json::Value>(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    }).await.unwrap();
    assert_eq!(body["path"], hub.cfg.db_path().to_string_lossy().as_ref());
    assert!(body["schema_version"].as_i64().unwrap() > 0);
    assert!(body["file_size"].as_u64().unwrap() > 0);
    hub.stop().await;
    assert!(!path.exists());
}

#[tokio::test]
async fn long_home_uses_a_discoverable_private_fallback_and_cleans_the_pointer() {
    let hub = Hub::start(true);
    let pointer = hub.cfg.home.join("run/hub.sock.path");
    let path = access::admin_socket_path(&hub.cfg.home).unwrap();
    assert!(pointer.is_file());
    assert_ne!(path, hub.cfg.home.join("run/hub.sock"));
    assert!(path.as_os_str().as_encoded_bytes().len() < 104);
    assert_eq!(
        std::fs::read(&pointer).unwrap(),
        path.as_os_str().as_encoded_bytes()
    );
    assert_eq!(
        std::fs::metadata(&pointer).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(hub.cli(&["token", "list"]).await.code, 0);
    hub.stop().await;
    assert!(!path.exists());
    assert!(!pointer.exists());
}

#[tokio::test]
async fn token_list_finishes_during_back_to_back_in_process_write_transactions() {
    let hub = Hub::start(false);
    assert_eq!(
        hub.cli(&["token", "create", "--user", "owner"]).await.code,
        0
    );
    let stop = Arc::new(AtomicBool::new(false));
    let (ready, started) = mpsc::sync_channel(1);
    let writer = hub.store.clone();
    let done = stop.clone();
    let worker = std::thread::spawn(move || {
        let mut count = 0;
        while !done.load(Ordering::SeqCst) {
            writer
                .transaction(|_| {
                    writer.audit(
                        "test",
                        "admin.pressure",
                        "test",
                        &serde_json::json!({}),
                        steve_store::WS,
                    )?;
                    if count == 0 {
                        ready.send(()).unwrap();
                    }
                    count += 1;
                    Ok::<_, steve_store::StoreError>(())
                })
                .unwrap();
        }
        count
    });
    started.recv().unwrap();
    let listed = tokio::time::timeout(Duration::from_secs(2), hub.cli(&["token", "list"])).await;
    stop.store(true, Ordering::SeqCst);
    assert!(worker.join().unwrap() > 0);
    let listed = listed.expect("online list must not reopen/migrate or take the writer lock");
    assert_eq!(listed.code, 0, "{}", listed.stderr);
    assert!(listed.stdout.contains("cli"));
    hub.stop().await;
}

#[test]
fn an_offline_cli_keeps_legacy_direct_open_but_a_live_instance_lock_forbids_it() {
    let home = tempfile::tempdir().unwrap();
    let (cfg, _) = config(home.path());
    let lock = steve_hub::upgrades::instance_lock(&cfg).unwrap();
    let args = vec!["token".into(), "list".into()];
    let output = access::command(&cfg, &args);
    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr
            .contains("Hub 正在运行或正在升级，但管理 socket 没有响应")
    );
    assert!(output.stderr.contains("hub.sock"));
    assert!(
        !cfg.db_path().exists(),
        "a locked CLI must not create/open the database"
    );
    drop(lock);
    // The legacy CLI assumes the installation's data directory already exists.
    std::fs::create_dir_all(&cfg.data_dir).unwrap();
    let created = access::command(
        &cfg,
        &[
            "token".into(),
            "create".into(),
            "--user".into(),
            "offline".into(),
            "--json".into(),
        ],
    );
    assert_eq!(created.code, 0, "{}", created.stderr);
    let store = Store::open(cfg.db_path(), steve_store::NoPublisher).unwrap();
    assert_eq!(store.access().tokens(None).unwrap().len(), 1);
    let listed = access::command(&cfg, &args);
    assert_eq!(listed.code, 0);
    assert_eq!(listed.stdout, access::command_on(&store, &args).unwrap());
}

#[test]
fn a_connected_but_failed_request_never_falls_back_and_preserves_error_newlines() {
    let home = tempfile::tempdir().unwrap();
    let (cfg, _) = config(home.path());
    std::fs::create_dir_all(cfg.home.join("run")).unwrap();
    let path = access::admin_socket_path(&cfg.home).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
    let worker = std::thread::spawn(move || {
        let (mut connection, _) = listener.accept().unwrap();
        let mut input = [0; 1024];
        connection.read(&mut input).unwrap();
        let output = serde_json::to_vec(&access::CliOutput {
            stdout: "".into(),
            stderr: "exact error\n\n".into(),
            code: 2,
        })
        .unwrap();
        write!(
            connection,
            "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            output.len()
        )
        .unwrap();
        connection.write_all(&output).unwrap();
    });
    let output = access::command(&cfg, &["token".into(), "list".into()]);
    assert_eq!(output.code, 2);
    assert_eq!(output.stderr, "exact error\n\n");
    assert!(!cfg.db_path().exists());
    worker.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pinned_writer_blocks_create_but_not_online_list_and_create_runs_after_release() {
    let hub = Hub::start(false);
    assert_eq!(
        hub.cli(&["token", "create", "--user", "owner"]).await.code,
        0
    );
    let writer = hub.store.clone();
    let (ready, started) = mpsc::sync_channel(1);
    let (release, resume) = mpsc::sync_channel(1);
    let held = std::thread::spawn(move || {
        writer
            .transaction(|_| {
                ready.send(()).unwrap();
                resume.recv().unwrap();
                Ok::<_, steve_store::StoreError>(())
            })
            .unwrap();
    });
    started.recv().unwrap();
    let cfg = hub.cfg.clone();
    let create = tokio::task::spawn_blocking(move || {
        access::command(
            &cfg,
            &[
                "token".into(),
                "create".into(),
                "--name".into(),
                "queued".into(),
            ],
        )
    });
    let listed = tokio::time::timeout(Duration::from_secs(2), hub.cli(&["token", "list"])).await;
    let was_pending = !create.is_finished();
    release.send(()).unwrap();
    held.join().unwrap();
    assert_eq!(listed.unwrap().code, 0);
    assert!(
        was_pending,
        "create cannot commit while this Store's writer is held"
    );
    let created = tokio::time::timeout(Duration::from_secs(2), create)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(created.code, 0, "{}", created.stderr);
    assert!(
        hub.store
            .access()
            .tokens(None)
            .unwrap()
            .iter()
            .any(|t| t.name == "queued")
    );
    hub.stop().await;
}

#[tokio::test]
async fn bootstrap_owner_uses_the_cli_environment_instead_of_the_services_environment() {
    let hub = Hub::start(false);
    let path = access::admin_socket_path(&hub.cfg.home).unwrap();
    let caller = format!("{}-caller", std::env::var("USER").unwrap_or_default());
    let expected = caller.clone();
    let output = tokio::task::spawn_blocking(move || {
        let body = serde_json::to_vec(&serde_json::json!({
            "args": ["token", "create", "--json"], "bootstrap_user": caller,
        })).unwrap();
        let mut socket = UnixStream::connect(path).unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        write!(socket, "POST /admin/v1/access/tokens HTTP/1.0\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).unwrap();
        socket.write_all(&body).unwrap();
        let mut response = String::new(); socket.read_to_string(&mut response).unwrap();
        serde_json::from_str::<access::CliOutput>(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    }).await.unwrap();
    assert_eq!(output.code, 0, "{}", output.stderr);
    let token: serde_json::Value = serde_json::from_str(&output.stdout).unwrap();
    assert_eq!(token["user"], expected);
    assert_eq!(hub.store.access().owner().unwrap().unwrap().name, expected);
    hub.stop().await;
}

#[tokio::test]
async fn backup_command_uses_the_live_socket_and_management_inventory() {
    let hub = Hub::start(false);
    let result = hub.cli(&["backup", "--reason", "diagnostic"]).await;
    assert_eq!(result.code, 0, "{}", result.stderr);
    let manifest: steve_store_sqlite::backup::Manifest =
        serde_json::from_str(&result.stdout).unwrap();
    assert!(manifest.completed && manifest.database_path().is_file());
    assert!(matches!(
        manifest.reason,
        steve_store_sqlite::backup::Reason::Diagnostic
    ));
    let path = access::admin_socket_path(&hub.cfg.home).unwrap();
    let usage = tokio::task::spawn_blocking(move || {
        let mut socket = UnixStream::connect(path).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .write_all(b"GET /admin/v1/backup HTTP/1.0\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        serde_json::from_str::<serde_json::Value>(response.split_once("\r\n\r\n").unwrap().1)
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(usage["diagnostic"]["count"], 1);
    let bad = hub.cli(&["backup", "--reason", "daily"]).await;
    assert_eq!(bad.code, 2);
    assert_eq!(
        steve_store_sqlite::backup::list(&hub.cfg.home.join("backups"))
            .unwrap()
            .len(),
        1
    );
    hub.stop().await;
}
#[test]
fn offline_backup_takes_instance_lock_and_never_opens_the_legacy_store() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, _) = config(temp.path());
    std::fs::create_dir_all(&cfg.data_dir).unwrap();
    let c = rusqlite::Connection::open(cfg.db_path()).unwrap();
    c.execute_batch("CREATE TABLE schema_version(v INT); INSERT INTO schema_version VALUES(50); CREATE TABLE old_record(v); INSERT INTO old_record VALUES('old');").unwrap();
    drop(c);
    let before = std::fs::read(cfg.db_path()).unwrap();
    let modified = std::fs::metadata(cfg.db_path())
        .unwrap()
        .modified()
        .unwrap();
    let args = vec!["backup".into(), "--reason".into(), "pre-upgrade".into()];
    let result = access::command(&cfg, &args);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let manifest: steve_store_sqlite::backup::Manifest =
        serde_json::from_str(&result.stdout).unwrap();
    assert_eq!(manifest.schema_version, Some(50));
    assert_eq!(std::fs::read(cfg.db_path()).unwrap(), before);
    assert_eq!(
        std::fs::metadata(cfg.db_path())
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    let _lock = steve_hub::upgrades::instance_lock(&cfg).unwrap();
    let refused = access::command(&cfg, &args);
    assert_eq!(refused.code, 2);
    assert_eq!(
        steve_store_sqlite::backup::list(&cfg.home.join("backups"))
            .unwrap()
            .len(),
        1
    );
}
