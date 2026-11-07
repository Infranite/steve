//! Trust the actual TCP peer, never a client-supplied forwarding address.
use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

#[derive(Default)]
pub struct Ingress {
    proxies: Vec<IpAddr>,
}

impl Ingress {
    /// Empty means loopback only. Use exact addresses, not networks or hostnames.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let raw = var("STEVE_NODE_PROXY_PEERS").unwrap_or_default();
        let mut proxies = Vec::new();
        if !raw.trim().is_empty() {
            for entry in raw.split(',') {
                let ip: IpAddr = entry.trim().parse().map_err(|_| {
                    "STEVE_NODE_PROXY_PEERS: use comma-separated exact proxy IP addresses"
                        .to_string()
                })?;
                if ip.is_unspecified() || ip.is_multicast() {
                    return Err("STEVE_NODE_PROXY_PEERS: wildcard and multicast addresses are not proxy peers".into());
                }
                proxies.push(ip);
            }
        }
        Ok(Self { proxies })
    }

    pub fn allows(&self, peer: Option<SocketAddr>, headers: &HeaderMap) -> bool {
        let Some(peer) = peer else { return false };
        if crate::app::is_loopback(&peer) {
            return true;
        }
        if !self.proxies.contains(&peer.ip()) {
            return false;
        }
        // The configured proxy must overwrite this header from its actual ingress scheme.
        // Reject duplicate and comma-joined values rather than choosing a spoofable element.
        let mut proto = headers.get_all("x-forwarded-proto").iter();
        matches!(proto.next(), Some(v) if v.as_bytes() == b"https") && proto.next().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn exact_proxy_and_one_https_assertion_are_required() {
        let ingress = Ingress::from_vars(|_| Some("192.0.2.10,2001:db8::10".into())).unwrap();
        let peer = Some("192.0.2.10:1234".parse().unwrap());
        let mut h = HeaderMap::new();
        assert!(!ingress.allows(peer, &h));
        h.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(ingress.allows(peer, &h));
        assert!(ingress.allows(Some("[2001:db8::10]:1234".parse().unwrap()), &h));
        assert!(!ingress.allows(Some("192.0.2.11:1234".parse().unwrap()), &h));
        assert!(!ingress.allows(None, &h));
        h.insert("x-forwarded-for", HeaderValue::from_static("192.0.2.10"));
        assert!(!ingress.allows(Some("192.0.2.11:1234".parse().unwrap()), &h));
        for value in ["http", "https,http", "http,https", "https, https", ""] {
            h.insert("x-forwarded-proto", value.parse().unwrap());
            assert!(!ingress.allows(peer, &h), "{value}");
        }
        h.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        h.append("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(!ingress.allows(peer, &h));
    }

    #[test]
    fn default_stays_loopback_only_and_bad_configuration_is_rejected() {
        let ingress = Ingress::from_vars(|_| None).unwrap();
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(!ingress.allows(Some("192.0.2.10:1234".parse().unwrap()), &h));
        assert!(ingress.allows(Some("127.0.0.1:1234".parse().unwrap()), &h));
        assert!(ingress.allows(Some("[::1]:1234".parse().unwrap()), &h));
        for value in [
            "*",
            "0.0.0.0",
            "::",
            "192.0.2.0/24",
            "proxy.example",
            "192.0.2.10,",
        ] {
            assert!(
                Ingress::from_vars(|_| Some(value.into())).is_err(),
                "{value}"
            );
        }
    }
}
