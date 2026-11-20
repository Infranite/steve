use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use steve_hub::upgrades::{
    cli,
    signing::{self, Keychain, Rotated},
};
use steve_modules::vault::{
    keystore::{FileKeyStore, KeyStore, KeyStoreError, signing as passwords},
    secret::Kek,
};

struct Memory {
    keys: FileKeyStore,
    fail: Mutex<Option<String>>,
}
impl KeyStore for Memory {
    fn kind(&self) -> &'static str {
        "file"
    }
    fn get(&self, a: &str) -> Result<Option<Kek>, KeyStoreError> {
        self.keys.get(a)
    }
    fn set(&self, a: &str, k: &Kek) -> Result<(), KeyStoreError> {
        let mut fail = self.fail.lock().unwrap();
        if fail.as_deref() == Some(a) {
            fail.take();
            return Err(KeyStoreError::Denied);
        }
        self.keys.set(a, k)
    }
    fn delete(&self, a: &str) -> Result<(), KeyStoreError> {
        self.keys.delete(a)
    }
}
struct Chain {
    active: Mutex<String>,
    mode: &'static str,
    changes: AtomicUsize,
    store: Arc<Memory>,
}
impl Keychain for Chain {
    fn opens(&self, p: &str) -> bool {
        *self.active.lock().unwrap() == p
    }
    fn change(&self, old: &str, new: &str) -> anyhow::Result<()> {
        self.changes.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            passwords::get_next(self.store.as_ref())
                .unwrap()
                .unwrap()
                .as_str(),
            new,
            "next is durable before changing the chain"
        );
        if self.mode == "refuse" {
            anyhow::bail!("refused")
        }
        let mut active = self.active.lock().unwrap();
        assert_eq!(*active, old);
        *active = new.into();
        if self.mode == "changed-error" {
            anyhow::bail!("reported failure after changing")
        }
        Ok(())
    }
}

#[test]
fn sg44_rotation_recovers_pending_changes_and_retains_a_working_password() {
    for case in [
        "missing",
        "resume",
        "neither",
        "discard-stale",
        "old-wrong",
        "refuse",
        "changed-error",
        "commit-fault",
        "next-fault",
    ] {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("signing")).unwrap();
        let store = Arc::new(Memory {
            keys: FileKeyStore::open(home.path().join("keys")).unwrap(),
            fail: Mutex::new(None),
        });
        let old = "a".repeat(64);
        let next = "b".repeat(64);
        if case != "missing" {
            passwords::set(store.as_ref(), &old).unwrap();
        }
        if ["resume", "neither", "discard-stale"].contains(&case) {
            passwords::set_next(store.as_ref(), &next).unwrap();
        }
        let active = match case {
            "resume" => next.clone(),
            "neither" | "old-wrong" => "c".repeat(64),
            _ => old.clone(),
        };
        let chain = Chain {
            active: Mutex::new(active),
            mode: match case {
                "refuse" => "refuse",
                "changed-error" => "changed-error",
                _ => "normal",
            },
            changes: AtomicUsize::new(0),
            store: store.clone(),
        };
        if case == "commit-fault" {
            *store.fail.lock().unwrap() = Some(passwords::ACCOUNT.into());
        }
        if case == "next-fault" {
            *store.fail.lock().unwrap() = Some(passwords::NEXT_ACCOUNT.into());
        }
        let result = signing::rotate(home.path(), store.as_ref(), &chain);
        match case {
            "missing" => assert_eq!(
                result,
                Rotated::No("the key store holds no password".into())
            ),
            "neither" => assert_eq!(
                result,
                Rotated::No("neither the password nor the next one opens the keychain".into())
            ),
            "old-wrong" => assert_eq!(
                result,
                Rotated::No("the stored password does not open the keychain".into())
            ),
            "refuse" => assert_eq!(result, Rotated::No("refused".into())),
            "commit-fault" => {
                assert_eq!(result, Rotated::No("keychain access denied".into()));
                assert!(passwords::get_next(store.as_ref()).unwrap().is_some());
                assert_eq!(
                    signing::rotate(home.path(), store.as_ref(), &chain),
                    Rotated::Yes
                );
            }
            "next-fault" => assert_eq!(result, Rotated::No("keychain access denied".into())),
            _ => assert_eq!(result, Rotated::Yes),
        }
        if !["missing", "neither", "old-wrong"].contains(&case) {
            let stored = passwords::get(store.as_ref()).unwrap().unwrap();
            assert!(chain.opens(&stored));
            assert!(passwords::get_next(store.as_ref()).unwrap().is_none());
        }
        let expected = usize::from(
            ["discard-stale", "refuse", "changed-error", "commit-fault"].contains(&case),
        );
        assert_eq!(chain.changes.load(Ordering::SeqCst), expected, "{case}");
    }
}

