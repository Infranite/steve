//! Real stop signals cancel the pre-journal wait without consuming a migration transaction.
use serde_json::{Value, json};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use steve_node_wire::Identity;
use steve_runtime::testkit::eventually;
use steve_runtime::testkit::process::{Child, Command};

const V3: &str = include_str!("../../steve-node/src/journal/fixtures/v3.sql");

fn sqlite(path: &Path, sql: &str) -> String {
    let result = Command::new("python3").arg("-c")
        .arg("import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.executescript(sys.argv[2]); print(c.execute('PRAGMA user_version').fetchone()[0]); c.close()")
        .arg(path).arg(sql).output().unwrap();
    assert!(
        result.status.success(),
        "SQLite fixture: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().into()
}

fn json_file(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    new: PathBuf,
    journal: PathBuf,
}

impl Fixture {
    fn waiting() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let new = home.join("releases/new");
        let binary = new.join("bin/steve-node");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_steve-node"), &binary).unwrap();
        let build = Command::new(&binary).arg("--build-info").output().unwrap();
        assert!(build.status.success());
        let build: Value = serde_json::from_slice(&build.stdout).unwrap();
        let journal = home.join("node/local/node.sqlite3");
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        assert_eq!(
            sqlite(
                &journal,
                &format!(
                    "{V3}\nINSERT INTO meta(key,value) VALUES('journal_id',123),('incarnation',0); PRAGMA user_version=3;"
                )
            ),
            "3"
        );
        std::fs::create_dir_all(home.join("node")).unwrap();
        Identity::generate()
            .unwrap()
            .save(&home.join("node/identity.key"))
            .unwrap();
        json_file(
            &home.join("harness.json"),
            &json!({"adapters":{"claude-code":"/usr/bin/false","codex":"/usr/bin/false"},"node":"/usr/bin/false","claude_executable":"/usr/bin/false"}),
        );
        json_file(
            &home.join("node/node.json"),
            &json!({"hub_url":"http://127.0.0.1:1",
            "hub_id":"hub-test","node_id":"node-test","pin":null,"loopback":true,"hub_key":null,"name":"test"}),
        );
        json_file(&home.join("services/node.json"), &json!({"release":new}));
        // Missing old binary makes helper absence uncertain even on a release-metadata build.
        // A dev build cannot identify this operation and also waits rather than migrating.
        json_file(
            &home.join("updates/node/nup_signal.json"),
            &json!({
                "state":{"operation":"nup_signal","phase":"linked","version":build["version"],
                    "previous":"0.5.0","error":null,"updated":1},
                "request":{"operation":"nup_signal","build":build,"artifact":"fixture",
                    "size":1,"sha256":"a".repeat(64),"health_secs":1},
                "release":new,"previous_release":home.join("releases/old")
            }),
        );
        Self {
            _temp: temp,
            home,
            new,
            journal,
        }
    }

    fn run(&self) -> Child {
        Command::new(self.new.join("bin/steve-node"))
            .arg("run")
            .env("HOME", self._temp.path())
            .env("STEVE_HOME", &self.home)
            .env("STEVE_NODE_LOOPBACK_PLAINTEXT", "1")
            .env("STEVE_VAULT_KEYSTORE", "file")
            .env("STEVE_BACKUP", "off")
            .env("STEVE_NO_RESUME", "1")
            .env("STEVE_FORBID_REAL_HOSTS", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn locked(&self) -> bool {
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.home.join("node/local/node.lock"))
        else {
            return false;
        };
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if locked == 0 {
            return false;
        }
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EWOULDBLOCK)
        );
        true
    }
}

#[tokio::test]
async fn sigterm_after_the_helpers_last_read_leaves_v3_for_rollback() {
    let fixture = Fixture::waiting();
    // Deterministically model the final legacy read before it chose rollback, then the late
    // claim. The separate real-journal/receipt test exercises the Node's claim writer itself.
    let record_path = fixture.home.join("updates/node/nup_signal.json");
    let mut record: Value = serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    record["state"]["phase"] = "switching".into();
    json_file(&record_path, &record);
    let last_read: Value = serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    assert_ne!(last_read["state"]["phase"], "linked");
    record["state"]["phase"] = "linked".into();
    json_file(&record_path, &record);
    let mut child = fixture.run();
    eventually("new Node owns its HomeLock while waiting", || async {
        fixture.locked()
    })
    .await;
    assert!(child.try_wait().unwrap().is_none());
    assert_eq!(sqlite(&fixture.journal, "BEGIN IMMEDIATE; ROLLBACK;"), "3");
    assert!(child.try_wait().unwrap().is_none());
    // The old helper's rollback stop is a genuine OS signal, not a test cancellation flag.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    eventually("real SIGTERM cancels the pre-journal wait", || {
        std::future::ready(child.try_wait().unwrap().is_some())
    })
    .await;
    assert!(child.wait().unwrap().success());
    assert_eq!(sqlite(&fixture.journal, ""), "3");
    assert!(!fixture.locked(), "the stopped future released HomeLock");
    // Restart with the same linked record: it must recheck, never treat an old claim as receipt.
    let mut restarted = fixture.run();
    eventually("restarted Node waits behind the same gate", || async {
        fixture.locked()
    })
    .await;
    assert_eq!(sqlite(&fixture.journal, "BEGIN IMMEDIATE; ROLLBACK;"), "3");
    assert_eq!(
        unsafe { libc::kill(restarted.id() as i32, libc::SIGTERM) },
        0
    );
    eventually("restart stops safely", || {
        std::future::ready(restarted.try_wait().unwrap().is_some())
    })
    .await;
    assert!(restarted.wait().unwrap().success());
    assert_eq!(sqlite(&fixture.journal, ""), "3");
}
