//! Real CLI, private fake release, cleared environment and inert service managers.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Output, Stdio},
};
use steve_runtime::testkit::process::Command;

fn executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A 1.2.3 installer under `root` whose programs cannot initialize a keychain or start a service;
/// returns its `steve`.
fn installer(root: &Path) -> PathBuf {
    let package = root.join("installer");
    for path in [
        package.join("bin"),
        package.join("ops"),
        package.join("web/dist"),
    ] {
        fs::create_dir_all(path).unwrap();
    }
    let cli = package.join("bin/steve");
    fs::copy(env!("CARGO_BIN_EXE_steve"), &cli).unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
    let manifest = json!({
        "format_version":1, "version":"1.2.3", "commit":"a".repeat(40),
        "hub_version":"1.2.3", "web_version":"1.2.3", "restic_version":"0.19.1", "schema":{"target":54}
    });
    for path in [
        package.join("release.json"),
        package.join("web/dist/release.json"),
    ] {
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    }
    fs::write(
        package.join("web/dist/index.html"),
        "<html><head><meta name=\"steve-web-version\" content=\"1.2.3\"></head></html>",
    )
    .unwrap();
    let info = json!({
        "version":"1.2.3", "commit":"a".repeat(40), "schema":54,
        "target":steve_install::build_info()["target"]
    });
    for role in ["hub", "node"] {
        executable(
            &package.join(format!("bin/steve-{role}")),
            &format!(
                "#!/bin/sh\nif [ \"$#\" -eq 1 ] && [ \"$1\" = '--build-info' ]; then printf '%s\\n' '{info}'; exit 0; fi\nexit 95\n"
            ),
        );
    }
    // These cannot initialize a keychain or start a service even if cancellation regresses.
    for relative in ["bin/steve-keyhelper", "bin/restic"] {
        executable(&package.join(relative), "#!/bin/sh\nexit 95\n");
    }
    fs::write(package.join("bin/restic.LICENSE"), "BSD-2-Clause\n").unwrap();
    cli
}

/// A service manager in `bin` that knows of no service (or of a running one) and refuses, and
/// records, anything but being asked.
fn manager(bin: &Path, calls: &Path, running: bool) {
    let calls = steve_install::common::shell_quote(calls.to_str().unwrap()).unwrap();
    let (name, permitted, absent, present) = if cfg!(target_os = "macos") {
        (
            "launchctl",
            "[ \"$1\" = print ]",
            "printf '%s\\n' 'Could not find service' >&2; exit 113",
            "printf '%s\\n' 'state = running' 'pid = 4242'",
        )
    } else {
        (
            "systemctl",
            "[ \"$1\" = --user ] && [ \"$2\" = show ]",
            "printf '%s\\n' 'LoadState=not-found' 'ActiveState=inactive' 'MainPID=0' 'ExecMainStatus=0'",
            "printf '%s\\n' 'LoadState=loaded' 'ActiveState=active' 'MainPID=4242' 'ExecMainStatus=0'",
        )
    };
    let output = if running { present } else { absent };
    executable(
        &bin.join(name),
        &format!(
            "#!/bin/sh\n{permitted} || {{ printf 'changed\\n' >> {calls}; exit 97; }}\nprintf 'read\\n' >> {calls}\n{output}\n"
        ),
    );
}

