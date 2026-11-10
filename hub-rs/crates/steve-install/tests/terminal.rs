//! Exercise actual CLI configuration and cancellation on an OS pseudo-terminal.
//! Every installation path and executable except the CLI is an isolated fixture.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
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

/// A 1.2.3 installer under `root` whose programs cannot initialize a keychain or start a service;
/// returns its `steve`.
fn installer(root: &Path) -> PathBuf {
    let package = root.join("installer");
    for path in [package.join("bin"), package.join("ops"), package.join("web/dist")] {
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
        &format!("#!/bin/sh\n{permitted} || {{ printf 'changed\\n' >> {calls}; exit 97; }}\nprintf 'read\\n' >> {calls}\n{output}\n"),
    );
}

#[test]
fn actual_setup_cancelled_at_a_prompt_persists_nothing() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let account = root.join("account");
    let empty_path = root.join("empty-path");
    let install = account.join(".steve");
    for path in [account.clone(), empty_path.clone()] {
        fs::create_dir_all(path).unwrap();
    }
    let cli = installer(&root);

    let mut pty = Pty::open().unwrap();
    let before = pty.attributes();
    let mut command = Command::new(&cli);
    command
        .args(["setup", "hub"])
        .env_clear()
        .env("STEVE_LANG", "zh-CN")
        .env("HOME", &account)
        .env("PATH", &empty_path)
        .env("TMPDIR", &root)
        .current_dir(&root)
        .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
        .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
        .stderr(Stdio::from(pty.slave.try_clone().unwrap()))
        .process_group(0);
    let mut process = Process(command.spawn().unwrap());
    // without TERM the wizard types its questions. An address nothing can listen on is asked
    // again; then the person gives up (end of input)
    pty.wait_prompt(&mut process.0, "监听地址 [0.0.0.0:8440]: ");
    pty.master.write_all(b"localhost:8440\n").unwrap();
    pty.wait_prompt(&mut process.0, "监听地址 [0.0.0.0:8440]: ");
    pty.master.write_all(&[4]).unwrap();
    let status = pty.wait_exit(&mut process.0);
    assert_eq!(status.code(), Some(1), "end of input is handled as cancellation");
    let after = pty.attributes();
    assert_eq!(after.c_lflag, before.c_lflag, "terminal flags are unchanged");
    assert_eq!(after.c_cc, before.c_cc, "terminal input controls are unchanged");
    let output = String::from_utf8_lossy(&pty.output);
    assert_eq!(output.matches("监听地址 [0.0.0.0:8440]: ").count(), 2, "{output}");
    assert!(output.contains("初始化已中断"), "{output}");
    assert!(!output.contains("OIDC"), "setup asks for no OIDC: {output}");
    assert!(!output.contains("浏览器访问地址"), "setup asks for no browser address: {output}");
    assert!(!output.contains('\x1b'), "plain output has no escape sequence: {output:?}");

    let receipt_bytes = fs::read(install.join("setup.json")).unwrap();
    let receipt: Value = serde_json::from_slice(&receipt_bytes).unwrap();
    assert_eq!(receipt["steps"], json!([]));
    assert_eq!(receipt["files"], json!({}));
    assert!(receipt["environment"].is_null());
    assert_ne!(receipt["keychain_started"], true);
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
    manager(&bin, &temporary.path().join("manager-calls"), false);
    let command = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_steve"));
        cmd.args(args)
            .env_clear()
            .env("STEVE_LANG", "zh-CN")
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

