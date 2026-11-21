//! Home selection and service refusals through the actual CLI, with fake managers only.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn home_precedence_and_missing_registration_never_reach_the_service_manager() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let account = root.join("account");
    let bin = root.join("bin");
    let calls = root.join("manager-calls");
    let quoted_calls = steve_install::common::shell_quote(calls.to_str().unwrap()).unwrap();
    fs::create_dir_all(&account).unwrap();
    fs::create_dir(&bin).unwrap();
    for manager in ["launchctl", "systemctl"] {
        let program = bin.join(manager);
        fs::write(
            &program,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {quoted_calls}\nexit 0\n"),
        )
        .unwrap();
        fs::set_permissions(program, fs::Permissions::from_mode(0o755)).unwrap();
    }
    for role in ["hub", "node"] {
        let default = account.join(if role == "hub" {
            ".steve"
        } else {
            ".steve-node"
        });
        let environment = root.join(format!("{role}-environment"));
        let explicit = root.join(format!("{role}-explicit"));
        let label = format!("io.github.infranite.steve.{role}");
        let config = if cfg!(target_os = "macos") {
            account
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist"))
        } else {
            account
                .join(".config/systemd/user")
                .join(format!("{label}.service"))
        };
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        // Even an otherwise owned default-label unit must not authorize a service action.
        let definition = if cfg!(target_os = "macos") {
            let mut bytes = Vec::new();
            plist::to_writer_xml(
                &mut bytes,
                &serde_json::json!({"Label":label,"EnvironmentVariables":{"STEVE_HOME":default}}),
            )
            .unwrap();
            bytes
        } else {
            format!(
                "[Service]\nEnvironment=\"STEVE_HOME={}\"\n",
                default.display()
            )
            .into_bytes()
        };
        fs::write(&config, &definition).unwrap();
        for selection in ["default", "environment", "explicit"] {
            let selected = match selection {
                "default" => &default,
                "environment" => &environment,
                _ => &explicit,
            };
            for action in ["start", "stop", "restart", "uninstall"] {
                let mut command = Command::new(env!("CARGO_BIN_EXE_steve"));
                command
                    .args([role, action])
                    .env_clear()
                    .env("HOME", &account)
                    .env("PATH", &bin);
                if selection != "default" {
                    command.env("STEVE_HOME", &environment);
                }
                if selection == "explicit" {
                    command.arg("--home").arg(&explicit);
                }
                let output = command.output().unwrap();
                let error = String::from_utf8_lossy(&output.stderr);
                assert!(!output.status.success(), "{role} {action}: {error}");
                assert!(
                    error.contains(
                        selected
                            .join(format!("services/{role}.json"))
                            .to_str()
                            .unwrap()
                    ),
                    "{selection}, {role} {action}: {error}"
                );
                assert!(error.contains("steve setup"), "{error}");
                assert!(
                    !calls.exists(),
                    "{selection}, {role} {action} called a manager"
                );
                assert_eq!(fs::read(&config).unwrap(), definition);
                assert!(!selected.exists(), "a refusal created its home");
            }
        }
        let record = environment.join(format!("services/{role}.json"));
        fs::create_dir_all(record.parent().unwrap()).unwrap();
        fs::write(
            &record,
            serde_json::to_vec(&serde_json::json!({"component":role,"label":"test.original"}))
                .unwrap(),
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_steve"))
            .args([role, "stop", "--label", "test.other"])
            .env_clear()
            .env("HOME", &account)
            .env("STEVE_HOME", &environment)
            .env("PATH", &bin)
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(error.contains(record.to_str().unwrap()), "{error}");
        assert!(
            error.contains("test.original") && error.contains("test.other"),
            "{error}"
        );
        assert!(!calls.exists(), "a different --label called a manager");
    }
}

#[test]
fn reconfigure_selects_the_environment_home_unless_overridden() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let environment = root.join("environment");
    let explicit = root.join("explicit");
    for home in [&environment, &explicit] {
        fs::create_dir(home).unwrap();
        // An intentionally non-private home makes inspection fail with its selected path.
        fs::set_permissions(home, fs::Permissions::from_mode(0o755)).unwrap();
    }
    for override_home in [false, true] {
        let selected = if override_home {
            &explicit
        } else {
            &environment
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_steve"));
        command
            .args(["setup", "hub", "--reconfigure"])
            .env_clear()
            .env("HOME", &root)
            .env("STEVE_HOME", &environment);
        if override_home {
            command.arg("--home").arg(&explicit);
        }
        let output = command.output().unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(error.contains(selected.to_str().unwrap()), "{error}");
        assert!(!selected.join("setup-reconfigure").exists());
    }
}
