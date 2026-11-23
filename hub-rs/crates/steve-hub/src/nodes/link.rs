//! How a Node's link reached the Hub (G1): the route its machine is on as seen from here, whether it
//! came encrypted, and through what. Told from the TCP peer and the door it came in by, never from
//! anything the Node says: the devices page labels the Hub → Node edge with it.
//!
//! Doors: the Hub's own port ([`Door::Hub`]: loopback peers, or the TLS-terminating proxies of
//! `STEVE_NODE_PROXY_PEERS`), the tailcat tunnel's listener ([`Door::Tailcat`]: its peer is always
//! loopback, the tunnel's own end) and the Hub's own Node's listener ([`Door::Colocated`]).

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

/// The listener a link came in by (an extension on its router).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Door {
    /// The Hub's own port.
    #[default]
    Hub,
    /// The loopback listener behind `tailcat serve`.
    Tailcat,
    /// The listener of the Hub's own Node in a process of its own (`127.0.0.1`).
    Colocated,
}

/// How a link arrived, as `/api/nodes` shows it in `link` (kept with the Node, [`steve_store::node_directory::admit_link`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arrival {
    /// Where the Node's machine is, as far as the Hub can tell: `loopback` (this machine), `lan`,
    /// `tailnet`, `public`, or `unknown` (a tunnel or a proxy that does not say).
    pub route: String,
    /// It came over TLS (a proxy that terminated it said `https`).
    pub tls: bool,
    /// `direct`, `proxy` (a reverse proxy in front of the Hub) or `tailcat` (its tunnel, encrypted
    /// end to end between the two keys).
    pub via: String,
    /// The address it came from, when known (a proxy's client, not the proxy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    /// When the link was opened (ms).
    #[serde(default)]
    pub since: i64,
}

impl Arrival {
    /// What a link handed to [`super::listener::serve`] directly (tests, in-process) is said to be.
    pub fn unknown() -> Arrival {
        Arrival {
            route: "unknown".into(),
            tls: false,
            via: "direct".into(),
            peer: None,
            since: 0,
        }
    }
}

fn plain(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v) => v.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        ip => ip,
    }
}

/// `loopback`, `tailnet`, `lan` or `public` for an address.
pub fn route_of_ip(ip: IpAddr) -> &'static str {
    let ip = plain(ip);
    if ip.is_loopback() {
        "loopback"
    } else if crate::access::tailnet(&ip) {
        "tailnet"
    } else if crate::access::private(&ip) {
        "lan"
    } else {
        "public"
    }
}

/// The one value of a header that must not be repeated or comma-joined; `None` otherwise.
fn single<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut all = headers.get_all(name).iter();
    let first = all.next()?.to_str().ok()?;
    (all.next().is_none() && !first.contains(',')).then(|| first.trim())
}