/// Answers every request on a loopback port with `body`, as a Hub's `/healthz` would.
fn health_server(body: &'static str) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

#[test]
fn hub_status_names_the_version_the_running_hub_reports() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let (account, bin, home) = (root.join("account"), root.join("bin"), root.join("account/.steve"));
    for path in [&bin, &home] {
        fs::create_dir_all(path).unwrap();
    }
    manager(&bin, &root.join("manager-calls"), true);
    let port = health_server(r#"{"ok":true,"version":"9.8.7","build_commit":"c0ffee0123456789"}"#);
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let address = |port: u16| {
        private_fixture_file(
            &home.join("steve.env"),
            format!("STEVE_HOST=0.0.0.0\nSTEVE_PORT={port}\nSTEVE_PUBLIC_URL=http://127.0.0.1:1\n"),
        )
    };
    let command = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_steve"));
        cmd.args(args).env_clear().env("STEVE_LANG", "zh-CN").env("HOME", &account).env("PATH", &bin);
        cmd
    };
    address(port);
    for action in ["status", "doctor"] {
        let (status, human) = terminal_output(&mut command(&["hub", action]));
        assert!(status.success(), "{human}");
        assert!(human.contains("服务进程：运行中"), "{human}");
        // twelve characters of the commit the Hub itself answered with
        assert!(human.contains("运行版本：9.8.7（c0ffee012345）"), "{human}");
    }
    // what automation reads does not depend on whether the Hub answered
    let (status, json) = terminal_output(&mut command(&["hub", "status", "--json"]));
    assert!(status.success());
    let report: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(report["service"]["running"], true);
    assert!(report["running_build"].is_null());
    assert_eq!(command(&["hub", "status"]).output().unwrap().stdout, json.as_bytes());
    // a process that does not answer is not given the configured version
    address(silent);
    let (status, human) = terminal_output(&mut command(&["hub", "status"]));
    assert!(status.success());
    assert!(human.contains("运行版本：未核验"), "{human}");
    assert!(!human.contains("9.8.7"), "{human}");
    let calls = fs::read_to_string(root.join("manager-calls")).unwrap();
    assert!(calls.lines().all(|call| call == "read"), "{calls}");
}

#[test]
fn another_versions_installation_is_refused_until_fresh_sets_it_aside() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let (account, bin, home) = (root.join("account"), root.join("bin"), root.join("account/.steve"));
    for directory in [&account, &bin, &home, &home.join("data")] {
        fs::create_dir_all(directory).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let cli = installer(&root);
    manager(&bin, &root.join("manager-calls"), false);
    // 1.2.2 finished installing here; the installer is 1.2.3
    let receipt = serde_json::to_vec(&json!({
        "format":1,"component":"hub","home":home,"version":"1.2.2","commit":"b".repeat(40),
        "steps":["environment","verified","installed"],"files":{}
    }))
    .unwrap();
    private_fixture_file(&home.join("setup.json"), &receipt);
    private_fixture_file(&home.join("data/steve.sqlite3"), b"the database of the installation in use");
    let before = installation_tree(&account);
    let setup = |args: &[&str]| {
        Command::new(&cli)
            .arg("setup")
            .args(args)
            .env_clear()
            .env("STEVE_LANG", "zh-CN")
            .env("HOME", &account)
            .env("PATH", &bin)
            .env("TMPDIR", &root)
            .current_dir(&root)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let home_text = home.to_str().unwrap();
    for args in [&["hub"][..], &["hub", "--listen", "127.0.0.1:8440"], &["node", "--home", home_text]] {
        let refused = setup(args);
        let said = String::from_utf8_lossy(&refused.stderr);
        assert_eq!(refused.status.code(), Some(1), "{args:?}: {said}");
        assert!(said.contains("Hub 1.2.2 · bbbbbbbb；不会覆盖"), "{args:?}: {said}");
        assert!(said.contains("steve setup --fresh"), "{args:?}: {said}");
        assert_eq!(installation_tree(&account), before, "{args:?} changed the installation");
    }
    assert!(!root.join("manager-calls").exists(), "a refusal asks the service manager nothing");

    // --fresh: the directory is kept whole under another name, then setup starts over (and here
    // ends at its first question, with no one to answer)
    let fresh = setup(&["hub", "--fresh"]);
    let said = format!("{}{}", String::from_utf8_lossy(&fresh.stdout), String::from_utf8_lossy(&fresh.stderr));
    assert_eq!(fresh.status.code(), Some(1), "{said}");
    assert!(said.contains("初始化已中断"), "{said}");
    let aside: Vec<PathBuf> = fs::read_dir(&account)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path != &home)
        .collect();
    assert_eq!(aside.len(), 1, "{aside:?}");
    let name = aside[0].file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with(".steve.bak-2") && name.len() == ".steve.bak-".len() + 15, "{name}");
    assert!(said.contains(aside[0].to_str().unwrap()), "setup says where the installation went: {said}");
    assert_eq!(fs::read(aside[0].join("setup.json")).unwrap(), receipt);
    let mut kept = installation_tree(&aside[0]);
    kept.remove(Path::new(".setup.lock"));
    let was: BTreeMap<PathBuf, TreeEntry> = before
        .into_iter()
        .filter_map(|(path, entry)| Some((path.strip_prefix(".steve").ok()?.to_owned(), entry)))
        .filter(|(path, _)| !path.as_os_str().is_empty())
        .collect();
    assert_eq!(kept, was, "nothing in the installation set aside changed");
    let started: Value = serde_json::from_slice(&fs::read(home.join("setup.json")).unwrap()).unwrap();
    assert_eq!(started["version"], "1.2.3");
    assert_eq!(started["steps"], json!([]));
    assert!(!home.join("data").exists());
    // the service was asked whether it runs; nothing was there to stop or remove
    let calls = fs::read_to_string(root.join("manager-calls")).unwrap();
    assert!(!calls.is_empty() && calls.lines().all(|call| call == "read"), "{calls}");
}

