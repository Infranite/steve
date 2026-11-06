//! How the Node reaches the Hub. `wss://` always, verified by a pin when pairing gave one (the
//! Hub terminates TLS itself) or by the webpki roots when it did not (a proxy with a public
//! certificate in front). Plain `ws://` only to a literal loopback address and only when
//! `STEVE_NODE_LOOPBACK_PLAINTEXT=1`: same machine, nothing leaves it, and the signed challenge is
//! still required.
//!
//! The pin is sha256 of the Hub's leaf certificate (DER), `sha256:<hex>`: a rotated certificate
//! needs a new pin, which is what a pin is for.

use std::net::IpAddr;
use std::sync::Arc;

use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_tungstenite::Connector;

use crate::envelope::{FrameSink, FrameStream};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    Pin([u8; 32]),
    WebPki,
    LoopbackPlaintext,
}

pub fn parse_pin(s: &str) -> Result<[u8; 32], String> {
    let h = s.strip_prefix("sha256:").unwrap_or(s);
    hex::decode(h)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| "a pin is sha256:<64 hex digits>".to_string())
}

pub fn pin_of(cert_der: &[u8]) -> String {
    format!("sha256:{}", hex::encode(crate::identity::sha256(cert_der)))
}

/// The host a `ws://` or `wss://` URL connects to, read by the parser the connection itself uses
/// (`http::Uri`, what tungstenite dials), brackets of an IPv6 literal taken off. Refused outright:
/// userinfo (`@`), a fragment (`#`), percent escapes, backslashes, whitespace and control
/// characters anywhere in it. Parsers disagree on exactly those (`ws://evil#@127.0.0.1/` is
/// `evil` to the connection and was `127.0.0.1` to a hand-made split), and a Hub URL never needs
/// them.
pub fn host_of(url: &str) -> Result<String, String> {
    if url.contains('@') {
        return Err("the hub url must not contain userinfo (@)".into());
    }
    if url.contains('#') {
        return Err("the hub url must not contain a fragment (#)".into());
    }
    if url
        .chars()
        .any(|c| c == '%' || c == '\\' || c.is_whitespace() || c.is_control())
    {
        return Err("the hub url must not contain %, \\, spaces or control characters".into());
    }
    let uri: http::Uri = url
        .parse()
        .map_err(|e| format!("the hub url does not parse: {e}"))?;
    let host = uri
        .authority()
        .map(|a| a.host())
        .filter(|h| !h.is_empty())
        .ok_or("the hub url has no host")?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    Ok(host.to_string())
}

/// The trust a URL gets. `allow_plaintext` is `STEVE_NODE_LOOPBACK_PLAINTEXT=1`.
pub fn policy(url: &str, pin: Option<&str>, allow_plaintext: bool) -> Result<Trust, String> {
    let plain = url.starts_with("ws://");
    if !plain && !url.starts_with("wss://") {
        return Err("the hub url must be wss:// (or ws:// to loopback)".into());
    }
    let host = host_of(url)?;
    if !plain {
        return match pin {
            Some(p) => Ok(Trust::Pin(parse_pin(p)?)),
            None => Ok(Trust::WebPki),
        };
    }
    let loopback = host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if !loopback {
        return Err("plain ws:// is only allowed to a literal 127.0.0.1 or ::1".into());
    }
    if !allow_plaintext {
        return Err("plain ws:// to loopback needs STEVE_NODE_LOOPBACK_PLAINTEXT=1".into());
    }
    Ok(Trust::LoopbackPlaintext)
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct Pinned {
    pin: [u8; 32],
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        use subtle_eq::ct_eq;
        if ct_eq(&crate::identity::sha256(end_entity.as_ref()), &self.pin) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "hub certificate does not match the pin".into(),
            ))
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algs.supported_schemes()
    }
}