fn private(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn hash(path: &Path) -> String {
    hex::encode(Sha256::digest(fs::read(path).unwrap()))
}
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    account: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    cli: PathBuf,
}
impl Fixture {
    fn new(role: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let account = root.join("account");
        let bin = root.join("bin");
        fs::create_dir_all(&account).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let cli = installer(&root);
        manager(&bin, &root.join("manager-calls"), false);
        let home = account.join("installation");
        fs::create_dir_all(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let this = Self {
            _dir: dir,
            root,
            account,
            home,
            bin,
            cli,
        };
        let info = json!({"version":"1.2.3","commit":"a".repeat(40),"schema":54,"target":steve_install::build_info()["target"]});
        let calls =
            steve_install::common::shell_quote(this.root.join("calls").to_str().unwrap()).unwrap();
        for component in ["hub", "node"] {
            executable(
                &this.root.join(format!("installer/bin/steve-{component}")),
                &format!(
                    r#"#!/bin/sh
printf '%s|%s\n' '{component}' "$*" >> {calls}
case "$1" in
 --build-info) printf '%s\n' '{info}' ;;
 --check-config) printf '%s\n' '{{"ok":true,"errors":[],"warnings":[]}}' ;;
 --access) printf '%s\n' '{{"user":"fixture-owner","token":"fixture-token"}}' ;;
 adapters|--adapters) printf '%s\n' '{{"adapters":[],"npm":false}}' ;;
 pair) /bin/mkdir -p "$STEVE_HOME/node"; printf '{{"hub_url":"%s","pin":null}}' "$2" > "$STEVE_HOME/node/node.json" ;;
 status) exit 0 ;;
 *) exit 95 ;;
esac
"#
                ),
            );
        }
        this.receipt(role, json!({}));
        this
    }
    fn receipt(&self, role: &str, extra: Value) {
        let mut record = json!({"format":1,"component":role,"home":self.home,"version":"1.2.3","commit":"a".repeat(40),"files":{},"steps":[]});
        for (key, value) in extra.as_object().unwrap() {
            record[key] = value.clone();
        }
        private(
            &self.home.join("setup.json"),
            &serde_json::to_vec(&record).unwrap(),
        );
    }
    fn invoke(&self, role: &str, args: &[&str]) -> Output {
        let mut command = Command::new(&self.cli);
        command
            .args(["setup", role, "--home"])
            .arg(&self.home)
            .args(args);
        if cfg!(target_os = "macos") && role == "node" {
            command.args(["--label", "test.fixture.ts5-node"]);
        }
        command
            .env_clear()
            .env("HOME", &self.account)
            .env("PATH", &self.bin)
            .env("TMPDIR", &self.root)
            .env("STEVE_LANG", "en")
            .env("STEVE_SETUP_PROGRESS", "1")
            .env("STEVE_PAIR_CODE", "fixture-code")
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
    fn log(&self) -> String {
        fs::read_to_string(self.root.join("calls")).unwrap_or_default()
    }
}
fn phases(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("steve-setup-progress ").map(str::to_owned))
        .collect()
}
fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn four_pairing_guards_stop_before_adapter_install_or_service_management() {
    for (scenario, message) in [
        (
            "identity_foreign",
            "The existing Node identity does not belong to this setup",
        ),
        (
            "pairing_sent_elsewhere",
            "A pairing request went to another Hub; the identity is not changed automatically",
        ),
        (
            "pairing_foreign",
            "The existing pairing does not belong to this setup",
        ),
        (
            "pairing_other_hub",
            "The existing pairing points to another Hub",
        ),
    ] {
        let f = Fixture::new("node");
        match scenario {
            "identity_foreign" => private(&f.home.join("node/identity.key"), b"fixture identity"),
            "pairing_sent_elsewhere" => {
                f.receipt(
                    "node",
                    json!({"pair_started":true,"hub_url":"https://old.example"}),
                );
                private(&f.home.join("node/identity.key"), b"fixture identity");
            }
            "pairing_foreign" => private(
                &f.home.join("node/node.json"),
                br#"{"hub_url":"https://hub.example","pin":null}"#,
            ),
            _ => {
                f.receipt(
                    "node",
                    json!({"pair_started":true,"hub_url":"https://old.example"}),
                );
                private(
                    &f.home.join("node/node.json"),
                    br#"{"hub_url":"https://old.example","pin":null}"#,
                );
            }
        }
        let out = f.invoke("node", &["--hub", "https://hub.example"]);
        let text = said(&out);
        assert!(!out.status.success(), "{scenario}: {text}");
        assert!(text.contains(message), "{scenario}: {text}");
        assert_eq!(phases(&out), ["install"], "{scenario}: {text}");
        // The wizard can query adapter capabilities before pairing; it must not install them.
        assert!(!f.log().contains("adapters install"), "{}", f.log());
        assert!(
            !f.log()
                .lines()
                .any(|l| l.starts_with("node|pair ") || l == "node|status")
        );
        assert!(
            !f.root.join("manager-calls").exists(),
            "{scenario} touched the service manager"
        );
    }
}

