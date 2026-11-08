//! Exercise the actual CLI's secret cancellation on an OS pseudo-terminal.
//! Every installation path and executable except the CLI is an isolated fixture.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

struct Pty {
    master: File,
    slave: File,
    output: Vec<u8>,
    prompt_cursor: usize,
}

impl Pty {
    fn open() -> io::Result<Self> {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: openpty writes two descriptors; no optional pointers are supplied.
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: each new descriptor is owned exactly once by its File.
        let pty = Self {
            master: unsafe { File::from_raw_fd(master) },
            slave: unsafe { File::from_raw_fd(slave) },
            output: Vec::new(),
            prompt_cursor: 0,
        };
        for fd in [master, slave] {
            // SAFETY: keep fixture-only descriptors out of the executed CLI; its stdio
            // receives separate copies through Command's Stdio configuration below.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let mut attributes = pty.attributes();
        attributes.c_lflag |= libc::ECHO | libc::ICANON | libc::ISIG;
        attributes.c_cc[libc::VINTR] = 3;
        // SAFETY: slave remains open, and attributes is initialized by tcgetattr.
        if unsafe { libc::tcsetattr(slave, libc::TCSANOW, &attributes) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: master remains open for both descriptor flag operations.
        let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(pty)
    }

    fn attributes(&self) -> libc::termios {
        // SAFETY: tcgetattr fills this termios for the live slave descriptor.
        let mut attributes = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.slave.as_raw_fd(), &mut attributes) },
            0,
            "{}",
            io::Error::last_os_error()
        );
        attributes
    }

    fn drain(&mut self) {
        let mut bytes = [0; 4096];
        loop {
            match self.master.read(&mut bytes) {
                Ok(0) => return,
                Ok(count) => self.output.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("read terminal: {error}"),
            }
            assert!(
                self.output.len() < 64 * 1024,
                "unexpected terminal output size"
            );
        }
    }

    fn wait_prompt(&mut self, child: &mut Child, prompt: &str) {
        // The wizard verifies and privately copies the real CLI before its first prompt.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            self.drain();
            if let Some(offset) = self.output[self.prompt_cursor..]
                .windows(prompt.len())
                .position(|bytes| bytes == prompt.as_bytes())
            {
                self.prompt_cursor += offset + prompt.len();
                return;
            }
            assert!(
                child.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "CLI did not reach {prompt}: {}",
                String::from_utf8_lossy(&self.output)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_exit(&mut self, child: &mut Child) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.drain();
            if let Some(status) = child.try_wait().unwrap() {
                self.drain();
                return status;
            }
            assert!(Instant::now() < deadline, "cancelled CLI did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            // SAFETY: process_group(0) gives this fixture a group named by its child PID.
            unsafe { libc::kill(-(self.0.id() as libc::pid_t), libc::SIGKILL) };
            let _ = self.0.wait();
        }
    }
}

fn executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn actual_setup_ctrl_c_restores_echo_and_never_persists_the_secret() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let account = root.join("account");
    let package = root.join("installer");
    let empty_path = root.join("empty-path");
    let install = account.join(".steve");
    for path in [
        account.clone(),
        empty_path.clone(),
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
        "hub_version":"1.2.3", "web_version":"1.2.3", "schema":{"target":54}
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
    for relative in [
        "bin/steve-keyhelper",
        "ops/run.sh",
        "ops/sign-setup.sh",
        "ops/release-lib.sh",
    ] {
        executable(&package.join(relative), "#!/bin/sh\nexit 95\n");
    }

    let mut pty = Pty::open().unwrap();
    let before = pty.attributes();
    let mut command = Command::new(&cli);
    command
        .args(["setup", "hub", "--listen", "0.0.0.0:8440"])
        .env_clear()
        .env("HOME", &account)
        .env("PATH", &empty_path)
        .env("TMPDIR", &root)
        .current_dir(&root)
        .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
        .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
        .stderr(Stdio::from(pty.slave.try_clone().unwrap()))
        .process_group(0);
    let mut process = Process(command.spawn().unwrap());
    for (prompt, answer) in [
        (
            "浏览器访问地址 [http://127.0.0.1:8440]: ",
            "http://0.0.0.0:8440\n",
        ),
        (
            "浏览器访问地址 [http://127.0.0.1:8440]: ",
            "https://hub.example/\n",
        ),
        ("OIDC issuer: ", "http://192.0.2.20/issuer/\n"),
        ("OIDC issuer: ", "https://issuer.example/tenant/\n"),
        ("OIDC client ID: ", "\n"),
        ("OIDC client ID: ", "terminal-client\n"),
    ] {
        pty.wait_prompt(&mut process.0, prompt);
        pty.master.write_all(answer.as_bytes()).unwrap();
    }
    pty.wait_prompt(&mut process.0, "OIDC client secret（不回显）: ");
    let hidden = pty.attributes();
    assert_eq!(hidden.c_lflag & (libc::ECHO | libc::ICANON | libc::ISIG), 0);
    let sentinel = "terminal-secret-must-never-appear-7f0a34";
    pty.master.write_all(sentinel.as_bytes()).unwrap();
    pty.master.write_all(&[3]).unwrap();
    let status = pty.wait_exit(&mut process.0);
    assert_eq!(status.code(), Some(1), "Ctrl-C is handled as cancellation");
    let after = pty.attributes();
    assert_eq!(
        after.c_lflag, before.c_lflag,
        "terminal flags must be restored"
    );
    assert_eq!(
        after.c_cc, before.c_cc,
        "terminal input controls must be restored"
    );
    let output = String::from_utf8_lossy(&pty.output);
    for prompt in [
        "浏览器访问地址 [http://127.0.0.1:8440]: ",
        "OIDC issuer: ",
        "OIDC client ID: ",
    ] {
        assert_eq!(output.matches(prompt).count(), 2, "{output}");
    }
    assert!(output.contains("secret 输入已中断"), "{output}");
    assert!(
        !output.contains(sentinel),
        "secret was echoed by the real CLI"
    );

    let receipt_bytes = fs::read(install.join("setup.json")).unwrap();
    let receipt: Value = serde_json::from_slice(&receipt_bytes).unwrap();
    assert_eq!(receipt["steps"], json!([]));
    assert_eq!(receipt["files"], json!({}));
    assert!(receipt["environment"].is_null());
    assert_ne!(receipt["keychain_started"], true);
    assert!(!String::from_utf8_lossy(&receipt_bytes).contains(sentinel));
    for relative in [
        "secrets",
        "steve.env",
        "signing",
        "services",
        "current",
        "data",
        "bin",
    ] {
        assert!(
            !install.join(relative).exists(),
            "cancelled setup wrote {relative}"
        );
    }
    assert!(!account.join("Library/LaunchAgents").exists());
    assert!(!account.join(".config/systemd/user").exists());
}