/// How a link through `door` from `peer` with `headers` arrived. Only the Hub's own port has
/// proxies in front of it; their forwarding headers are believed because the listener admitted
/// only loopback and configured proxy peers ([`super::Ingress`]).
pub fn arrival(door: Door, peer: Option<SocketAddr>, headers: &HeaderMap) -> Arrival {
    let now = super::now();
    match door {
        Door::Tailcat => Arrival {
            route: "unknown".into(),
            tls: false,
            via: "tailcat".into(),
            peer: None,
            since: now,
        },
        Door::Colocated => Arrival {
            route: "loopback".into(),
            tls: false,
            via: "direct".into(),
            peer: peer.map(|p| plain(p.ip()).to_string()),
            since: now,
        },
        Door::Hub => {
            let tls = single(headers, "x-forwarded-proto").is_some_and(|p| p == "https");
            let forwarded = single(headers, "x-forwarded-for")
                .or_else(|| single(headers, "x-real-ip"))
                .and_then(|v| v.parse::<IpAddr>().ok());
            let peer_ip = peer.map(|p| plain(p.ip()));
            let proxied = forwarded.is_some() || tls || peer_ip.is_some_and(|ip| !ip.is_loopback());
            match (proxied, forwarded) {
                (true, Some(ip)) => Arrival {
                    route: route_of_ip(ip).into(),
                    tls,
                    via: "proxy".into(),
                    peer: Some(plain(ip).to_string()),
                    since: now,
                },
                (true, None) => Arrival {
                    route: "unknown".into(),
                    tls,
                    via: "proxy".into(),
                    peer: None,
                    since: now,
                },
                (false, _) => Arrival {
                    route: peer_ip.map_or("unknown", route_of_ip).into(),
                    tls: false,
                    via: "direct".into(),
                    peer: peer_ip.map(|ip| ip.to_string()),
                    since: now,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn peer(s: &str) -> Option<SocketAddr> {
        Some(s.parse().unwrap())
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn each_door_and_route_is_told_apart() {
        let none = HeaderMap::new();
        let a = arrival(Door::Hub, peer("127.0.0.1:5000"), &none);
        assert_eq!(
            (a.route.as_str(), a.tls, a.via.as_str(), a.peer.as_deref()),
            ("loopback", false, "direct", Some("127.0.0.1"))
        );
        assert!(a.since > 0);
        let a = arrival(Door::Hub, peer("[::ffff:127.0.0.1]:5000"), &none);
        assert_eq!(a.route, "loopback");
        // a proxy says where the Node is and that it came over TLS
        for (from, route) in [
            ("192.168.1.20", "lan"),
            ("100.101.102.103", "tailnet"),
            ("203.0.113.9", "public"),
        ] {
            let mut h = headers(&[("x-forwarded-proto", "https")]);
            h.insert("x-forwarded-for", from.parse().unwrap());
            let a = arrival(Door::Hub, peer("192.0.2.10:443"), &h);
            assert_eq!(
                (a.route.as_str(), a.tls, a.via.as_str(), a.peer.as_deref()),
                (route, true, "proxy", Some(from))
            );
        }
        // a local proxy too
        let a = arrival(
            Door::Hub,
            peer("127.0.0.1:5000"),
            &headers(&[("x-forwarded-for", "203.0.113.9")]),
        );
        assert_eq!(
            (a.route.as_str(), a.tls, a.via.as_str()),
            ("public", false, "proxy")
        );
        // a proxy that does not say who: where it is is unknown
        let a = arrival(
            Door::Hub,
            peer("192.0.2.10:443"),
            &headers(&[("x-forwarded-proto", "https")]),
        );
        assert_eq!(
            (a.route.as_str(), a.tls, a.via.as_str(), a.peer),
            ("unknown", true, "proxy", None)
        );
        // repeated or joined forwarding is not believed
        let a = arrival(
            Door::Hub,
            peer("192.0.2.10:443"),
            &headers(&[
                ("x-forwarded-proto", "https"),
                ("x-forwarded-for", "203.0.113.9, 10.0.0.1"),
            ]),
        );
        assert_eq!(a.route, "unknown");
        let a = arrival(Door::Tailcat, peer("127.0.0.1:5000"), &none);
        assert_eq!(
            (a.route.as_str(), a.tls, a.via.as_str(), a.peer),
            ("unknown", false, "tailcat", None)
        );
        let a = arrival(Door::Colocated, peer("127.0.0.1:5000"), &none);
        assert_eq!((a.route.as_str(), a.via.as_str()), ("loopback", "direct"));
    }

    #[test]
    fn the_json_is_the_api_shape() {
        let a = Arrival {
            route: "lan".into(),
            tls: true,
            via: "proxy".into(),
            peer: Some("192.168.1.20".into()),
            since: 7,
        };
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::json!({"route": "lan", "tls": true, "via": "proxy", "peer": "192.168.1.20", "since": 7})
        );
        assert_eq!(
            serde_json::from_value::<Arrival>(serde_json::to_value(&a).unwrap()).unwrap(),
            a
        );
    }
}