#[derive(Debug, PartialEq, Eq)]
enum TreeEntry {
    Directory(u32),
    File(u32, String),
    Link(PathBuf),
}

fn installation_tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn collect(root: &Path, directory: &Path, entries: &mut BTreeMap<PathBuf, TreeEntry>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let mode = metadata.permissions().mode() & 0o777;
            let value = if metadata.file_type().is_symlink() {
                TreeEntry::Link(fs::read_link(&path).unwrap())
            } else if metadata.is_dir() {
                collect(root, &path, entries);
                TreeEntry::Directory(mode)
            } else {
                assert!(metadata.is_file(), "unexpected fixture file type");
                TreeEntry::File(mode, steve_install::common::digest(&path).unwrap())
            };
            entries.insert(path.strip_prefix(root).unwrap().to_owned(), value);
        }
    }
    let mut entries = BTreeMap::new();
    collect(root, root, &mut entries);
    entries
}

fn private_fixture_file(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

/// A native, offline fixture lets macOS exercise the production signature verifier.
/// Ad-hoc signatures apply only to these temporary binaries; no certificate or Keychain
/// is created, queried, or used. The helper is never executable by the tested workflow.
fn maintenance_hub(root: &Path, binary: &Path, info: &Value, calls: &Path) {
    let source = root.join("fixture_hub.rs");
    fs::write(
        &source,
        format!(
            r##"use std::io::Write;
fn main() {{
    let args: Vec<_> = std::env::args().collect();
    let operation = args.get(1).map(String::as_str).unwrap_or("unexpected");
    let mut log = std::fs::OpenOptions::new().create(true).append(true).open({calls:?}).unwrap();
    writeln!(log, "{{}}", operation).unwrap();
    if args.len() != 2 || !args[0].ends_with("/steve-hub") {{ std::process::exit(95); }}
    match operation {{
        "--build-info" => println!("{{}}", {info:?}),
        "--check-config" => {{
            for (key, expected) in [
                ("STEVE_HOST", "0.0.0.0"), ("STEVE_PORT", "8555"),
                ("STEVE_PUBLIC_URL", "https://updated.example"),
                ("STEVE_OIDC_ISSUER", "https://issuer.example/new/"),
                ("STEVE_OIDC_CLIENT_ID", "updated-client"),
            ] {{ assert_eq!(std::env::var(key).unwrap(), expected); }}
            assert!(std::env::var_os("STEVE_OIDC_CLIENT_SECRET").is_none());
            assert!(std::path::Path::new(&std::env::var("STEVE_OIDC_CLIENT_SECRET_FILE").unwrap()).is_file());
            println!("{{}}", r#"{{"ok":true,"warnings":[]}}"#);
        }}
        _ => std::process::exit(95),
    }}
}}
"##,
            calls = calls.to_str().unwrap(),
            info = info.to_string(),
        ),
    )
    .unwrap();
    // Only the test compiler inherits its toolchain environment. The actual CLI below
    // receives an empty environment plus a temporary HOME and a denying fixture PATH.
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(compiler)
        .arg("--edition=2024")
        .arg(&source)
        .arg("-o")
        .arg(binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "compile isolated Hub fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::set_permissions(binary, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn actual_reconfigure_preserves_installation_and_supports_noop_and_eof() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let account = root.join("existing account");
    let home = account.join(".steve");
    let release = home.join("releases/1.2.3");
    let deny_bin = root.join("deny-bin");
    let calls = root.join("hub-calls");
    let forbidden = root.join("forbidden-calls");
    let label = "io.github.infranite.steve.terminal-fixture";
    for directory in [
        account.clone(),
        home.clone(),
        home.join("releases"),
        release.clone(),
        release.join("bin"),
        release.join("web"),
        release.join("web/dist"),
        home.join("web"),
        home.join("services"),
        home.join("secrets"),
        home.join("data"),
        home.join("vault-keys"),
        deny_bin.clone(),
    ] {
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let forbidden_quoted = steve_install::common::shell_quote(forbidden.to_str().unwrap()).unwrap();
    for name in ["launchctl", "systemctl", "security"] {
        executable(
            &deny_bin.join(name),
            &format!("#!/bin/sh\nprintf '{name}\\n' >> {forbidden_quoted}\nexit 95\n"),
        );
    }
    let info =
        json!({"format_version":1,"version":"1.2.3","commit":"a".repeat(40),"check_config":true});
    for relative in ["release.json", "web/dist/release.json"] {
        private_fixture_file(&release.join(relative), serde_json::to_vec(&info).unwrap());
    }
    maintenance_hub(&root, &release.join("bin/steve-hub"), &info, &calls);
    symlink(&release, home.join("current")).unwrap();
    symlink(release.join("web/dist"), home.join("web/current")).unwrap();
    let service_definition = if cfg!(target_os = "macos") {
        account
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"))
    } else {
        account
            .join(".config/systemd/user")
            .join(format!("{label}.service"))
    };
    fs::create_dir_all(service_definition.parent().unwrap()).unwrap();
    #[cfg(target_os = "macos")]
    {
        for directory in [home.join("signing"), home.join("bin")] {
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let helper = home.join("bin/steve-keyhelper");
        fs::copy(release.join("bin/steve-hub"), &helper).unwrap();
        for (binary, identifier) in [
            (release.join("bin/steve-hub"), format!("{label}.hub")),
            (helper, format!("{label}.keyhelper")),
        ] {
            let output = Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-", "--identifier", &identifier])
                .arg(&binary)
                .env_clear()
                .env("STEVE_LANG", "zh-CN")
                .env("HOME", &account)
                .env("PATH", &deny_bin)
                .env("TMPDIR", &root)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "ad-hoc fixture signature: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        private_fixture_file(&home.join("signing/identity.json"), serde_json::to_vec(&json!({
            "label":label,"hub_identifier":format!("{label}.hub"),
            "keyhelper_identifier":format!("{label}.keyhelper"),"vault_service":format!("{label}.vault")
        })).unwrap());
        let mut definition = Vec::new();
        plist::to_writer_xml(
            &mut definition,
            &json!({
                "Label":label,"EnvironmentVariables":{"STEVE_HOME":home},
                "ProgramArguments":["/usr/bin/false"]
            }),
        )
        .unwrap();
        private_fixture_file(&service_definition, definition);
    }
    #[cfg(target_os = "linux")]
    private_fixture_file(
        &service_definition,
        format!(
            "[Service]\nEnvironment={}\nExecStart=/usr/bin/false\n",
            serde_json::to_string(&format!("STEVE_HOME={}", home.display())).unwrap()
        ),
    );
    private_fixture_file(
        &home.join("services/hub.json"),
        serde_json::to_vec(&json!({
            "component":"hub","label":label,"release":release,"build":info
        }))
        .unwrap(),
    );
    let secret = home.join("secrets/oidc-client-secret");
    let sentinel = "reconfigure-existing-secret-never-output-83db7";
    private_fixture_file(&secret, sentinel);
    private_fixture_file(
        &home.join("data/hub.sqlite3"),
        b"existing database fixture; never open or replace",
    );
    private_fixture_file(
        &home.join("vault-keys/existing.key"),
        b"existing vault identity fixture",
    );
    let mut environment = BTreeMap::from([
        ("STEVE_HOME".to_owned(), home.to_str().unwrap().to_owned()),
        ("STEVE_HOST".into(), "127.0.0.1".into()),
        ("STEVE_PORT".into(), "8440".into()),
        ("STEVE_ALLOW_ANY_HOST".into(), "0".into()),
        (
            "STEVE_PUBLIC_URL".into(),
            "https://original.example/".into(),
        ),
        (
            "STEVE_PUBLIC_URLS".into(),
            "https://original.example/,https://alias.example/".into(),
        ),
        (
            "STEVE_OIDC_ISSUER".into(),
            "https://issuer.example/old/".into(),
        ),
        ("STEVE_OIDC_CLIENT_ID".into(), "original-client".into()),
        (
            "STEVE_OIDC_CLIENT_SECRET_FILE".into(),
            secret.to_str().unwrap().to_owned(),
        ),
        ("STEVE_VAULT_KEYSTORE".into(), "file".into()),
        (
            "UNRELATED".into(),
            "literal $HOME; $(do-not-run) ' preserved".into(),
        ),
    ]);
    let render = |values: &BTreeMap<String, String>| {
        let mut bytes = String::from("# operator comment stays byte-for-byte\r\n");
        for (key, value) in values {
            bytes.push_str(&format!(
                "export {key}={} # keep {key}\r\n",
                steve_install::common::shell_quote(value).unwrap()
            ));
        }
        bytes.into_bytes()
    };
    let original_env = render(&environment);
    private_fixture_file(&home.join("steve.env"), &original_env);
    let original_receipt = json!({
        "format":1,"component":"hub","home":home,"version":"1.2.3","commit":"a".repeat(40),
        "steps":["environment","verified","installed"],"environment":environment,
        "files":{"steve.env":steve_install::common::digest(&home.join("steve.env")).unwrap(),
            "secrets/oidc-client-secret":steve_install::common::digest(&secret).unwrap()},
        "retained_operator_metadata":{"example":"must remain"}
    });
    let original_receipt_bytes = serde_json::to_vec(&original_receipt).unwrap();
    private_fixture_file(&home.join("setup.json"), &original_receipt_bytes);
    let before = installation_tree(&account);
    let start = || {
        let pty = Pty::open().unwrap();
        let process = Process(
            Command::new(env!("CARGO_BIN_EXE_steve"))
                .args(["setup", "hub", "--reconfigure", "--home"])
                .arg(&home)
                .env_clear()
                .env("STEVE_LANG", "zh-CN")
                .env("HOME", &account)
                .env("PATH", &deny_bin)
                .env("TMPDIR", &root)
                .current_dir(&root)
                .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
                .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
                .stderr(Stdio::from(pty.slave.try_clone().unwrap()))
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        (pty, process)
    };
    let (mut pty, mut process) = start();
    for (prompt, answer) in [
        (
            "监听地址（多个 IP:端口用逗号分隔） [127.0.0.1:8440]: ",
            "0.0.0.0:8555\n",
        ),
        (
            "浏览器访问地址 [https://original.example/]: ",
            "https://updated.example/\n",
        ),
        (
            "OIDC issuer [https://issuer.example/old/]: ",
            "https://issuer.example/new/\n",
        ),
        ("OIDC client ID [original-client]: ", "updated-client\n"),
        ("确认保存并备份现有配置？[y/N] [N]: ", "y\n"),
    ] {
        pty.wait_prompt(&mut process.0, prompt);
        pty.master.write_all(answer.as_bytes()).unwrap();
    }
    let status = pty.wait_exit(&mut process.0);
    let output = String::from_utf8_lossy(&pty.output);
    assert!(status.success(), "{output}");
    assert!(
        output.contains("配置与收据已保存，本机配置检查通过"),
        "{output}"
    );
    assert!(output.contains("steve hub restart --home"), "{output}");
    assert!(!output.contains(sentinel) && !output.contains("OIDC client secret（不回显）"));
    for (key, value) in [
        ("STEVE_HOST", "0.0.0.0"),
        ("STEVE_PORT", "8555"),
        ("STEVE_ALLOW_ANY_HOST", "1"),
        ("STEVE_PUBLIC_URL", "https://updated.example"),
        (
            "STEVE_PUBLIC_URLS",
            "https://updated.example,https://alias.example/",
        ),
        ("STEVE_OIDC_ISSUER", "https://issuer.example/new/"),
        ("STEVE_OIDC_CLIENT_ID", "updated-client"),
    ] {
        environment.insert(key.into(), value.into());
    }
    assert_eq!(
        fs::read(home.join("steve.env")).unwrap(),
        render(&environment)
    );
    let mut expected_receipt = original_receipt.clone();
    expected_receipt["environment"] = serde_json::to_value(&environment).unwrap();
    expected_receipt["files"]["steve.env"] = steve_install::common::digest(&home.join("steve.env"))
        .unwrap()
        .into();
    let receipt: Value =
        serde_json::from_slice(&fs::read(home.join("setup.json")).unwrap()).unwrap();
    assert_eq!(receipt, expected_receipt);
    let protected = |mut tree: BTreeMap<PathBuf, TreeEntry>| {
        tree.retain(|path, _| {
            ![
                ".steve/steve.env",
                ".steve/setup.json",
                ".steve/.setup.lock",
                ".steve/releases/.release.lock",
                ".steve/setup-reconfigure",
            ]
            .iter()
            .any(|editable| path.starts_with(editable))
        });
        tree
    };
    let committed = installation_tree(&account);
    assert_eq!(
        protected(before),
        protected(installation_tree(&account)),
        "secret, database, identity, service and release links must be unchanged"
    );
    let archives: Vec<_> = fs::read_dir(home.join("setup-reconfigure"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(archives.len(), 1);
    assert_ne!(archives[0].file_name().unwrap(), "pending");
    assert_eq!(
        fs::read(archives[0].join("before.env")).unwrap(),
        original_env
    );
    assert_eq!(
        fs::read(archives[0].join("before-setup.json")).unwrap(),
        original_receipt_bytes
    );
    assert_eq!(
        fs::metadata(archives[0].join("before.env"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let prompts = [
        "监听地址（多个 IP:端口用逗号分隔） [0.0.0.0:8555]: ",
        "浏览器访问地址 [https://updated.example]: ",
        "OIDC issuer [https://issuer.example/new/]: ",
        "OIDC client ID [updated-client]: ",
    ];
    let (mut pty, mut process) = start();
    for prompt in prompts {
        pty.wait_prompt(&mut process.0, prompt);
        pty.master.write_all(b"\n").unwrap();
    }
    let status = pty.wait_exit(&mut process.0);
    let output = String::from_utf8_lossy(&pty.output);
    assert!(status.success(), "{output}");
    assert!(output.contains("配置与收据已经一致，无需修改"), "{output}");
    assert!(!output.contains("确认保存") && !output.contains(sentinel));
    assert_eq!(
        installation_tree(&account),
        committed,
        "no-op must not create another transaction or rewrite any file"
    );
    let (mut pty, mut process) = start();
    pty.wait_prompt(&mut process.0, prompts[0]);
    pty.master.write_all(b"127.0.0.1:9999\n").unwrap();
    pty.wait_prompt(&mut process.0, prompts[1]);
    let eof = pty.attributes().c_cc[libc::VEOF];
    pty.master.write_all(&[eof]).unwrap();
    let status = pty.wait_exit(&mut process.0);
    let output = String::from_utf8_lossy(&pty.output);
    assert_eq!(status.code(), Some(1), "{output}");
    assert!(
        output.contains("中断") && !output.contains(sentinel),
        "{output}"
    );
    assert_eq!(
        installation_tree(&account),
        committed,
        "EOF after editing a field must leave all installation bytes and modes unchanged"
    );
    assert!(
        !forbidden.exists(),
        "service or Keychain command was executed"
    );
    let calls = fs::read_to_string(calls).unwrap();
    assert!(
        calls
            .lines()
            .all(|call| matches!(call, "--build-info" | "--check-config")),
        "{calls}"
    );
    assert_eq!(
        calls
            .lines()
            .filter(|call| *call == "--check-config")
            .count(),
        1,
        "only the committed modification runs candidate validation"
    );
}
