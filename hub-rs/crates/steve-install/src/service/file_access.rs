//! macOS TCC file-access check and one-time guided setup for Steve Hub and Node.
//! Queries the running service's socket when available and falls back to deterministic,
//! non-destructive local checks against protected directories and configured project roots.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Result;
use steve_i18n::t;
use steve_types::devices::{
    FileAccessReport, FileAccessStatus, NODE_FILE_ACCESS_SOCKET, ProtectedAccess, ProtectedLocation,
};

use crate::ui;

/// Exit code returned when macOS file-access authorization is still required so scripts and
/// headless SSH sessions can distinguish missing TCC grants from generic errors (`1`) and user
/// cancellation (`130`).
pub const EXIT_ACCESS_MISSING: i32 = 2;

const FULL_DISK_ACCESS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// Maximum wait for a freshly started service to publish its file-access socket.
const FIRST_BOOT_SOCKET_WAIT: Duration = Duration::from_secs(4);
/// Maximum time spent waiting for a single socket response from the running Node.
const SOCKET_READ_TIMEOUT: Duration = Duration::from_secs(8);
/// Retry interval while waiting for a freshly started service socket to appear.
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound on JSON payload size read from the local file-access socket.
const SOCKET_READ_LIMIT_BYTES: u64 = 16 * 1024;

/// Non-prompting macOS directories guarded exclusively by Full Disk Access.
const FULL_DISK_ACCESS_CANARIES: &[&str] = &[
    "Library/Application Support/com.apple.TCC",
    "Library/Safari",
    "Library/Mail",
    "Library/Messages",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Desktop,
    Headless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideOutcome {
    Ready,
    NotApplicable,
    GuidedAndGranted,
    MissingActionRequired,
    Unknown,
}

impl GuideOutcome {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Ready | Self::NotApplicable | Self::GuidedAndGranted => 0,
            Self::MissingActionRequired | Self::Unknown => EXIT_ACCESS_MISSING,
        }
    }
}

/// Stable executable path registered with macOS TCC for this role so upgrades keep grants intact.
pub fn tcc_executable_path(role: &str, home: &Path) -> PathBuf {
    if role == "hub" {
        home.join("current/bin/steve-hub")
    } else {
        home.join(super::boot::RUNNER)
    }
}

/// Detect whether the current process has an interactive macOS desktop session that can open
/// System Settings without hanging. SSH sessions, non-TTY streams, and console-less sessions
/// always resolve to [`SessionKind::Headless`].
pub fn detect_session_kind() -> SessionKind {
    detect_session_kind_from(
        cfg!(target_os = "macos"),
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        |name| std::env::var_os(name).map(|v| v.to_string_lossy().into_owned()),
        console_owned_by_current_user(),
    )
}

fn detect_session_kind_from(
    is_macos: bool,
    interactive_tty: bool,
    env: impl Fn(&str) -> Option<String>,
    owns_console: bool,
) -> SessionKind {
    if !is_macos || !interactive_tty || !owns_console {
        return SessionKind::Headless;
    }
    for var in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "STEVE_PLAIN"] {
        if env(var).is_some_and(|v| !v.is_empty()) {
            return SessionKind::Headless;
        }
    }
    if env("TERM").is_some_and(|t| t.is_empty() || t == "dumb") {
        return SessionKind::Headless;
    }
    if env("STEVE_HEADLESS").is_some_and(|v| v == "1") {
        return SessionKind::Headless;
    }
    SessionKind::Desktop
}

#[cfg(target_os = "macos")]
fn console_owned_by_current_user() -> bool {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata("/dev/console") {
        Ok(meta) => meta.uid() == unsafe { libc::getuid() },
        Err(_) => true,
    }
}

#[cfg(not(target_os = "macos"))]
fn console_owned_by_current_user() -> bool {
    false
}

fn read_optional_bytes(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

fn hub_data_dir(home: &Path) -> PathBuf {
    read_optional_bytes(&home.join("steve.env"))
        .and_then(|bytes| crate::env_file::EnvDocument::parse(&bytes).ok())
        .and_then(|doc| doc.values().get("STEVE_DATA_DIR").cloned())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("data"))
}

