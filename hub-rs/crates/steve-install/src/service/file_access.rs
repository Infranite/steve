//! The user makes the macOS grant. Observe it through the running boot service.

use std::io::{IsTerminal, Read};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Result;
use steve_i18n::t;
use steve_types::devices::{
    FileAccessReport, FileAccessStatus, NODE_FILE_ACCESS_SOCKET, ProtectedLocation,
};

use crate::ui;

const FULL_DISK_ACCESS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

fn read(home: &Path, wait: Duration) -> FileAccessReport {
    let end = Instant::now() + wait;
    let socket = loop {
        match UnixStream::connect(home.join(NODE_FILE_ACCESS_SOCKET)) {
            Ok(socket) => break socket,
            Err(_) if Instant::now() < end => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return FileAccessReport::unknown(),
        }
    };
    let report = (|| {
        socket.set_read_timeout(Some(Duration::from_secs(8))).ok()?;
        let mut bytes = Vec::new();
        socket.take(16 * 1024).read_to_end(&mut bytes).ok()?;
        serde_json::from_slice(&bytes).ok()
    })();
    report.unwrap_or_else(FileAccessReport::unknown)
}

fn status(value: FileAccessStatus) -> String {
    match value {
        FileAccessStatus::Granted => t!("install.file_access.granted"),
        FileAccessStatus::Missing => t!("install.file_access.missing"),
        FileAccessStatus::Unknown => t!("install.file_access.unknown"),
    }
}

fn show(report: &FileAccessReport) {
    if report.status == FileAccessStatus::Granted && report.locations.is_empty() {
        ui::info(&t!("install.file_access.none_needed"));
        return;
    }
    ui::info(&status(report.status));
    for access in &report.locations {
        let folder = match access.location {
            ProtectedLocation::Desktop => t!("install.file_access.desktop"),
            ProtectedLocation::Documents => t!("install.file_access.documents"),
            ProtectedLocation::Downloads => t!("install.file_access.downloads"),
            ProtectedLocation::Volume => t!("install.file_access.volume"),
            ProtectedLocation::ProtectedLibrary => t!("install.file_access.library"),
        };
        ui::info(&format!("  {folder} · {}", status(access.status)));
    }
}

pub fn guide(home: &Path, first_boot: bool, json: bool) -> Result<()> {
    let interactive = !json && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let wait = if first_boot {
        Duration::from_secs(4)
    } else {
        Duration::ZERO
    };
    let report = read(home, wait);
    if json {
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    show(&report);
    if report.status != FileAccessStatus::Missing {
        return Ok(());
    }
    let runner = home.join(super::boot::RUNNER);
    ui::info(&t!("install.file_access.enable", path = runner.display()));
    if interactive && ui::confirm(&t!("install.file_access.open"), true)? {
        let opened = Command::new("/usr/bin/open")
            .arg(FULL_DISK_ACCESS)
            .status()
            .is_ok_and(|exit| exit.success());
        if opened && ui::confirm(&t!("install.file_access.recheck"), true)? {
            show(&read(home, Duration::ZERO));
        }
    }
    Ok(())
}
