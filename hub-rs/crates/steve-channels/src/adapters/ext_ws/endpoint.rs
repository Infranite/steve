//! Where a connection dials and how it trusts the other end (§1, §3.1, §8): the URL a user entered,
//! checked when the connection is made and again before every dial; the certificate pin; the
//! bearer token's shape; the media origin derived from the URL.

use std::net::IpAddr;

use steve_node_wire::tls::{self, Trust};

/// A checked channel-service URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub url: String,
    pub host: String,
    pub trust: Trust,
}

/// Why a URL, pin or token is refused (shown on the form; never holds the token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    Scheme,
    Shape(String),
    Query,
    PlainNotLoopback,
    Pin,
    Token,
}

impl Refused {
    /// The message in the request's language.
    pub fn message(&self) -> String {
        match self {
            Self::Scheme => steve_i18n::t!("channels.ext_ws.url_scheme"),
            Self::Shape(why) => steve_i18n::t!("channels.ext_ws.url_shape", why = why),
            Self::Query => steve_i18n::t!("channels.ext_ws.url_query"),
            Self::PlainNotLoopback => steve_i18n::t!("channels.ext_ws.url_plain"),
            Self::Pin => steve_i18n::t!("channels.ext_ws.pin"),
            Self::Token => steve_i18n::t!("channels.ext_ws.token_shape"),
        }
    }
}

/// Checks a URL and the optional pin (§1, test vector V2): `wss://` (verified by the webpki roots
/// or the pin), or `ws://` to a literal loopback address only; no userinfo, fragment, `%`, `\`,
/// whitespace, control characters or query.
pub fn check(url: &str, pin: Option<&str>) -> Result<Endpoint, Refused> {
    let url = url.trim();
    let plain = url.starts_with("ws://");
    if !plain && !url.starts_with("wss://") {
        return Err(Refused::Scheme);
    }
    // the dialer's own parser, and its refusals
    let host = tls::host_of(url).map_err(Refused::Shape)?;
    if url.contains('?') {
        return Err(Refused::Query);
    }
    let pin = pin.map(str::trim).filter(|p| !p.is_empty());
    let trust = if plain {
        if !host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
            return Err(Refused::PlainNotLoopback);
        }
        Trust::LoopbackPlaintext
    } else {
        match pin {
            Some(p) => tls::parse_pins(p).map_err(|_| Refused::Pin)?,
            None => Trust::WebPki,
        }
    };
    Ok(Endpoint {
        url: url.to_string(),
        host,
        trust,
    })
}

/// A bearer token: 16–4096 bytes of printable ASCII, no whitespace (§3.1).
pub fn token_ok(t: &str) -> bool {
    (16..=4096).contains(&t.len()) && t.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

impl Endpoint {
    /// The media origin (§1, §8): the same host and port, `wss` → `https`, `ws` → `http`.
    pub fn media_origin(&self) -> String {
        let (scheme, rest) = match self.url.split_once("://") {
            Some(("ws", r)) => ("http", r),
            Some((_, r)) => ("https", r),
            None => ("https", self.url.as_str()),
        };
        let authority = rest.split('/').next().unwrap_or(rest);
        format!("{scheme}://{authority}")
    }

    /// The URL of one media key (§8): `<origin>/v1/media/<escaped key>?chat_id=…&source_id=…`.
    pub fn media_url(
        &self,
        key: &str,
        chat_id: &str,
        source_id: Option<&str>,
        id: Option<&str>,
    ) -> Result<url::Url, Refused> {
        let mut u =
            url::Url::parse(&self.media_origin()).map_err(|e| Refused::Shape(e.to_string()))?;
        u.path_segments_mut()
            .map_err(|_| Refused::Shape("no path".into()))?
            .push("v1")
            .push("media")
            .push(key);
        {
            let mut q = u.query_pairs_mut();
            q.append_pair("chat_id", chat_id);
            if let Some(s) = source_id {
                q.append_pair("source_id", s);
            }
            if let Some(i) = id {
                q.append_pair("id", i);
            }
        }
        Ok(u)
    }

    /// The host the outbound guard checks before a dial.
    pub fn guard(&self) -> Result<(), String> {
        steve_modules::net::check_host(&self.host).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// V2: which URLs are accepted.
    #[test]
    fn v2_urls() {
        for ok in [
            "wss://chat.example.com/ws",
            "ws://127.0.0.1:7788/ws",
            "ws://[::1]:7788/",
        ] {
            assert!(check(ok, None).is_ok(), "{ok}");
        }
        assert_eq!(
            check("ws://localhost:7788/", None),
            Err(Refused::PlainNotLoopback)
        );
        assert_eq!(
            check("ws://192.168.1.5/", None),
            Err(Refused::PlainNotLoopback)
        );
        assert!(matches!(
            check("wss://evil#@127.0.0.1/", None),
            Err(Refused::Shape(_))
        ));
        assert_eq!(
            check("wss://a.example/ws?token=x", None),
            Err(Refused::Query)
        );
        assert!(matches!(
            check("wss://a.example/%2e", None),
            Err(Refused::Shape(_))
        ));
        assert_eq!(check("https://a.example/", None), Err(Refused::Scheme));
    }

    #[test]
    fn pins_and_tokens() {
        let pin = format!("sha256:{}", "ab".repeat(32));
        assert_eq!(
            check("wss://lan.example:9443/ws", Some(&pin)).map(|e| e.trust),
            Ok(Trust::Pin([0xab; 32]))
        );
        assert_eq!(
            check("wss://lan.example/ws", Some("sha256:zz")),
            Err(Refused::Pin)
        );
        assert_eq!(
            check("wss://lan.example/ws", Some(" ")).map(|e| e.trust),
            Ok(Trust::WebPki)
        );
        assert!(token_ok("abcdefghijklmnop"));
        assert!(!token_ok("short"));
        assert!(!token_ok("has a space in it ok"));
        assert!(!token_ok(&"x".repeat(4097)));
    }

    #[test]
    fn the_media_origin_follows_the_url() {
        let e = check("wss://chat.example.com:8443/a/ws", None).unwrap();
        assert_eq!(e.media_origin(), "https://chat.example.com:8443");
        let e = check("ws://127.0.0.1:7788/ws", None).unwrap();
        assert_eq!(e.media_origin(), "http://127.0.0.1:7788");
        let u = e.media_url("k/1 2", "c_1", Some("ev"), None).unwrap();
        assert_eq!(
            u.as_str(),
            "http://127.0.0.1:7788/v1/media/k%2F1%202?chat_id=c_1&source_id=ev"
        );
    }
}