fn socket_path(role: &str, home: &Path) -> PathBuf {
    if role == "hub" {
        hub_data_dir(home)
            .join("node")
            .join(NODE_FILE_ACCESS_SOCKET)
    } else {
        home.join(NODE_FILE_ACCESS_SOCKET)
    }
}

fn roots_file_path(role: &str, home: &Path) -> PathBuf {
    if role == "hub" {
        hub_data_dir(home).join("node/roots.json")
    } else {
        home.join("roots.json")
    }
}

fn configured_roots(role: &str, home: &Path) -> Vec<PathBuf> {
    let Some(bytes) = read_optional_bytes(&roots_file_path(role, home)) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|item| {
            item.as_str()
                .or_else(|| item.get("path").and_then(serde_json::Value::as_str))
        })
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .collect()
}

fn read_socket(socket_file: &Path, wait: Duration) -> Option<FileAccessReport> {
    let end = Instant::now() + wait;
    let socket = loop {
        match UnixStream::connect(socket_file) {
            Ok(socket) => break socket,
            Err(_) if Instant::now() < end => std::thread::sleep(SOCKET_POLL_INTERVAL),
            Err(_) => return None,
        }
    };
    socket.set_read_timeout(Some(SOCKET_READ_TIMEOUT)).ok()?;
    let mut bytes = Vec::new();
    socket
        .take(SOCKET_READ_LIMIT_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn read_folder(path: &Path) -> Option<FileAccessStatus> {
    use FileAccessStatus::*;
    match std::fs::read_dir(path).and_then(|mut entries| entries.next().transpose()) {
        Ok(_) => Some(Granted),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        // TCC denies a directory open with EPERM.
        Err(e) if e.raw_os_error() == Some(libc::EPERM) => Some(Missing),
        Err(_) => Some(Unknown),
    }
}

fn worst_status(a: FileAccessStatus, b: FileAccessStatus) -> FileAccessStatus {
    use FileAccessStatus::*;
    match (a, b) {
        (Missing, _) | (_, Missing) => Missing,
        (Unknown, _) | (_, Unknown) => Unknown,
        (NotApplicable, other) | (other, NotApplicable) => other,
        (Granted, Granted) => Granted,
    }
}

fn classify_protected_root(user_home: &Path, root: &Path) -> (ProtectedLocation, PathBuf) {
    for (relative, location) in [
        ("Desktop", ProtectedLocation::Desktop),
        ("Documents", ProtectedLocation::Documents),
        ("Downloads", ProtectedLocation::Downloads),
        ("Library/Mail", ProtectedLocation::ProtectedLibrary),
        ("Library/Messages", ProtectedLocation::ProtectedLibrary),
        ("Library/Safari", ProtectedLocation::ProtectedLibrary),
    ] {
        let base = user_home.join(relative);
        if root.starts_with(&base) {
            return (location, base);
        }
    }
    let volumes = Path::new("/Volumes");
    if let Ok(rest) = root.strip_prefix(volumes)
        && let Some(volume) = rest.components().next()
    {
        return (ProtectedLocation::Volume, volumes.join(volume));
    }
    (ProtectedLocation::Volume, root.to_path_buf())
}

fn probe_local_paths(
    role: &str,
    home: &Path,
    user_home: &Path,
    roots: &[PathBuf],
    read: impl Fn(&Path) -> Option<FileAccessStatus>,
) -> FileAccessReport {
    use FileAccessStatus::*;
    let mut fda_observed = None;
    for relative in FULL_DISK_ACCESS_CANARIES {
        if let Some(st) = read(&user_home.join(relative)) {
            if st == Missing {
                fda_observed = Some(Missing);
                break;
            }
            fda_observed = Some(match fda_observed {
                Some(prev) => worst_status(prev, st),
                None => st,
            });
        }
    }
    let full_disk_access = fda_observed.unwrap_or(Unknown);

    let mut locations: BTreeMap<(ProtectedLocation, PathBuf), FileAccessStatus> = BTreeMap::new();
    for (relative, location) in [
        ("Desktop", ProtectedLocation::Desktop),
        ("Documents", ProtectedLocation::Documents),
        ("Downloads", ProtectedLocation::Downloads),
    ] {
        let candidate = user_home.join(relative);
        if let Some(status) = read(&candidate) {
            locations.insert((location, candidate), status);
        }
    }
    for root in roots {
        let (location, probe_path) = classify_protected_root(user_home, root);
        if let Some(status) = read(&probe_path) {
            let entry = locations.entry((location, probe_path)).or_insert(Granted);
            *entry = worst_status(*entry, status);
        }
    }
    let status = locations.values().copied().fold(Granted, worst_status);
    FileAccessReport {
        status,
        full_disk_access,
        locations: locations
            .into_iter()
            .map(|((location, path), status)| ProtectedAccess {
                location,
                status,
                path: Some(path.to_string_lossy().into_owned()),
            })
            .collect(),
        executable_path: Some(
            tcc_executable_path(role, home)
                .to_string_lossy()
                .into_owned(),
        ),
    }
}

fn probe_live_or_local(role: &str, home: &Path, wait: Duration) -> FileAccessReport {
    if let Some(mut report) = read_socket(&socket_path(role, home), wait)
        && report.status != FileAccessStatus::Unknown
    {
        if report.executable_path.is_none() {
            report.executable_path = Some(
                tcc_executable_path(role, home)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        return report;
    }
    let user_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.to_path_buf());
    let roots = configured_roots(role, home);
    probe_local_paths(role, home, &user_home, &roots, read_folder)
}

fn status_label(value: FileAccessStatus) -> String {
    match value {
        FileAccessStatus::Granted => t!("install.file_access.granted"),
        FileAccessStatus::Missing => t!("install.file_access.missing"),
        FileAccessStatus::Unknown => t!("install.file_access.unknown"),
        FileAccessStatus::NotApplicable => t!("install.file_access.not_applicable"),
    }
}

fn location_label(location: ProtectedLocation) -> String {
    match location {
        ProtectedLocation::Desktop => t!("install.file_access.desktop"),
        ProtectedLocation::Documents => t!("install.file_access.documents"),
        ProtectedLocation::Downloads => t!("install.file_access.downloads"),
        ProtectedLocation::Volume => t!("install.file_access.volume"),
        ProtectedLocation::ProtectedLibrary => t!("install.file_access.library"),
    }
}

fn verify_command(role: &str, home: &Path) -> String {
    let default_home = crate::common::role_home(role, None).ok();
    if default_home.as_deref() == Some(home) {
        format!("steve {role} file-access")
    } else {
        let quoted = home
            .to_str()
            .and_then(|s| crate::common::shell_quote(s).ok())
            .unwrap_or_else(|| format!("'{}'", home.display()));
        format!("steve {role} file-access --home {quoted}")
    }
}

pub(crate) trait GuideDriver {
    fn probe(&mut self, wait: Duration) -> FileAccessReport;
    fn session(&self) -> SessionKind;
    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool>;
    fn open_settings(&mut self, url: &str) -> bool;
    fn emit_done(&mut self, text: &str, detail: &str);
    fn emit_info(&mut self, text: &str);
    fn emit_warn(&mut self, text: &str);
}

struct LiveDriver<'a> {
    role: &'a str,
    home: &'a Path,
    session: SessionKind,
}

impl GuideDriver for LiveDriver<'_> {
    fn probe(&mut self, wait: Duration) -> FileAccessReport {
        probe_live_or_local(self.role, self.home, wait)
    }

    fn session(&self) -> SessionKind {
        self.session
    }

    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool> {
        ui::confirm(prompt, default)
    }

    fn open_settings(&mut self, url: &str) -> bool {
        // `/usr/bin/open` dispatches via LaunchServices IPC and returns immediately; 10s bounds a
        // wedged desktop session without blocking interactive setup.
        const OPEN_SETTINGS_TIMEOUT: Duration = Duration::from_secs(10);
        let mut command = Command::new("/usr/bin/open");
        command.arg(url);
        steve_runtime::process::run_bounded(
            &mut command,
            "/usr/bin/open",
            steve_runtime::process::BoundedOptions::silent("open_settings", OPEN_SETTINGS_TIMEOUT),
        )
        .is_ok_and(|out| out.status.success())
    }

    fn emit_done(&mut self, text: &str, detail: &str) {
        ui::done(text, detail);
    }

    fn emit_info(&mut self, text: &str) {
        ui::info(text);
    }

    fn emit_warn(&mut self, text: &str) {
        ui::warn(text);
    }
}

fn outcome_for(report: &FileAccessReport) -> GuideOutcome {
    match report.status {
        FileAccessStatus::Granted => GuideOutcome::Ready,
        FileAccessStatus::Missing => GuideOutcome::MissingActionRequired,
        FileAccessStatus::NotApplicable => GuideOutcome::NotApplicable,
        FileAccessStatus::Unknown => GuideOutcome::Unknown,
    }
}

fn show_report(report: &FileAccessReport, driver: &mut dyn GuideDriver) {
    if report.status == FileAccessStatus::Granted {
        let detail = if report.locations.is_empty() {
            t!("install.file_access.none_needed")
        } else {
            report
                .locations
                .iter()
                .map(|a| location_label(a.location))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        driver.emit_done(&t!("install.file_access.ready"), &detail);
        return;
    }
    driver.emit_warn(&status_label(report.status));
    if report.full_disk_access != FileAccessStatus::NotApplicable {
        driver.emit_info(&format!(
            "  {} · {}",
            t!("install.file_access.full_disk_access"),
            status_label(report.full_disk_access)
        ));
    }
    for access in &report.locations {
        let folder = location_label(access.location);
        let suffix = access
            .path
            .as_deref()
            .map(|p| format!(" ({p})"))
            .unwrap_or_default();
        driver.emit_info(&format!(
            "  {folder}{suffix} · {}",
            status_label(access.status)
        ));
    }
}

pub(crate) fn run_guide_macos(
    role: &str,
    home: &Path,
    first_boot: bool,
    json: bool,
    driver: &mut dyn GuideDriver,
) -> Result<GuideOutcome> {
    let wait = if first_boot {
        FIRST_BOOT_SOCKET_WAIT
    } else {
        Duration::ZERO
    };
    let report = driver.probe(wait);
    if json {
        println!("{}", serde_json::to_string(&report)?);
        return Ok(outcome_for(&report));
    }
    if report.status == FileAccessStatus::NotApplicable {
        driver.emit_info(&t!("install.file_access.not_applicable"));
        return Ok(GuideOutcome::NotApplicable);
    }
    show_report(&report, driver);
    let command = verify_command(role, home);
    match report.status {
        FileAccessStatus::Granted => Ok(GuideOutcome::Ready),
        FileAccessStatus::NotApplicable => Ok(GuideOutcome::NotApplicable),
        FileAccessStatus::Unknown => {
            driver.emit_info(&t!("install.file_access.unknown_hint", command = command));
            Ok(GuideOutcome::Unknown)
        }
        FileAccessStatus::Missing => {
            let target_path = report
                .executable_path
                .clone()
                .unwrap_or_else(|| tcc_executable_path(role, home).display().to_string());
            if driver.session() == SessionKind::Headless {
                driver.emit_info(&t!(
                    "install.file_access.headless_hint",
                    path = target_path,
                    settings_url = FULL_DISK_ACCESS,
                    command = command
                ));
                return Ok(GuideOutcome::MissingActionRequired);
            }
            driver.emit_info(&t!("install.file_access.enable", path = target_path));
            if driver.confirm(&t!("install.file_access.open"), true)? {
                let opened = driver.open_settings(FULL_DISK_ACCESS);
                if opened && driver.confirm(&t!("install.file_access.recheck"), true)? {
                    let rechecked = driver.probe(Duration::ZERO);
                    show_report(&rechecked, driver);
                    if rechecked.status == FileAccessStatus::Granted {
                        return Ok(GuideOutcome::GuidedAndGranted);
                    }
                }
            }
            driver.emit_info(&t!("install.file_access.verify_later", command = command));
            Ok(GuideOutcome::MissingActionRequired)
        }
    }
}

pub fn guide_for_role(
    role: &str,
    home: &Path,
    first_boot: bool,
    json: bool,
) -> Result<GuideOutcome> {
    if !cfg!(target_os = "macos") {
        let report = FileAccessReport::not_applicable();
        if json {
            println!("{}", serde_json::to_string(&report)?);
        } else {
            ui::info(&t!("install.file_access.not_applicable"));
        }
        return Ok(GuideOutcome::NotApplicable);
    }
    let session = if json {
        SessionKind::Headless
    } else {
        detect_session_kind()
    };
    let mut driver = LiveDriver {
        role,
        home,
        session,
    };
    run_guide_macos(role, home, first_boot, json, &mut driver)
}

pub fn guide(home: &Path, first_boot: bool, json: bool) -> Result<GuideOutcome> {
    guide_for_role("node", home, first_boot, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct MockDriver {
        reports: VecDeque<FileAccessReport>,
        session: SessionKind,
        confirms: VecDeque<bool>,
        open_succeeds: bool,
        opened_urls: Vec<String>,
        done: Vec<(String, String)>,
        info: Vec<String>,
        warn: Vec<String>,
    }

    impl MockDriver {
        fn new(session: SessionKind, reports: Vec<FileAccessReport>) -> Self {
            Self {
                reports: reports.into(),
                session,
                confirms: VecDeque::new(),
                open_succeeds: true,
                opened_urls: Vec::new(),
                done: Vec::new(),
                info: Vec::new(),
                warn: Vec::new(),
            }
        }
    }

    impl GuideDriver for MockDriver {
        fn probe(&mut self, _wait: Duration) -> FileAccessReport {
            self.reports
                .pop_front()
                .unwrap_or_else(FileAccessReport::unknown)
        }
        fn session(&self) -> SessionKind {
            self.session
        }
        fn confirm(&mut self, _prompt: &str, _default: bool) -> Result<bool> {
            Ok(self
                .confirms
                .pop_front()
                .expect("unexpected prompt in fixture"))
        }
        fn open_settings(&mut self, url: &str) -> bool {
            self.opened_urls.push(url.to_owned());
            self.open_succeeds
        }
        fn emit_done(&mut self, text: &str, detail: &str) {
            self.done.push((text.to_owned(), detail.to_owned()));
        }
        fn emit_info(&mut self, text: &str) {
            self.info.push(text.to_owned());
        }
        fn emit_warn(&mut self, text: &str) {
            self.warn.push(text.to_owned());
        }
    }

    fn missing_report(exe: &str) -> FileAccessReport {
        FileAccessReport {
            status: FileAccessStatus::Missing,
            full_disk_access: FileAccessStatus::Missing,
            locations: vec![ProtectedAccess {
                location: ProtectedLocation::Documents,
                status: FileAccessStatus::Missing,
                path: Some("/Users/example/Documents".into()),
            }],
            executable_path: Some(exe.into()),
        }
    }

    fn granted_report(exe: &str) -> FileAccessReport {
        FileAccessReport {
            status: FileAccessStatus::Granted,
            full_disk_access: FileAccessStatus::Granted,
            locations: vec![ProtectedAccess {
                location: ProtectedLocation::Documents,
                status: FileAccessStatus::Granted,
                path: Some("/Users/example/Documents".into()),
            }],
            executable_path: Some(exe.into()),
        }
    }

    #[test]
    fn desktop_guide_silently_passes_when_ready_and_guides_once_when_missing() {
        let home = Path::new("/tmp/steve-hub-home");
        let exe = tcc_executable_path("hub", home)
            .to_string_lossy()
            .into_owned();

        // Already granted: prints clear ready status, no prompts, no System Settings launch.
        let mut ready_driver = MockDriver::new(SessionKind::Desktop, vec![granted_report(&exe)]);
        let outcome = run_guide_macos("hub", home, false, false, &mut ready_driver).unwrap();
        assert_eq!(outcome, GuideOutcome::Ready);
        assert_eq!(outcome.exit_code(), 0);
        assert!(ready_driver.opened_urls.is_empty());
        assert_eq!(ready_driver.done.len(), 1);
        assert!(ready_driver.warn.is_empty());

        // Missing -> desktop user opens settings -> grants -> recheck confirms Ready.
        let mut guide_driver = MockDriver::new(
            SessionKind::Desktop,
            vec![missing_report(&exe), granted_report(&exe)],
        );
        guide_driver.confirms = VecDeque::from([true, true]);
        let outcome = run_guide_macos("hub", home, false, false, &mut guide_driver).unwrap();
        assert_eq!(outcome, GuideOutcome::GuidedAndGranted);
        assert_eq!(outcome.exit_code(), 0);
        assert_eq!(guide_driver.opened_urls, vec![FULL_DISK_ACCESS]);
        assert!(
            guide_driver.info.iter().any(|line| line.contains(&exe)),
            "must name the stable executable path: {:?}",
            guide_driver.info
        );
        assert!(
            guide_driver
                .info
                .iter()
                .any(|line| line.contains("/Users/example/Documents")),
            "must list missing path: {:?}",
            guide_driver.info
        );
        assert_eq!(guide_driver.done.len(), 1);
    }

    #[test]
    fn headless_guide_never_blocks_or_opens_popup_and_returns_explicit_exit_code() {
        let home = Path::new("/tmp/steve-node-home");
        let exe = tcc_executable_path("node", home)
            .to_string_lossy()
            .into_owned();
        let mut driver = MockDriver::new(SessionKind::Headless, vec![missing_report(&exe)]);
        let outcome = run_guide_macos("node", home, false, false, &mut driver).unwrap();
        assert_eq!(outcome, GuideOutcome::MissingActionRequired);
        assert_eq!(outcome.exit_code(), EXIT_ACCESS_MISSING);
        assert!(
            driver.opened_urls.is_empty(),
            "headless mode must never invoke /usr/bin/open"
        );
        assert!(
            driver
                .info
                .iter()
                .any(|line| line.contains("steve node file-access") && line.contains(&exe)),
            "headless hint must give executable path and follow-up CLI command: {:?}",
            driver.info
        );
    }

    #[test]
    fn session_detection_treats_ssh_plain_and_non_console_as_headless() {
        let none = |_: &str| None;
        assert_eq!(
            detect_session_kind_from(true, true, none, true),
            SessionKind::Desktop
        );
        assert_eq!(
            detect_session_kind_from(false, true, none, true),
            SessionKind::Headless
        );
        assert_eq!(
            detect_session_kind_from(true, false, none, true),
            SessionKind::Headless
        );
        assert_eq!(
            detect_session_kind_from(true, true, none, false),
            SessionKind::Headless
        );
        assert_eq!(
            detect_session_kind_from(
                true,
                true,
                |k| (k == "SSH_CONNECTION").then(|| "10.0.0.1 12345 10.0.0.2 22".into()),
                true,
            ),
            SessionKind::Headless
        );
        assert_eq!(
            detect_session_kind_from(true, true, |k| (k == "TERM").then(|| "dumb".into()), true,),
            SessionKind::Headless
        );
    }

    #[test]
    fn local_path_probe_checks_standard_tcc_folders_and_configured_roots() {
        let user_home = Path::new("/Users/example");
        let home = user_home.join(".steve");
        let vol_root = Path::new("/Volumes").join("ExternalDrive");
        let roots = vec![user_home.join("Documents/repo"), vol_root.join("project")];
        let fda_canary = user_home.join("Library/Application Support/com.apple.TCC");
        let desktop = user_home.join("Desktop");
        let documents = user_home.join("Documents");
        let downloads = user_home.join("Downloads");
        let report = probe_local_paths("hub", &home, user_home, &roots, |path| {
            if path == fda_canary {
                Some(FileAccessStatus::Missing)
            } else if path == desktop || path == downloads {
                Some(FileAccessStatus::Granted)
            } else if path == documents || path == vol_root {
                Some(FileAccessStatus::Missing)
            } else {
                None
            }
        });
        assert_eq!(report.status, FileAccessStatus::Missing);
        assert_eq!(report.full_disk_access, FileAccessStatus::Missing);
        assert_eq!(
            report.executable_path.as_deref(),
            Some("/Users/example/.steve/current/bin/steve-hub")
        );
        assert_eq!(report.locations.len(), 4);
        assert!(report.locations.iter().any(|l| {
            l.location == ProtectedLocation::Documents
                && l.status == FileAccessStatus::Missing
                && l.path.as_deref() == Some("/Users/example/Documents")
        }));
        assert!(report.locations.iter().any(|l| {
            l.location == ProtectedLocation::Volume
                && l.status == FileAccessStatus::Missing
                && l.path.as_deref() == vol_root.to_str()
        }));
    }
}
