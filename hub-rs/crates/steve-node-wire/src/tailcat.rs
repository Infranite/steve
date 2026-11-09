//! Reaching a Hub over tailcat (A6), an optional external program: <https://github.com/tailscale/tailcat>.
//!
//! The Hub runs `tailcat serve <port>` in front of a loopback listener that answers the Node routes
//! only; a Node given `tailcat:<address>:<port>` as its Hub runs `tailcat forward` and dials the
//! local end in plain `ws://`. The tunnel is WireGuard between the two keys, and the address names
//! the Hub's key, so it stands where TLS and a pin would. Neither side links tailcat in: without the
//! program the feature is just unavailable.

use std::path::{Path, PathBuf};

pub const SCHEME: &str = "tailcat:";

/// `tailcat:<address>:<port>` → the address and the port it serves; `None` for any other URL.
pub fn parse(hub_url: &str) -> Option<(&str, u16)> {
    let rest = hub_url.trim().strip_prefix(SCHEME)?;
    let (addr, port) = rest.rsplit_once(':')?;
    let port = port.parse::<u16>().ok().filter(|p| *p != 0)?;
    // addresses are base64url (`tc…`); nothing else reaches the command line
    let ok = addr.len() >= 8
        && addr.len() <= 512
        && addr.starts_with("tc")
        && addr
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then_some((addr, port))
}

pub fn hub_url(addr: &str, port: u16) -> String {
    format!("{SCHEME}{addr}:{port}")
}

/// `STEVE_TAILCAT` when set, else `tailcat` on `PATH` or in the usual install places (a service's
/// `PATH` is often shorter than a shell's).
pub fn program() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("STEVE_TAILCAT").filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return is_file(&p).then_some(p);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(
            ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/snap/bin"]
                .into_iter()
                .map(PathBuf::from),
        )
        .map(|d| d.join("tailcat"))
        .find(|p| is_file(p))
}

fn is_file(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file())
}

/// The address in a `tailcat serve` or `genkey` line (`… : tcXXXX` or a bare `tcXXXX`).
pub fn address_in(line: &str) -> Option<&str> {
    let last = line.trim().rsplit([' ', ':']).next()?;
    parse(&format!("{SCHEME}{last}:1")).map(|_| last)
}

/// The local end `tailcat forward` reports: `# forwarding 127.0.0.1:53540 -> remote …`.
pub fn forwarding_in(line: &str) -> Option<std::net::SocketAddr> {
    let rest = line.trim().strip_prefix("# forwarding ")?;
    rest.split_whitespace().next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_lines_parse_and_nothing_else_does() {
        let a = "tcpGFwWCB4nRddSA5iOpp6k0zTflr20JYFvzjEHFNyeJ";
        assert_eq!(parse(&hub_url(a, 8442)), Some((a, 8442)));
        assert_eq!(parse("https://hub.example"), None);
        assert_eq!(parse(&format!("tailcat:{a}:0")), None);
        assert_eq!(parse(&format!("tailcat:{a};rm -rf:1")), None);
        assert_eq!(parse("tailcat:x:1"), None);
        assert_eq!(
            address_in(&format!("# 🐈 Server listening with saved key \"/k/hub.private.json\": {a}")),
            Some(a)
        );
        assert_eq!(address_in(a), Some(a));
        assert_eq!(address_in("# wrote file to /k/hub.private.json"), None);
        assert_eq!(
            forwarding_in("# forwarding 127.0.0.1:53540 -> remote localhost:8442"),
            Some("127.0.0.1:53540".parse().unwrap())
        );
        assert_eq!(forwarding_in("# Selected bootstrap relay region 304"), None);
    }
}
