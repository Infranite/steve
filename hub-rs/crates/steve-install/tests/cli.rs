//! Run the actual native CLI with no interpreter or package-manager search path.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn invoke(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_steve"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", home.join("no-executables"))
        .env("STEVE_LANG", "zh-CN")
        .output()
        .unwrap()
}

/// The first line of `steve --help` under these language settings.
fn help_in(args: &[&str], env: &[(&str, &str)]) -> String {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_steve"))
        .args(args)
        .arg("--help")
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", home.path().join("no-executables"))
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn the_language_is_the_flag_then_steve_lang_then_the_locale() {
    let zh = help_in(&[], &[("STEVE_LANG", "zh-CN")]);
    let en = help_in(&[], &[("STEVE_LANG", "en")]);
    assert_ne!(zh, en);
    assert!(
        zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "{zh}"
    );
    assert!(
        !en.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "{en}"
    );
    // --lang wins over every variable, and any tag of a language chooses it
    assert_eq!(help_in(&["--lang", "en"], &[("STEVE_LANG", "zh-CN")]), en);
    assert_eq!(
        help_in(
            &["--lang", "zh-Hans"],
            &[("STEVE_LANG", "en"), ("LANG", "en_US.UTF-8")]
        ),
        zh
    );
    // STEVE_LANG, then LC_ALL, LC_MESSAGES, LANG: the first that names one of ours
    assert_eq!(
        help_in(&[], &[("STEVE_LANG", "en"), ("LC_ALL", "zh_CN.UTF-8")]),
        en
    );
    assert_eq!(
        help_in(&[], &[("LC_ALL", "zh_CN.UTF-8"), ("LANG", "en_US.UTF-8")]),
        zh
    );
    assert_eq!(
        help_in(
            &[],
            &[
                ("LC_ALL", "C"),
                ("LC_MESSAGES", "en_GB"),
                ("LANG", "zh_CN.UTF-8")
            ]
        ),
        en
    );
    assert_eq!(help_in(&[], &[("LANG", "zh_TW.UTF-8")]), zh);
    // a language Steve has no texts in is an error, not a guess
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_steve"))
        .args(["--lang", "fr", "--help"])
        .env_clear()
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--lang fr"));
}

#[test]
fn read_only_cli_commands_work_without_python_or_an_existing_home() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("must-not-be-created");
    for args in [&["--help"][..], &["--version"], &["--build-info"]] {
        let out = invoke(&home, args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!home.exists());
        let text = String::from_utf8(out.stdout).unwrap();
        if args == ["--help"] {
            for command in ["setup", "--reconfigure", "hub", "node", "upgrade"] {
                assert!(text.contains(command));
            }
        }
        if args == ["--version"] {
            assert!(text.starts_with("steve CLI/package "));
        }
        if args == ["--build-info"] {
            let info: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(info["cli_protocol"], 2);
            assert_eq!(info["native_install"], true);
        }
    }
    let bytes = fs::read(env!("CARGO_BIN_EXE_steve")).unwrap();
    assert!(bytes.starts_with(b"\x7fELF") || bytes.starts_with(b"\xcf\xfa\xed\xfe"));
}

#[test]
fn rejected_arguments_never_echo_a_mistaken_secret() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("absent");
    let sentinel = "fixture-secret-never-log-this";
    for args in [
        vec!["setup", "hub", "--client-secret", sentinel],
        vec!["setup", "--reconfigure"],
        vec!["setup", "node", "--reconfigure"],
        vec!["setup", "--reconfigure", "--reconfigure"],
        vec![sentinel],
    ] {
        let out = invoke(&home, &args);
        assert!(!out.status.success());
        assert!(!String::from_utf8_lossy(&out.stdout).contains(sentinel));
        assert!(!String::from_utf8_lossy(&out.stderr).contains(sentinel));
        assert!(!home.exists());
    }
}

#[test]
fn an_old_hub_is_not_started_with_an_unrecognized_upgrade_command() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("installation");
    let release = home.join("releases/previous");
    fs::create_dir_all(release.join("bin")).unwrap();
    let unexpected = temp.path().join("unexpected-command");
    let quoted = steve_install::common::shell_quote(unexpected.to_str().unwrap()).unwrap();
    let hub = release.join("bin/steve-hub");
    fs::write(&hub, format!("#!/bin/sh\nif [ \"$1\" = --build-info ]; then\nprintf '%s\\n' '{{\"version\":\"1.0.0\",\"upgrade_runner\":{{\"journal\":1}}}}'\nexit 0\nfi\nprintf unexpected > {quoted}\nexit 97\n")).unwrap();
    fs::set_permissions(&hub, fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(&release, home.join("current")).unwrap();
    let out = invoke(
        &temp.path().join("unused-home"),
        &["upgrade", "status", "--home", home.to_str().unwrap()],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("设置页"));
    assert!(!unexpected.exists());
}

