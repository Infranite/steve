//! The test guard on outbound connections: with `STEVE_FORBID_REAL_HOSTS=1` every client (GitHub,
//! Feishu, mail, OAuth endpoints, the flow `http` node, git pushes) refuses any host that is not
//! loopback, before a single byte leaves the process. Test Hubs set it so that a fake token can
//! never reach a real provider again (it happened twice with api.github.com).
//!
//! The check is on the host *name* as written, before DNS: `127.0.0.0/8`, `::1` (also as
//! `::ffff:127.x.x.x`) and `localhost` pass, everything else is refused — also a name that would
//! resolve to loopback. The variable is read once per process; only the exact value `1` turns it
//! on. The `*_with` functions take the flag as a parameter, for tests. Tests whose fake resolver maps
//! made-up names to loopback register them with [`allow_test_names`]; only names under the reserved
//! `.test` TLD (RFC 6761) can be registered, so no real host can ever pass that way.

use std::net::IpAddr;
use std::sync::OnceLock;

/// The environment variable.
pub const ENV: &str = "STEVE_FORBID_REAL_HOSTS";

/// A connection refused by the guard. The message names only the host, never a path or token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("测试环境禁止连接外部主机：{0}")]
pub struct HostForbidden(pub String);

/// Turned on by [`forbid_in_this_process`].
static FORCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether `STEVE_FORBID_REAL_HOSTS=1` (read once, then cached), or the guard was turned on in
/// this process.
pub fn forbid_real_hosts() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    FORCED.load(std::sync::atomic::Ordering::Relaxed)
        || *FLAG.get_or_init(|| flag_value(std::env::var(ENV).ok().as_deref()))
}

/// Turns the guard on for the rest of the process, without touching the environment (in-process
/// test Hubs: setting a variable while other threads run is not safe). There is no way back.
pub fn forbid_in_this_process() {
    FORCED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// How a value of the variable is read: on only for `1` (surrounding blanks ignored).
pub fn flag_value(v: Option<&str>) -> bool {
    v.map(str::trim) == Some("1")
}

/// `localhost`, a loopback IPv4 / IPv6 literal (brackets and a trailing dot allowed).
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.');
    let h = h
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(h);
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    h.parse::<IpAddr>()
        .is_ok_and(|ip| ip.to_canonical().is_loopback())
}

/// Fake names a test resolves to loopback itself (see [`allow_test_names`]).
static TEST_NAMES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Test support: lets the guard pass these made-up names, which the test's own resolver maps to
/// loopback (the flow `http` node takes a fake `Lookup`). Only names under the reserved `.test` TLD
/// are taken (others panic), so a real provider can never be let through. Process-wide, no way back.
#[doc(hidden)]
pub fn allow_test_names(names: &[&str]) {
    let mut v = TEST_NAMES.lock().unwrap_or_else(|e| e.into_inner());
    for n in names {
        let n = n.trim().trim_end_matches('.').to_ascii_lowercase();
        assert!(
            n.ends_with(".test") && n.len() > ".test".len(),
            "only reserved .test names can be allowed, not {n}"
        );
        if !v.contains(&n) {
            v.push(n);
        }
    }
}

fn is_test_name(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    h.ends_with(".test")
        && TEST_NAMES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&h)
}

/// [`check_host`] with an explicit flag.
pub fn check_host_with(forbid: bool, host: &str) -> Result<(), HostForbidden> {
    if !forbid || is_loopback_host(host) || is_test_name(host) {
        Ok(())
    } else {
        Err(HostForbidden(short(host)))
    }
}

/// Call right before connecting to `host` (a name or IP literal, no port).
pub fn check_host(host: &str) -> Result<(), HostForbidden> {
    check_host_with(forbid_real_hosts(), host)
}

/// [`check_url`] with an explicit flag.
pub fn check_url_with(forbid: bool, url: &str) -> Result<(), HostForbidden> {
    if !forbid {
        return Ok(());
    }
    match url::Url::parse(url.trim()) {
        // a local file (git's bare test remotes) is no connection
        Ok(u) if u.scheme() == "file" => Ok(()),
        Ok(u) => match u.host_str() {
            Some(h) => check_host_with(true, h),
            None => Err(HostForbidden(short(url))),
        },
        // what cannot be parsed cannot be shown to be loopback
        Err(_) => Err(HostForbidden(short(url))),
    }
}

/// Call right before requesting `url` (also a [`url::Url`], which is `AsRef<str>`).
pub fn check_url(url: impl AsRef<str>) -> Result<(), HostForbidden> {
    check_url_with(forbid_real_hosts(), url.as_ref())
}

