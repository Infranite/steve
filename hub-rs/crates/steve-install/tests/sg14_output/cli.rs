//! Real CLI selection of human versus JSON output, using an isolated native manager.
use super::output;

use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use steve_runtime::testkit::{eventually_blocking, process::Command};

fn drain(master: &mut File, bytes: &mut Vec<u8>) {
    let mut buffer = [0; 4096];
    loop {
        match master.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&buffer[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => panic!("read PTY: {e}"),
        }
    }
    assert!(bytes.len() < 64 * 1024);
}

fn terminal(command: &mut Command) -> (ExitStatus, String) {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: openpty initializes both descriptors; all optional pointers are null.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    // SAFETY: each descriptor returned by openpty is owned exactly once.
    let (mut master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
    use std::os::fd::AsRawFd;
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        // SAFETY: live descriptors; only flags are changed, stdio gets its own duplicate.
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    // SAFETY: master remains live throughout the nonblocking reads below.
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut bytes = Vec::new();
    let mut status = None;
    eventually_blocking("isolated service summary CLI exits", || {
        drain(&mut master, &mut bytes);
        status = child.try_wait().unwrap();
        if status.is_some() {
            drain(&mut master, &mut bytes);
        }
        status.is_some()
    });
    (
        status.unwrap(),
        String::from_utf8(bytes).unwrap().replace("\r\n", "\n"),
    )
}

fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn native_manager_states_cross_language_and_json_selection() {
    for state in ["running", "stopped", "absent", "unknown"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let account = root.join("account");
        let home = root.join("installation");
        let bin = root.join("bin");
        let calls = root.join("manager-calls");
        for p in [&account, &home, &bin] {
            fs::create_dir(p).unwrap();
        }
        let label = "sg14.fixture";
        if state != "absent" {
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
            if cfg!(target_os = "macos") {
                plist::to_file_xml(
                    &config,
                    &json!({"Label":label,"EnvironmentVariables":{"STEVE_HOME":home}}),
                )
                .unwrap();
            } else {
                fs::write(
                    &config,
                    format!(
                        "[Service]\nEnvironment={}\n",
                        steve_install::common::shell_quote(&format!(
                            "STEVE_HOME={}",
                            home.display()
                        ))
                        .unwrap()
                    ),
                )
                .unwrap();
            }
            fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let (manager, query, answer) = if cfg!(target_os = "macos") {
            (
                "launchctl",
                "[ \"$1\" = print ]",
                match state {
                    "running" => "printf '%s\\n' 'state = running' 'pid = 4242'",
                    "stopped" => "printf '%s\\n' 'state = waiting'",
                    "absent" => "printf 'Could not find service\\n' >&2; exit 113",
                    _ => "printf 'manager unavailable\\n' >&2; exit 77",
                },
            )
        } else {
            (
                "systemctl",
                "[ \"$1\" = --user ] && [ \"$2\" = show ]",
                match state {
                    "running" => {
                        "printf '%s\\n' 'LoadState=loaded' 'ActiveState=active' 'MainPID=4242' 'ExecMainStatus=0'"
                    }
                    "stopped" => {
                        "printf '%s\\n' 'LoadState=loaded' 'ActiveState=inactive' 'MainPID=0' 'ExecMainStatus=0'"
                    }
                    "absent" => {
                        "printf '%s\\n' 'LoadState=not-found' 'ActiveState=inactive' 'MainPID=0' 'ExecMainStatus=0'"
                    }
                    _ => "printf 'manager unavailable\\n' >&2; exit 77",
                },
            )
        };
        let log = steve_install::common::shell_quote(calls.to_str().unwrap()).unwrap();
        executable(
            &bin.join(manager),
            &format!(
                "#!/bin/sh\n{query} || {{ printf 'forbidden\\n' >> {log}; exit 97; }}\nprintf 'query\\n' >> {log}\n{answer}\n"
            ),
        );
        for locale in ["zh-CN", "en"] {
            let command = || {
                let mut c = Command::new(env!("CARGO_BIN_EXE_steve"));
                c.args(["--lang", locale, "node", "status", "--home"])
                    .arg(&home)
                    .args(["--label", label])
                    .env_clear()
                    .env("HOME", &account)
                    .env("PATH", &bin)
                    .env("TMPDIR", &root)
                    .env("STEVE_HOME", &home)
                    .env("STEVE_PORT", "18990")
                    .env("STEVE_LOCAL_NODE_PORT", "18993")
                    .env("STEVE_VAULT_KEYSTORE", "file")
                    .env("STEVE_BACKUP", "off")
                    .env("STEVE_NO_RESUME", "1")
                    .env("STEVE_PLAIN", "1")
                    .env("TERM", "dumb")
                    .current_dir(&root);
                c
            };
            let automatic = command().output().unwrap();
            let explicit = command().arg("--json").output().unwrap();
            assert!(
                automatic.status.success() && explicit.status.success(),
                "{state}: {}",
                String::from_utf8_lossy(&automatic.stderr)
            );
            assert!(automatic.stderr.is_empty() && explicit.stderr.is_empty());
            assert_eq!(automatic.stdout, explicit.stdout, "{state}/{locale}");
            let report: Value = serde_json::from_slice(&automatic.stdout).unwrap();
            let expected_json = format!("{}\n", serde_json::to_string_pretty(&report).unwrap());
            assert_eq!(automatic.stdout, expected_json.as_bytes());
            assert_eq!(
                report["service"]["running"],
                match state {
                    "running" => json!(true),
                    "unknown" => Value::Null,
                    _ => json!(false),
                }
            );
            let expected = steve_i18n::with(locale, || {
                output::service("node", "status", &home, Some(label), &report)
            });
            let (exit, human) = terminal(&mut command());
            assert!(exit.success());
            assert_eq!(human, expected, "{state}/{locale}/human");
            let (exit, json) = terminal(command().arg("--json"));
            assert!(exit.success());
            assert_eq!(json, expected_json, "{state}/{locale}/json");
        }
        let queries = fs::read_to_string(&calls).unwrap();
        assert_eq!(queries.lines().count(), 8);
        assert!(queries.lines().all(|l| l == "query"));
        assert!(
            !home.join("logs").exists(),
            "rendering cannot read or create service logs"
        );
    }
}