fn metadata(path: &Path, value: &serde_json::Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn adapter_release(home: &Path, role: &str, name: &str, calls: &Path) -> std::path::PathBuf {
    let release = home.join("releases").join(name);
    fs::create_dir_all(release.join("bin")).unwrap();
    metadata(
        &release.join("release.json"),
        &serde_json::json!({
            "format_version": 1, "version": name, "commit": "a".repeat(40)
        }),
    );
    let quoted = steve_install::common::shell_quote(calls.to_str().unwrap()).unwrap();
    let program = release.join(format!("bin/steve-{role}"));
    fs::write(&program, format!("#!/bin/sh\nprintf '%s|%s|%s|%s|%s\\n' {role} {name} \"$STEVE_HOME\" \"$STEVE_LANG\" \"$*\" >> {quoted}\n[ \"$2\" = status ] && exit 3\nexit 0\n")).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    release
}

fn register(home: &Path, role: &str, release: &Path) {
    metadata(
        &home.join(format!("services/{role}.json")),
        &serde_json::json!({
            "component": role, "label": format!("test.steve.{role}"), "release": release,
            "build": {"version": release.file_name().unwrap().to_str().unwrap(), "commit": "a".repeat(40)}
        }),
    );
}

#[test]
fn adapters_commands_reach_the_installed_hub_or_nodes_release() {
    let temp = tempfile::tempdir().unwrap();
    let account = temp.path().canonicalize().unwrap();
    let calls = account.join("calls");
    // The directory's name is not its role. A remote installer and a local setup use the same
    // service contract; a low-level `steve node install` need not have a setup receipt at all.
    for (role, directory) in [
        ("hub", "misleading-node-home"),
        ("node", "misleading-hub-home"),
    ] {
        let home = account.join(directory);
        let old = adapter_release(&home, role, "1.0.0", &calls);
        let new = adapter_release(&home, role, "1.1.0", &calls);
        register(&home, role, if role == "hub" { &old } else { &new });
        metadata(
            &home.join("setup.json"),
            &serde_json::json!({
                "format": 1, "component": role, "home": home, "version": "1.0.0", "steps": ["installed"]
            }),
        );
        if role == "hub" {
            std::os::unix::fs::symlink(&new, home.join("current")).unwrap();
        }
        // Node: no current. Hub: original registration/receipt, newer current.
        for (args, expected) in [
            (vec!["adapters", "install", "codex"], 0),
            (vec!["adapters", "install", "claude-code", "codex", role], 0),
            (vec!["adapters", "upgrade"], 0),
            (vec!["adapters"], 3),
        ] {
            let mut args = args;
            args.extend(["--home", home.to_str().unwrap()]);
            let out = invoke(&account, &args);
            assert_eq!(
                out.status.code(),
                Some(expected),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let prefix = format!("{role}|1.1.0|{}|zh-CN|", home.display());
        let first = if role == "hub" {
            "--adapters"
        } else {
            "adapters"
        };
        let actual = fs::read_to_string(&calls).unwrap();
        let lines: Vec<_> = actual.lines().filter(|s| s.starts_with(&prefix)).collect();
        assert_eq!(
            lines,
            [
                format!("{prefix}{first} install codex"),
                format!("{prefix}{first} install claude-code codex"),
                format!("{prefix}{first} install"),
                format!("{prefix}{first} status"),
            ]
        );
        // Stale Node current must not override its service registration after an upgrade/rollback.
        if role == "node" {
            std::os::unix::fs::symlink(&old, home.join("current")).unwrap();
            let out = invoke(
                &account,
                &[
                    "adapters",
                    "install",
                    "codex",
                    "--home",
                    home.to_str().unwrap(),
                ],
            );
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                fs::read_to_string(&calls)
                    .unwrap()
                    .lines()
                    .last()
                    .unwrap()
                    .starts_with(&prefix)
            );
        }
    }
}

#[test]
fn adapters_defaults_prefer_an_installed_hub_and_find_a_standalone_node() {
    let temp = tempfile::tempdir().unwrap();
    let account = temp.path().canonicalize().unwrap();
    let calls = account.join("calls");
    for role in ["node", "hub"] {
        let home = account.join(if role == "hub" {
            ".steve"
        } else {
            ".steve-node"
        });
        let release = adapter_release(&home, role, "1.0.0", &calls);
        register(&home, role, &release);
        if role == "hub" {
            std::os::unix::fs::symlink(&release, home.join("current")).unwrap();
        }
        let out = invoke(&account, &["adapters", "install", "codex"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            fs::read_to_string(&calls)
                .unwrap()
                .lines()
                .last()
                .unwrap()
                .starts_with(role)
        );
    }
    let out = invoke(&account, &["adapters", "install", "codex", "node"]);
    assert!(out.status.success());
    assert!(
        fs::read_to_string(&calls)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .starts_with("node")
    );
    // A retired Hub is no longer the default installation, even if its current link remains.
    let record = account.join(".steve/services/hub.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    value["uninstalled"] = true.into();
    metadata(&record, &value);
    let out = invoke(&account, &["adapters", "install", "codex"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fs::read_to_string(&calls)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .starts_with("node")
    );
}

#[test]
fn adapters_reject_unknown_arguments_before_resolving_a_home() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec!["adapters", "remove", "codex"],
        vec!["adapters", "upgrade", "codex"],
        vec!["adapters", "install", "codex", "hub", "more"],
    ] {
        let out = invoke(temp.path(), &args);
        assert_eq!(out.status.code(), Some(1));
    }
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn adapters_refuse_uninstalled_foreign_or_incomplete_homes_without_running_anything() {
    let cases = [
        "empty",
        "current-only",
        "setup-only",
        "wrong-role",
        "wrong-home",
        "uninstalled",
        "pending-uninstall",
        "bad-json",
        "symlink-record",
        "writable-record",
        "mismatched-release",
        "missing-binary",
        "foreign-current",
        "explicit-wrong-role",
    ];
    for case in cases {
        let temp = tempfile::tempdir().unwrap();
        let account = temp.path().canonicalize().unwrap();
        let home = account.join("installation");
        let calls = account.join("calls");
        let role = if case == "foreign-current" {
            "hub"
        } else {
            "node"
        };
        let release = adapter_release(&home, role, "1.0.0", &calls);
        register(&home, role, &release);
        let record = home.join(format!("services/{role}.json"));
        match case {
            "empty" | "current-only" | "setup-only" => {
                fs::remove_file(&record).unwrap();
                if case == "current-only" {
                    std::os::unix::fs::symlink(&release, home.join("current")).unwrap();
                }
            }
            "uninstalled" | "pending-uninstall" => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
                if case == "uninstalled" {
                    value["uninstalled"] = true.into();
                } else {
                    value["uninstall"] = serde_json::json!({"phase": "pending"});
                }
                metadata(&record, &value);
            }
            "bad-json" => fs::write(&record, b"not-json").unwrap(),
            "symlink-record" => {
                let other = home.join("other.json");
                fs::rename(&record, &other).unwrap();
                std::os::unix::fs::symlink(other, &record).unwrap();
            }
            "writable-record" => {
                fs::set_permissions(&record, fs::Permissions::from_mode(0o666)).unwrap()
            }
            "mismatched-release" => metadata(
                &release.join("release.json"),
                &serde_json::json!({"format_version": 1, "version": "2.0.0", "commit": "a".repeat(40)}),
            ),
            "missing-binary" => fs::remove_file(release.join("bin/steve-node")).unwrap(),
            "foreign-current" => {
                let foreign = adapter_release(&account.join("foreign"), "hub", "1.0.0", &calls);
                std::os::unix::fs::symlink(foreign, home.join("current")).unwrap();
            }
            _ => {}
        }
        if matches!(case, "setup-only" | "wrong-role" | "wrong-home") {
            metadata(
                &home.join("setup.json"),
                &serde_json::json!({
                    "format": 1, "component": if case == "wrong-role" {"hub"} else {"node"},
                    "home": if case == "wrong-home" { account.clone() } else { home.clone() }, "steps": ["installed"]
                }),
            );
        }
        let mut args = vec!["adapters", "install", "codex"];
        if case == "explicit-wrong-role" {
            args.push("hub");
        }
        args.extend(["--home", home.to_str().unwrap()]);
        let out = invoke(&account, &args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{case}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!calls.exists(), "{case} ran an adapter command");
    }
}
