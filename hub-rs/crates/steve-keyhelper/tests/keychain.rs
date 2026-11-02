//! The scenario that broke production: a keychain item survives rebuilt callers only when the code that owns it
//! (its cdhash partition) stays the same. Everything happens in a temp dir: a throwaway signing keychain with a
//! throwaway self-signed "Steve Hub"-like identity (as ops/sign-setup.sh makes), a throwaway data keychain the helper
//! uses via STEVE_KEYHELPER_KEYCHAIN, two helper builds and several Hub-like callers. Never touches the login
//! keychain or the keychain search list.
//!
//! Run: `cargo test -p steve-keyhelper --test keychain -- --ignored --nocapture`

#![cfg(target_os = "macos")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const HUB_ID: &str = "io.github.infranite.steve.hub";
const HELPER_ID: &str = "io.github.infranite.steve.keyhelper";
const ACCOUNT: &str = "default/kek/v1";

fn sh(cmd: &str, args: &[&str]) -> Output {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{cmd}: {e}"));
    assert!(
        out.status.success(),
        "{cmd} {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn s(p: &Path) -> &str {
    p.to_str().expect("utf-8 path")
}

/// Deletes the throwaway keychains even when an assertion fails.
struct Keychains(Vec<String>);
impl Drop for Keychains {
    fn drop(&mut self) {
        for k in &self.0 {
            let _ = Command::new("/usr/bin/security")
                .args(["delete-keychain", k])
                .output();
        }
    }
}

struct Env {
    dir: PathBuf,
    sign_kc: PathBuf,
    data_kc: PathBuf,
    identity: String,
    pass: String,
}

impl Env {
    /// `codesign` `bin` with the throwaway identity; `extra` e.g. `-o runtime` (a different cdhash for the same bytes).
    fn sign(&self, bin: &Path, identifier: &str, extra: &[&str]) {
        let mut args = vec![
            "-f",
            "-s",
            &self.identity,
            "--keychain",
            s(&self.sign_kc),
            "-i",
            identifier,
        ];
        args.extend_from_slice(extra);
        args.push(s(bin));
        sh("/usr/bin/codesign", &args);
    }

    fn adhoc(&self, bin: &Path, identifier: &str) {
        sh(
            "/usr/bin/codesign",
            &["-f", "-s", "-", "-i", identifier, s(bin)],
        );
    }

    fn copy(&self, from: &Path, name: &str) -> PathBuf {
        let to = self.dir.join(name);
        std::fs::copy(from, &to).expect("copy");
        to
    }

    /// Runs `prog args`, with `stdin`, the data keychain in the env; (exit code, stdout, stderr).
    fn run(
        &self,
        prog: &Path,
        args: &[&str],
        stdin: Option<&str>,
        insecure: bool,
    ) -> (i32, String, String) {
        let mut c = Command::new(prog);
        c.args(args)
            .env("STEVE_KEYHELPER_KEYCHAIN", &self.data_kc)
            .env_remove("STEVE_KEYHELPER_INSECURE_TEST")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if insecure {
            c.env("STEVE_KEYHELPER_INSECURE_TEST", "1");
        }
        let mut child = c.spawn().expect("spawn");
        if let Some(v) = stdin {
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(v.as_bytes())
                .expect("write");
        }
        let out = child.wait_with_output().expect("wait");
        let r = (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        );
        println!(
            "  {} {} -> exit {}{}",
            prog.file_name().unwrap().to_string_lossy(),
            args.join(" "),
            r.0,
            if r.2.is_empty() {
                String::new()
            } else {
                format!("  [{}]", r.2.replace('\n', " | "))
            }
        );
        r
    }

    /// A Hub-like caller runs the helper directly (spawn, not exec, no shell), like `HelperStore`.
    fn via(
        &self,
        caller: &Path,
        helper: &Path,
        args: &[&str],
        stdin: Option<&str>,
    ) -> (i32, String, String) {
        let mut a = vec![s(helper)];
        a.extend_from_slice(args);
        self.run(caller, &a, stdin, false)
    }
}

fn cdhash(bin: &Path) -> String {
    let out = Command::new("/usr/bin/codesign")
        .args(["-dvvv", s(bin)])
        .output()
        .expect("codesign");
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    text.lines()
        .find_map(|l| l.strip_prefix("CDHash=").map(str::to_string))
        .unwrap_or_else(|| panic!("no CDHash in {text}"))
}

fn designated(bin: &Path) -> String {
    let out = Command::new("/usr/bin/codesign")
        .args(["-d", "-r-", s(bin)])
        .output()
        .expect("codesign");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    text.lines()
        .find(|l| l.starts_with("designated"))
        .unwrap_or_default()
        .to_string()
}

/// A Hub-like caller: spawns argv[1..] as a child with stdin/stdout/stderr inherited and exits with its code.
/// `build` only makes each build a different binary (a different cdhash), like a rebuilt Hub.
fn build_caller(dir: &Path, build: &str) -> PathBuf {
    let src = dir.join(format!("caller-{build}.rs"));
    std::fs::write(
        &src,
        format!(
            r#"const BUILD: &str = "{build}";
fn main() {{
    let a: Vec<String> = std::env::args().skip(1).collect();
    let st = std::process::Command::new(&a[0]).args(&a[1..]).status().expect("spawn helper");
    if std::env::var_os("CALLER_VERBOSE").is_some() {{ eprintln!("caller build {{BUILD}}"); }}
    std::process::exit(st.code().unwrap_or(1));
}}
"#
        ),
    )
    .expect("write caller");
    let bin = dir.join(format!("hub-{build}"));
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    sh(&rustc, &["-O", "-o", s(&bin), s(&src)]);
    bin
}

fn make_identity(dir: &Path, kc: &Path, pass: &str, name: &str) {
    let cnf = dir.join("cert.cnf");
    std::fs::write(
        &cnf,
        format!(
            "[req]\ndistinguished_name=dn\nx509_extensions=v3\nprompt=no\n[dn]\nCN={name}\n[v3]\nbasicConstraints=critical,CA:false\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=critical,codeSigning\n"
        ),
    )
    .expect("cnf");
    let (key, cert, p12) = (
        dir.join("key.pem"),
        dir.join("cert.pem"),
        dir.join("id.p12"),
    );
    sh(
        "openssl",
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            s(&key),
            "-out",
            s(&cert),
            "-days",
            "2",
            "-config",
            s(&cnf),
        ],
    );
    let p12pass = "p12-test-only";
    let legacy = Command::new("openssl")
        .args([
            "pkcs12",
            "-export",
            "-legacy",
            "-inkey",
            s(&key),
            "-in",
            s(&cert),
            "-out",
            s(&p12),
            "-passout",
            &format!("pass:{p12pass}"),
        ])
        .output()
        .expect("openssl");
    if !legacy.status.success() {
        sh(
            "openssl",
            &[
                "pkcs12",
                "-export",
                "-inkey",
                s(&key),
                "-in",
                s(&cert),
                "-out",
                s(&p12),
                "-passout",
                &format!("pass:{p12pass}"),
            ],
        );
    }
    sh("/usr/bin/security", &["create-keychain", "-p", pass, s(kc)]);
    sh("/usr/bin/security", &["unlock-keychain", "-p", pass, s(kc)]);
    sh("/usr/bin/security", &["set-keychain-settings", s(kc)]);
    sh(
        "/usr/bin/security",
        &[
            "import",
            s(&p12),
            "-k",
            s(kc),
            "-P",
            p12pass,
            "-T",
            "/usr/bin/codesign",
        ],
    );
    sh(
        "/usr/bin/security",
        &[
            "set-key-partition-list",
            "-S",
            "apple-tool:,apple:",
            "-s",
            "-k",
            pass,
            s(kc),
        ],
    );
    let _ = std::fs::remove_file(key);
}