mod subtle_eq {
    /// Constant time, so a pin comparison leaks nothing about how much of it matched.
    pub fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

pub fn client_config(trust: &Trust) -> Result<Option<Arc<rustls::ClientConfig>>, String> {
    let p = provider();
    let b = rustls::ClientConfig::builder_with_provider(p.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?;
    Ok(match trust {
        Trust::LoopbackPlaintext => None,
        Trust::Pin(pin) => Some(Arc::new(
            b.dangerous()
                .with_custom_certificate_verifier(Arc::new(Pinned {
                    pin: *pin,
                    algs: p.signature_verification_algorithms,
                }))
                .with_no_client_auth(),
        )),
        Trust::WebPki => {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            Some(Arc::new(
                b.with_root_certificates(roots).with_no_client_auth(),
            ))
        }
    })
}

/// Opens the WebSocket under `trust`.
pub async fn connect(url: &str, trust: &Trust) -> std::io::Result<(FrameSink, FrameStream)> {
    let cfg = client_config(trust).map_err(std::io::Error::other)?;
    let connector = match cfg {
        Some(c) => Connector::Rustls(c),
        None => Connector::Plain,
    };
    let (ws, _) = tokio_tungstenite::connect_async_tls_with_config(
        url,
        Some(crate::ws::config()),
        false,
        Some(connector),
    )
    .await
    .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(crate::ws::split(ws))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_only_to_loopback_and_only_when_asked() {
        assert_eq!(
            policy("ws://127.0.0.1:7777/node/ws", None, true),
            Ok(Trust::LoopbackPlaintext)
        );
        assert_eq!(
            policy("ws://[::1]:7777/node/ws", None, true),
            Ok(Trust::LoopbackPlaintext)
        );
        assert!(policy("ws://127.0.0.1:7777/node/ws", None, false).is_err());
        assert!(
            policy("ws://localhost:7777/", None, true).is_err(),
            "a name is not a literal"
        );
        assert!(policy("ws://10.0.0.2:7777/", None, true).is_err());
        assert!(policy("ws://evil.example@127.0.0.1.example/", None, true).is_err());
        assert!(policy("http://127.0.0.1/", None, true).is_err());
        assert_eq!(policy("wss://hub.example/", None, false), Ok(Trust::WebPki));
        let pin = format!("sha256:{}", "ab".repeat(32));
        assert_eq!(
            policy("wss://hub.example/", Some(&pin), false),
            Ok(Trust::Pin([0xab; 32]))
        );
        assert!(policy("wss://hub.example/", Some("sha256:zz"), false).is_err());
    }

    /// M2: the guard reads the host the connection dials, and what parsers disagree on is refused.
    #[test]
    fn plaintext_guard_cannot_be_tricked_into_a_remote_host() {
        // the connection (http::Uri) goes to evil.example here; a hand-made split saw 127.0.0.1
        let uri: http::Uri = "ws://evil.example#@127.0.0.1/".parse().unwrap();
        assert_eq!(uri.host(), Some("evil.example"));
        for url in [
            "ws://evil.example#@127.0.0.1/",
            "ws://evil.example#@127.0.0.1:7777/node/ws",
            "ws://evil.example/#@127.0.0.1",
            "ws://127.0.0.1#@evil.example/",
            "ws://user@127.0.0.1/",
            "ws://user:pw@127.0.0.1:7777/node/ws",
            "ws://evil.example@127.0.0.1/",
            "ws://127.0.0.1@evil.example/",
            "ws://evil.example%40127.0.0.1/",
            "ws://127.0.0.1%40evil.example/",
            "ws://127.0.0.1%2f@evil.example/",
            "ws://127.0.0.1\\@evil.example/",
            "ws://127.0.0.1\\evil.example/",
            "ws://127.0.0.1 @evil.example/",
            "ws://127.0.0.1./",
            "ws://127.0.0.1.:7777/node/ws",
            "ws://127.1/",
            "ws://0x7f.0.0.1/",
            "ws://2130706433/",
            "ws://[::ffff:127.0.0.1]/",
            "ws://[::1%25lo0]/",
            "ws://[::1]@evil.example/",
            "ws://evil.example?@127.0.0.1/",
            "ws:///127.0.0.1/",
            "ws://",
        ] {
            assert!(
                policy(url, None, true).is_err(),
                "{url} must not count as loopback"
            );
        }
        for url in [
            "ws://127.0.0.1/",
            "ws://127.0.0.2:7777/node/ws",
            "ws://[::1]/node/ws",
            "ws://[0:0:0:0:0:0:0:1]:7777/node/ws",
        ] {
            assert_eq!(
                policy(url, None, true),
                Ok(Trust::LoopbackPlaintext),
                "{url}"
            );
        }
        // userinfo and fragments are refused over TLS too: the hub url never needs them
        assert!(policy("wss://user@hub.example/", None, false).is_err());
        assert!(policy("wss://hub.example/#x", None, false).is_err());
        assert_eq!(
            host_of("wss://hub.example:8443/node/ws"),
            Ok("hub.example".into())
        );
        assert_eq!(host_of("ws://[::1]:7777/"), Ok("::1".into()));
    }
}