/// Test support: lets a test check a client under the real variable without `set_var` (process
/// global, racy in a threaded test run). Call it first with the test's full path
/// (`module::tests::name`, or just `name` in an integration test): in the test process it reruns
/// exactly that test in a child copy of the binary with `STEVE_FORBID_REAL_HOSTS=1`, asserts the
/// child passed and returns `false` (skip the body); in the child, or when the whole run already
/// has the flag, it returns `true` (run the body).
#[doc(hidden)]
pub fn forbidding_child(test: &str) -> bool {
    if forbid_real_hosts() {
        return true;
    }
    let exe = std::env::current_exe().expect("test binary");
    let out = std::process::Command::new(exe)
        .env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
        .env_remove("STEVE_SUPERVISOR_PID")
        .env_remove("STEVE_UPGRADE_ID")
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(ENV, "1")
        .output()
        .expect("run the test again in a child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "{test} under {ENV}=1:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    false
}

/// Test support: an HTTP client whose every request goes through a proxy on a dead loopback port,
/// so a test that points a client at a real host can't reach it even if the guard were broken
/// (the request then fails as a network error instead of [`HostForbidden`]).
#[doc(hidden)]
pub fn dead_end_client() -> reqwest::Client {
    reqwest::Client::builder()
        .proxy(reqwest::Proxy::all("http://127.0.0.1:9").expect("proxy url"))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("http client")
}

/// Hosts come from configuration; keep the message short anyway.
fn short(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() > 100 {
        s.chars().take(100).chain(['…']).collect()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_only_for_one() {
        assert!(flag_value(Some("1")));
        assert!(flag_value(Some(" 1\n")));
        for v in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some("yes"),
            Some("11"),
        ] {
            assert!(!flag_value(v), "{v:?}");
        }
    }

    #[test]
    fn only_registered_test_names_pass() {
        assert!(check_host_with(true, "fixture-a.test").is_err());
        allow_test_names(&["fixture-a.test"]);
        assert_eq!(check_host_with(true, "fixture-a.test"), Ok(()));
        assert_eq!(check_host_with(true, "FIXTURE-A.test."), Ok(()));
        assert_eq!(check_url_with(true, "http://fixture-a.test:9/x"), Ok(()));
        assert!(check_host_with(true, "fixture-b.test").is_err());
        assert!(check_host_with(true, "fixture-a.test.example.com").is_err());
        assert!(std::panic::catch_unwind(|| allow_test_names(&["api.github.com"])).is_err());
        assert!(check_host_with(true, "api.github.com").is_err());
    }

    #[test]
    fn loopback_allowed() {
        for h in [
            "127.0.0.1",
            "127.1.2.3",
            "localhost",
            "LOCALHOST.",
            "::1",
            "[::1]",
            "::ffff:127.0.0.1",
        ] {
            assert_eq!(check_host_with(true, h), Ok(()), "{h}");
        }
        for u in [
            "http://127.0.0.1:9/open-apis",
            "https://localhost:8443/x",
            "http://[::1]:80/",
            "wss://127.0.0.1:1/ws?device_id=1",
            "file:///tmp/remotes",
        ] {
            assert_eq!(check_url_with(true, u), Ok(()), "{u}");
        }
    }

    #[test]
    fn real_hosts_refused() {
        for h in [
            "api.github.com",
            "smtp.example.com",
            "10.0.0.1",
            "0.0.0.0",
            "169.254.169.254",
            "::",
            "localhost.example.com",
            "127.0.0.1.nip.io",
        ] {
            assert_eq!(
                check_host_with(true, h),
                Err(HostForbidden(h.into())),
                "{h}"
            );
        }
        let e = check_url_with(true, "https://api.github.com/user?x=1").unwrap_err();
        assert_eq!(e, HostForbidden("api.github.com".into()));
        assert_eq!(e.to_string(), "测试环境禁止连接外部主机：api.github.com");
        assert!(check_url_with(true, "not a url").is_err());
        assert!(check_url_with(true, "mailto:a@b.c").is_err());
    }

    #[test]
    fn unset_allows_everything() {
        assert_eq!(check_host_with(false, "api.github.com"), Ok(()));
        assert_eq!(check_url_with(false, "https://open.feishu.cn/"), Ok(()));
        assert_eq!(check_url_with(false, "garbage"), Ok(()));
    }

    /// The real variable, read in a child copy of this test binary.
    #[test]
    fn env_var_is_read() {
        if !forbidding_child("net::tests::env_var_is_read") {
            return;
        }
        assert!(forbid_real_hosts());
        assert!(check_url("https://api.github.com/").is_err());
        assert_eq!(check_host("127.0.0.1"), Ok(()));
    }
}