#[test]
fn pin_normalization_and_same_hub_only_resume_table() {
    let saved = format!("sha256:{}", "aB".repeat(32));
    for scenario in ["explicit", "same-hub", "other-hub"] {
        let f = Fixture::new("node");
        if cfg!(target_os = "macos") {
            // An incomplete signing stub stops before invoking real macOS signing/keychain tools.
            private(&f.home.join("signing/identity.json"), b"{}");
        }
        let old = if scenario == "other-hub" {
            "https://old.example"
        } else {
            "https://hub.example"
        };
        f.receipt("node",json!({"pair_started":true,"hub_url":old,"pin":if scenario=="explicit"{Value::Null}else{json!(saved.to_lowercase())}}));
        let args = if scenario == "explicit" {
            vec!["--hub", "https://hub.example", "--pin", saved.as_str()]
        } else {
            vec!["--hub", "https://hub.example"]
        };
        let out = f.invoke("node", &args);
        assert_eq!(
            phases(&out),
            ["install", "pair", "adapters", "start"],
            "{}",
            said(&out)
        );
        let log = f.log();
        let pair = log.lines().find(|l| l.starts_with("node|pair ")).unwrap();
        let expected = if scenario == "other-hub" {
            "node|pair https://hub.example".to_string()
        } else {
            format!(
                "node|pair https://hub.example --pin {}",
                saved.to_lowercase()
            )
        };
        assert_eq!(pair, expected);
        let record: Value =
            serde_json::from_slice(&fs::read(f.home.join("setup.json")).unwrap()).unwrap();
        assert_eq!(
            record["pin"],
            if scenario == "other-hub" {
                Value::Null
            } else {
                json!(saved.to_lowercase())
            }
        );
        assert!(!log.contains("fixture-code"));
    }
}

#[test]
fn hub_checks_before_issuing_a_token_and_resume_does_not_issue_twice() {
    let f = Fixture::new("hub");
    // Resume after signing avoids invoking real platform signing/keychain programs. The first
    // token has not been issued yet, so the real setup must still check and issue it once.
    private(&f.home.join("bin/steve-keyhelper"), b"fixture helper");
    private(&f.home.join("signing/identity.json"), b"{}");
    let helper = hash(&f.home.join("bin/steve-keyhelper"));
    let identity = hash(&f.home.join("signing/identity.json"));
    f.receipt("hub",json!({"steps":["keychain"],"keychain_started":true,
        "signed_hub_sha256":hash(&f.root.join("installer/bin/steve-hub")),"helper_sha256":helper,"identity_sha256":identity,
        "environment":{"STEVE_HOST":"127.0.0.1","STEVE_PORT":"18990","STEVE_PUBLIC_URL":"http://127.0.0.1:18991","STEVE_WEB_PORT":"18991","STEVE_VAULT_KEYSTORE":"file"}}));
    for attempt in 0..2 {
        let out = f.invoke("hub", &[]);
        let text = said(&out);
        assert!(phases(&out).contains(&"start".to_string()), "{text}");
        let log = f.log();
        let check = log.lines().position(|l| l == "hub|--check-config").unwrap();
        let token = log
            .lines()
            .position(|l| l.starts_with("hub|--access token create "))
            .unwrap();
        assert!(check < token, "{log}");
        assert_eq!(
            log.lines()
                .filter(|l| l.starts_with("hub|--access token create "))
                .count(),
            1,
            "attempt {attempt}: {log}"
        );
        assert_eq!(
            log.lines().filter(|l| *l == "hub|--check-config").count(),
            attempt + 1,
            "{log}"
        );
        assert_eq!(
            fs::read_to_string(f.home.join("secrets/owner.token")).unwrap(),
            "user=fixture-owner\ntoken=fixture-token\n"
        );
    }
}
