//! What machine this is (`node.host`): its name, its system and version, its processor and how
//! much it has of them. Read once: none of it changes while the Node runs. Each figure comes from
//! the platform's own account of itself; one it cannot give is left out.

use std::sync::OnceLock;

use steve_node_api::HostInfo;

static HOST: OnceLock<HostInfo> = OnceLock::new();

/// This machine, read the first time it is asked (it may run a short command: call it off the
/// async workers).
pub(crate) fn this_machine() -> HostInfo {
    HOST.get_or_init(read).clone()
}

fn read() -> HostInfo {
    let (os_name, os_version) = system();
    HostInfo {
        hostname: name(),
        os: std::env::consts::OS.into(),
        os_name,
        os_version,
        arch: std::env::consts::ARCH.into(),
        cpus: std::thread::available_parallelism()
            .ok()
            .map(|n| n.get() as u32),
        mem_total: crate::vitals::memory().0,
    }
}

fn clean(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.chars().filter(|c| !c.is_control()).take(128).collect())
}

/// The host name the kernel has.
fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `buf.len()` bytes into it
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    ok.then(|| String::from_utf8_lossy(&buf[..end]).into_owned())
        .and_then(|h| clean(&h))
}

/// macOS: the computer name the owner gave it (`scutil --get ComputerName`), else the host name.
#[cfg(target_os = "macos")]
fn name() -> Option<String> {
    std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| clean(&String::from_utf8_lossy(&o.stdout)))
        .or_else(hostname)
}

#[cfg(not(target_os = "macos"))]
fn name() -> Option<String> {
    hostname()
}

/// macOS and its version (`kern.osproductversion`).
#[cfg(target_os = "macos")]
fn system() -> (Option<String>, Option<String>) {
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: the name is NUL-terminated and `buf` has the room passed in `len`
    let ok = unsafe {
        libc::sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    let end = buf[..len.min(buf.len())]
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(len.min(buf.len()));
    let version = ok
        .then(|| String::from_utf8_lossy(&buf[..end]).into_owned())
        .and_then(|v| clean(&v));
    (Some("macOS".into()), version)
}

/// The distribution and its version, as `/etc/os-release` names them.
#[cfg(target_os = "linux")]
fn system() -> (Option<String>, Option<String>) {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    let (name, version) = os_release(&text);
    (name.or_else(|| Some("Linux".into())), version)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn system() -> (Option<String>, Option<String>) {
    (None, None)
}

/// `NAME` and `VERSION_ID` of an os-release file (quotes removed).
#[cfg(any(target_os = "linux", test))]
fn os_release(text: &str) -> (Option<String>, Option<String>) {
    let value = |key: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
            .and_then(|v| clean(&v))
    };
    (value("NAME"), value("VERSION_ID"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_names_the_distribution_and_its_version() {
        let text = "PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\n";
        assert_eq!(
            os_release(text),
            (Some("Ubuntu".into()), Some("24.04".into()))
        );
        assert_eq!(os_release("ID=x\n"), (None, None));
        assert_eq!(os_release("NAME=''\n"), (None, None));
    }

    #[test]
    fn this_machine_says_what_it_is() {
        let h = this_machine();
        assert_eq!(h.os, std::env::consts::OS);
        assert_eq!(h.arch, std::env::consts::ARCH);
        assert!(h.cpus.is_some_and(|n| n > 0));
        assert!(h.mem_total.is_some_and(|m| m > 0));
        assert!(h.hostname.as_deref().is_some_and(|n| !n.is_empty()));
        if cfg!(target_os = "macos") {
            assert_eq!(h.os_name.as_deref(), Some("macOS"));
            assert!(h.os_version.is_some());
        }
        // read once
        assert_eq!(this_machine(), h);
    }
}