#[test]
#[ignore = "creates throwaway keychains and a signing identity, runs codesign; run by hand on macOS"]
fn rebuilt_callers_keep_reading_what_the_helper_stored() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dir = tmp.path().canonicalize().expect("canon");
    let before = sh("/usr/bin/security", &["list-keychains", "-d", "user"]).stdout;
    let env = Env {
        sign_kc: dir.join("signing.keychain-db"),
        data_kc: dir.join("data.keychain-db"),
        identity: format!("Steve Keyhelper Test {}", std::process::id()),
        pass: "steve-keyhelper-test-only".into(),
        dir: dir.clone(),
    };
    let _cleanup = Keychains(vec![s(&env.sign_kc).into(), s(&env.data_kc).into()]);
    make_identity(&dir, &env.sign_kc, &env.pass, &env.identity);
    sh(
        "/usr/bin/security",
        &["create-keychain", "-p", &env.pass, s(&env.data_kc)],
    );
    sh(
        "/usr/bin/security",
        &["unlock-keychain", "-p", &env.pass, s(&env.data_kc)],
    );
    sh(
        "/usr/bin/security",
        &["set-keychain-settings", s(&env.data_kc)],
    );

    let built = PathBuf::from(env!("CARGO_BIN_EXE_steve-keyhelper"));
    // the installed helper, and a later "upgraded" one (same bytes, hardened-runtime flag: another cdhash)
    let h1 = env.copy(&built, "steve-keyhelper");
    env.sign(&h1, HELPER_ID, &[]);
    let h2 = env.copy(&built, "steve-keyhelper.new");
    env.sign(&h2, HELPER_ID, &["-o", "runtime"]);
    // two differently built Hubs, signed like release.sh signs bin/steve-hub
    let hub_a = build_caller(&dir, "a");
    let hub_b = build_caller(&dir, "b");
    env.sign(&hub_a, HUB_ID, &[]);
    env.sign(&hub_b, HUB_ID, &[]);
    // refused callers: ad-hoc (what an unsigned release is), right certificate but another identifier
    let hub_adhoc = env.copy(&hub_a, "hub-adhoc");
    env.adhoc(&hub_adhoc, HUB_ID);
    let other_id = env.copy(&hub_a, "other-tool");
    env.sign(&other_id, "io.github.infranite.other", &[]);

    println!("helper h1   {} cdhash {}", designated(&h1), cdhash(&h1));
    println!("helper h2   {} cdhash {}", designated(&h2), cdhash(&h2));
    println!(
        "hub-a       {} cdhash {}",
        designated(&hub_a),
        cdhash(&hub_a)
    );
    println!(
        "hub-b       {} cdhash {}",
        designated(&hub_b),
        cdhash(&hub_b)
    );
    println!(
        "hub-adhoc   {} cdhash {}",
        designated(&hub_adhoc),
        cdhash(&hub_adhoc)
    );
    assert_ne!(
        cdhash(&hub_a),
        cdhash(&hub_b),
        "the callers must be different builds"
    );
    assert_ne!(
        cdhash(&h1),
        cdhash(&h2),
        "the helpers must be different builds"
    );
    assert!(designated(&hub_a).contains("certificate leaf"));

    let key = data_encoding_b64(&[0x5a; 32]);
    let key2 = data_encoding_b64(&[0xa5; 32]);

    println!("-- hub-a stores the key through the helper; both Hub builds read it");
    assert_eq!(env.via(&hub_a, &h1, &["status"], None).0, 0);
    assert_eq!(
        env.via(&hub_a, &h1, &["get", ACCOUNT], None).0,
        12,
        "nothing yet"
    );
    assert_eq!(env.via(&hub_a, &h1, &["set", ACCOUNT], Some(&key)).0, 0);
    let (c, out, _) = env.via(&hub_b, &h1, &["get", ACCOUNT], None);
    assert_eq!(
        (c, out.as_str()),
        (0, key.as_str()),
        "a REBUILT Hub reads what another build stored"
    );
    let (c, out, _) = env.via(&hub_a, &h1, &["get", ACCOUNT], None);
    assert_eq!((c, out.as_str()), (0, key.as_str()));
    assert_eq!(
        env.via(&hub_b, &h1, &["set", ACCOUNT], Some(&key2)).0,
        0,
        "hub-b replaces"
    );
    let (c, out, _) = env.via(&hub_a, &h1, &["get", ACCOUNT], None);
    assert_eq!(
        (c, out.as_str()),
        (0, key2.as_str()),
        "hub-a reads what hub-b replaced"
    );
    assert_eq!(env.via(&hub_a, &h1, &["set", ACCOUNT], Some(&key)).0, 0);

    println!("-- refused callers get nothing");
    for (who, bin) in [("ad-hoc Hub", &hub_adhoc), ("other identifier", &other_id)] {
        let (c, out, _) = env.via(bin, &h1, &["get", ACCOUNT], None);
        assert_eq!(c, 11, "{who} refused");
        assert!(out.is_empty(), "{who}: no output");
    }
    let (c, out, _) = env.run(&h1, &["get", ACCOUNT], None, false);
    assert_eq!(
        (c, out.is_empty()),
        (11, true),
        "run directly by the test process: refused"
    );
    let (c, _, _) = env.run(&h1, &["get", ACCOUNT], None, true);
    assert_eq!(
        c, 11,
        "STEVE_KEYHELPER_INSECURE_TEST does not weaken a signed helper"
    );
    assert_eq!(
        env.via(&hub_a, &h1, &["set", "x/kek/v1"], Some("not base64"))
            .0,
        13,
        "malformed input"
    );

    println!("-- an unsigned (ad-hoc) helper refuses everything without the test switch");
    let h_adhoc = env.copy(&built, "steve-keyhelper-adhoc");
    env.adhoc(&h_adhoc, HELPER_ID);
    assert_eq!(env.via(&hub_a, &h_adhoc, &["status"], None).0, 11);
    assert_eq!(
        env.run(&h_adhoc, &["status"], None, true).0,
        0,
        "INSECURE_TEST=1: status works"
    );

    // A throwaway keychain file applies no `partition_id` to new items; the login keychain does
    // (`cdhash:<creator>` for code without an Apple Team ID). Checked by hand in the login keychain on
    // 2026-11-02 with a test entry: the creating binary reads it again, a rebuilt binary with the same
    // certificate and identifier gets -25293. So here an upgraded helper may already read the old item,
    // and the migration may be a no-op; both cases must work.
    println!(
        "-- helper upgrade: h2 migrates the entry from h1 (a no-op where no partition applies)"
    );
    let partitioned = env.via(&hub_a, &h2, &["get", ACCOUNT], None).0 == 11;
    println!("   partition applied in this keychain: {partitioned}");
    assert_eq!(
        env.run(&h2, &["migrate-from", s(&h_adhoc)], None, false).0,
        11,
        "an old helper with another signature is refused"
    );
    let (c, _, log) = env.run(&h2, &["migrate-from", s(&h1)], None, false);
    assert_eq!(c, 0, "{log}");
    assert!(
        log.contains("default/kek/v1: migrated to the new helper"),
        "{log}"
    );
    for hub in [&hub_a, &hub_b] {
        let (c, out, _) = env.via(hub, &h2, &["get", ACCOUNT], None);
        assert_eq!(
            (c, out.as_str()),
            (0, key.as_str()),
            "both Hub builds read through the new helper"
        );
    }
    // run again: in the login keychain the old helper is now refused (unchanged); without partitions it moves again
    let (c, _, log) = env.run(&h2, &["migrate-from", s(&h1)], None, false);
    assert_eq!(c, 0, "{log}");
    let again = if partitioned {
        "already readable by the new helper, unchanged"
    } else {
        "migrated to the new helper"
    };
    assert!(log.contains(again), "{log}");

    println!("-- delete, locked keychain");
    assert_eq!(env.via(&hub_b, &h2, &["delete", ACCOUNT], None).0, 0);
    assert_eq!(
        env.via(&hub_b, &h2, &["delete", ACCOUNT], None).0,
        0,
        "deleting nothing is fine"
    );
    assert_eq!(env.via(&hub_b, &h2, &["get", ACCOUNT], None).0, 12);
    assert_eq!(env.via(&hub_b, &h2, &["set", ACCOUNT], Some(&key)).0, 0);
    sh("/usr/bin/security", &["lock-keychain", s(&env.data_kc)]);
    let (c, out, _) = env.via(&hub_b, &h2, &["status"], None);
    assert_eq!((c, out.as_str()), (10, "locked"));
    assert_eq!(env.via(&hub_b, &h2, &["get", ACCOUNT], None).0, 10);

    drop(_cleanup);
    assert!(
        !env.sign_kc.exists() && !env.data_kc.exists(),
        "throwaway keychains deleted"
    );
    let after = sh("/usr/bin/security", &["list-keychains", "-d", "user"]).stdout;
    assert_eq!(before, after, "search list unchanged");
}

fn data_encoding_b64(b: &[u8]) -> String {
    data_encoding::BASE64.encode(b)
}
