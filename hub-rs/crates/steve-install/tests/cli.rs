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
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn the_language_is_the_flag_then_steve_lang_then_the_locale() {
    let zh = help_in(&[], &[("STEVE_LANG", "zh-CN")]);
    let en = help_in(&[], &[("STEVE_LANG", "en")]);
    assert_ne!(zh, en);
    assert!(zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)), "{zh}");
    assert!(!en.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)), "{en}");
    // --lang wins over every variable, and any tag of a language chooses it
    assert_eq!(help_in(&["--lang", "en"], &[("STEVE_LANG", "zh-CN")]), en);
    assert_eq!(help_in(&["--lang", "zh-Hans"], &[("STEVE_LANG", "en"), ("LANG", "en_US.UTF-8")]), zh);
    // STEVE_LANG, then LC_ALL, LC_MESSAGES, LANG: the first that names one of ours
    assert_eq!(help_in(&[], &[("STEVE_LANG", "en"), ("LC_ALL", "zh_CN.UTF-8")]), en);
    assert_eq!(help_in(&[], &[("LC_ALL", "zh_CN.UTF-8"), ("LANG", "en_US.UTF-8")]), zh);
    assert_eq!(help_in(&[], &[("LC_ALL", "C"), ("LC_MESSAGES", "en_GB"), ("LANG", "zh_CN.UTF-8")]), en);
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