#[test]
fn sg44_cli_rejects_ambiguous_input_and_an_isolated_non_account_home() {
    let home = tempfile::tempdir().unwrap();
    let h = home
        .path()
        .join("isolated-instance")
        .to_string_lossy()
        .into_owned();
    for args in [
        vec!["request", "--version", "1.2.3"],
        vec!["check", "--version", "v1.2.3"],
        vec!["watch", "--timeout", "NaN"],
        vec!["status", "--operation", "NOT-A-UUID"],
        vec!["status"],
    ] {
        let args = std::iter::once(h.clone())
            .chain(args.into_iter().map(String::from))
            .collect::<Vec<_>>();
        assert_eq!(cli::main(&args), 1);
    }
    assert!(
        !std::path::Path::new(&h).exists(),
        "rejected CLI requests never initialize an installation"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn sg44_background_migration_keeps_invalid_sources_and_honors_the_release_lock() {
    use steve_modules::vault::keystore::FileKeyStore;
    use steve_runtime::testkit::process::Command;
    let name = "sg44_background_migration_keeps_invalid_sources_and_honors_the_release_lock";
    if let Ok(case) = std::env::var("SG44_BACKGROUND") {
        assert_eq!(std::env::var("STEVE_VAULT_KEYSTORE").unwrap(), "file");
        let root = std::path::PathBuf::from(std::env::var_os("SG44_ROOT").unwrap());
        let home = root.join("instance");
        std::fs::create_dir_all(home.join("signing")).unwrap();
        std::fs::create_dir_all(home.join("releases")).unwrap();
        let original = "a".repeat(64);
        let mut holder = None;
        if case == "empty" {
            let store = FileKeyStore::open(home.join("vault-keys")).unwrap();
            passwords::set(&store, &original).unwrap();
        } else {
            std::fs::write(
                home.join(signing::FILE),
                if case == "held" {
                    original.as_str()
                } else {
                    "invalid"
                },
            )
            .unwrap();
        }
        if case == "held" {
            use std::io::{BufRead, BufReader};
            let mut child=Command::new("python3")
                .args(["-c","import fcntl,sys; f=open(sys.argv[1],'w'); fcntl.lockf(f,fcntl.LOCK_EX); print('ready',flush=True); sys.stdin.read()"])
                .arg(home.join("releases/.release.lock"))
                .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
            let mut line = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert_eq!(line.trim(), "ready");
            holder = Some(child);
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(signing::migrate_background(home.clone()));
        match case.as_str() {
            "held" => {
                assert!(!home.join("vault-keys").exists());
                assert_eq!(
                    std::fs::read_to_string(home.join(signing::FILE)).unwrap(),
                    original
                );
            }
            "malformed" => assert_eq!(
                std::fs::read_to_string(home.join(signing::FILE)).unwrap(),
                "invalid"
            ),
            _ => {
                let store = FileKeyStore::open(home.join("vault-keys")).unwrap();
                assert_eq!(passwords::get(&store).unwrap().unwrap().as_str(), original);
                assert!(passwords::get_next(&store).unwrap().is_none());
            }
        }
        if let Some(mut child) = holder {
            drop(child.stdin.take());
            assert!(child.wait().unwrap().success());
        }
        return;
    }
    for case in ["empty", "malformed", "held"] {
        let root = tempfile::tempdir().unwrap();
        let account = root.path().join("account");
        std::fs::create_dir(&account).unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env("SG44_BACKGROUND", case)
            .env("SG44_ROOT", root.path())
            .env("HOME", &account)
            .env("STEVE_HOME", root.path().join("instance"))
            .env("STEVE_VAULT_KEYSTORE", "file")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