fn terminal_output(command: &mut Command) -> (ExitStatus, String) {
    let mut pty = Pty::open().unwrap();
    let mut process = Process(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    let status = pty.wait_exit(&mut process.0);
    (
        status,
        String::from_utf8(pty.output).unwrap().replace("\r\n", "\n"),
    )
}

#[test]
fn service_output_uses_stdout_terminal_and_preserves_json_for_automation() {
    let temporary = tempfile::tempdir().unwrap();
    let account = temporary.path().join("account");
    let bin = temporary.path().join("bin");
    fs::create_dir_all(&account).unwrap();
    fs::create_dir(&bin).unwrap();
    let called = temporary.path().join("manager-calls");
    let called = steve_install::common::shell_quote(called.to_str().unwrap()).unwrap();
    let (manager, output, permitted) = if cfg!(target_os = "macos") {
        (
            "launchctl",
            "printf '%s\\n' 'Could not find service' >&2; exit 113",
            "[ \"$1\" = print ]",
        )
    } else {
        (
            "systemctl",
            "printf '%s\\n' 'LoadState=not-found' 'ActiveState=inactive' 'MainPID=0' 'ExecMainStatus=0'",
            "[ \"$1\" = --user ] && [ \"$2\" = show ]",
        )
    };
    let manager = bin.join(manager);
    fs::write(
        &manager,
        format!("#!/bin/sh\n{permitted} || exit 97\nprintf 'read\\n' >> {called}\n{output}\n"),
    )
    .unwrap();
    fs::set_permissions(&manager, fs::Permissions::from_mode(0o755)).unwrap();
    let command = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_steve"));
        cmd.args(args)
            .env_clear()
            .env("HOME", &account)
            .env("PATH", &bin);
        cmd
    };
    let automatic = command(&["node", "status"]).output().unwrap();
    let explicit = command(&["node", "status", "--json"]).output().unwrap();
    assert!(automatic.status.success() && explicit.status.success());
    assert_eq!(automatic.stdout, explicit.stdout);
    let report: serde_json::Value = serde_json::from_slice(&automatic.stdout).unwrap();
    assert_eq!(
        automatic.stdout,
        format!("{}\n", serde_json::to_string_pretty(&report).unwrap()).as_bytes()
    );
    assert_eq!(report["service"]["running"], false);
    assert!(report["running_build"].is_null());

    let (status, human) = terminal_output(&mut command(&["node", "status"]));
    assert!(status.success());
    assert!(human.starts_with("Steve Node · 状态\n"));
    for text in [
        "服务进程：已停止",
        "运行版本：未核验",
        "设备配对：尚未配置",
        "Hub 连接与活动会话：未核验",
    ] {
        assert!(human.contains(text), "missing {text}: {human}");
    }
    let (status, json) = terminal_output(&mut command(&["node", "status", "--json"]));
    assert!(status.success());
    assert_eq!(json.as_bytes(), automatic.stdout);
    let (status, doctor) = terminal_output(&mut command(&["node", "doctor"]));
    assert!(
        status.success(),
        "doctor's existing exit semantics must not change"
    );
    assert!(doctor.contains("本地静态检查"));
    assert!(doctor.contains("服务尚未注册"));
    assert!(doctor.contains("Node 尚未配对"));
    let (status, info) = terminal_output(&mut command(&["--build-info"]));
    assert!(status.success());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&info).unwrap()["cli_protocol"],
        2
    );
    assert!(!account.join(".steve-node").exists());
    assert_eq!(
        fs::read_to_string(temporary.path().join("manager-calls")).unwrap(),
        "read\n".repeat(5)
    );
}
