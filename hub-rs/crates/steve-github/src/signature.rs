//! Webhook signatures (`X-Hub-Signature-256`).
//!
//! GitHub signs the raw request body with HMAC-SHA256 keyed by the webhook
//! secret and sends `sha256=<lowercase hex>`. Verification must run on the
//! exact bytes received (never on re-serialised JSON) and compare in constant
//! time. See <https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries>.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// Header GitHub puts the SHA-256 signature in.
pub const SIGNATURE_HEADER: &str = "X-Hub-Signature-256";

const PREFIX: &str = "sha256=";

type HmacSha256 = Hmac<Sha256>;

fn mac(secret: &[u8], body: &[u8]) -> HmacSha256 {
    // HMAC accepts keys of any length, so this cannot fail.
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(body);
    mac
}

/// The header value GitHub would send for `body`: `sha256=<hex>`.
pub fn sign(secret: &[u8], body: &[u8]) -> String {
    format!(
        "{PREFIX}{}",
        hex::encode(mac(secret, body).finalize().into_bytes())
    )
}

/// Checks a received `X-Hub-Signature-256` header against `body`.
///
/// Returns false for a missing prefix, malformed hex, wrong length or a
/// mismatch; callers answer all of those with the same bare 401. An empty
/// secret never verifies, so an unconfigured connection cannot accept
/// unsigned-looking deliveries.
pub fn verify(secret: &[u8], body: &[u8], header: &str) -> bool {
    if secret.is_empty() {
        return false;
    }
    let Some(hex_sig) = header.trim().strip_prefix(PREFIX) else {
        return false;
    };
    let Ok(sig) = hex::decode(hex_sig) else {
        return false;
    };
    // `verify_slice` compares in constant time and rejects wrong lengths.
    mac(secret, body).verify_slice(&sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vector from GitHub's docs page "Validating webhook deliveries".
    const DOC_SECRET: &[u8] = b"It's a Secret to Everybody";
    const DOC_BODY: &[u8] = b"Hello, World!";
    const DOC_SIG: &str = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";

    #[test]
    fn matches_github_doc_vector() {
        assert_eq!(sign(DOC_SECRET, DOC_BODY), DOC_SIG);
        assert!(verify(DOC_SECRET, DOC_BODY, DOC_SIG));
    }

    #[test]
    fn rejects_tampering_and_malformed_headers() {
        assert!(!verify(DOC_SECRET, b"Hello, World?", DOC_SIG));
        assert!(!verify(b"other secret", DOC_BODY, DOC_SIG));
        // sha1-style header, missing prefix, bad hex, truncated, empty.
        assert!(!verify(
            DOC_SECRET,
            DOC_BODY,
            &DOC_SIG.replace("sha256=", "sha1=")
        ));
        assert!(!verify(
            DOC_SECRET,
            DOC_BODY,
            DOC_SIG.trim_start_matches("sha256=")
        ));
        assert!(!verify(DOC_SECRET, DOC_BODY, "sha256=zz"));
        assert!(!verify(DOC_SECRET, DOC_BODY, &DOC_SIG[..DOC_SIG.len() - 2]));
        assert!(!verify(DOC_SECRET, DOC_BODY, ""));
    }

    #[test]
    fn uppercase_hex_and_whitespace_are_accepted() {
        let upper = format!("sha256={}", DOC_SIG[7..].to_uppercase());
        assert!(verify(DOC_SECRET, DOC_BODY, &upper));
        assert!(verify(DOC_SECRET, DOC_BODY, &format!(" {DOC_SIG}\r")));
    }

    #[test]
    fn empty_secret_never_verifies() {
        let sig = sign(b"", DOC_BODY);
        assert!(!verify(b"", DOC_BODY, &sig));
    }
}
