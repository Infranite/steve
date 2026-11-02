//! Envelope encryption (accounts-vault.md §3.1): the workspace KEK wraps one DEK per item, the DEK
//! encrypts that item's secret fields. Both layers are XChaCha20-Poly1305 with a fresh random 24-byte
//! nonce per write, stored as `nonce || ciphertext+tag`. The AAD binds every ciphertext to its
//! workspace, item, and field (or KEK version), so a blob copied to another row fails to open.
//!
//! Decryption of field values is `pub(crate)` on purpose: §10 rule 2 allows it in exactly three call
//! sites (reveal/finish, injection, `with_credential`), all inside this module.

use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use data_encoding::BASE32_NOPAD;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::secret::{Dek, Kek, RecoveryCode, SecretBytes};

const NONCE: usize = 24;
const TAG: usize = 16;
/// Recovery code: 32 key bytes + 3 checksum bytes = 280 bits = 56 base32 characters.
const CHECKSUM: usize = 3;
const GROUPS: usize = 8;

/// Why a crypto operation failed. Carries no data: the cause is never worth a secret in a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// Wrong key, wrong AAD (blob moved to another row), or tampered/truncated ciphertext.
    #[error("密文无法解开")]
    Open,
    #[error("系统随机数不可用")]
    Random,
}

fn cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(key.into())
}

fn seal(key: &[u8; 32], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0u8; NONCE];
    getrandom::fill(&mut nonce).map_err(|_| CryptoError::Random)?;
    let ct = cipher(key)
        .encrypt(&nonce.into(), Payload { msg: plain, aad })
        .map_err(|_| CryptoError::Open)?;
    let mut out = Vec::with_capacity(NONCE + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn open(key: &[u8; 32], aad: &[u8], blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if blob.len() < NONCE + TAG {
        return Err(CryptoError::Open);
    }
    let (nonce, ct) = blob.split_at(NONCE);
    let nonce: [u8; NONCE] = nonce.try_into().map_err(|_| CryptoError::Open)?;
    cipher(key)
        .decrypt(&nonce.into(), Payload { msg: ct, aad })
        .map(Zeroizing::new)
        .map_err(|_| CryptoError::Open)
}

fn dek_aad(ws: &str, item_id: &str, kek_version: i64) -> Vec<u8> {
    format!("steve/v1/dek|{ws}|{item_id}|{kek_version}").into_bytes()
}

fn field_aad(ws: &str, item_id: &str, field_id: &str) -> Vec<u8> {
    format!("steve/v1/field|{ws}|{item_id}|{field_id}").into_bytes()
}

/// `vault_items.dek_wrapped` for `item_id` under KEK `kek_version`.
pub fn wrap_dek(
    kek: &Kek,
    ws: &str,
    item_id: &str,
    kek_version: i64,
    dek: &Dek,
) -> Result<Vec<u8>, CryptoError> {
    seal(kek.bytes(), &dek_aad(ws, item_id, kek_version), dek.bytes())
}

/// Opens a wrapped DEK. Fails if any of `ws`, `item_id`, `kek_version` differ from wrapping time.
pub fn unwrap_dek(
    kek: &Kek,
    ws: &str,
    item_id: &str,
    kek_version: i64,
    wrapped: &[u8],
) -> Result<Dek, CryptoError> {
    let plain = open(kek.bytes(), &dek_aad(ws, item_id, kek_version), wrapped)?;
    Dek::from_slice(&plain).ok_or(CryptoError::Open)
}

/// `vault_fields.value_ct` for one secret field.
pub fn seal_field(
    dek: &Dek,
    ws: &str,
    item_id: &str,
    field_id: &str,
    plain: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    seal(dek.bytes(), &field_aad(ws, item_id, field_id), plain)
}

/// The plaintext of one secret field. Only for the three call sites of §10 rule 2: `items/reveal.rs`, `items/credential.rs`, and V4's injection.
pub(crate) fn open_field(
    dek: &Dek,
    ws: &str,
    item_id: &str,
    field_id: &str,
    ct: &[u8],
) -> Result<SecretBytes, CryptoError> {
    let mut plain = open(dek.bytes(), &field_aad(ws, item_id, field_id), ct)?;
    Ok(SecretBytes::new(std::mem::take(&mut *plain)))
}

/// `hex(sha256("steve/kek-fp" || KEK))[:16]`: identifies a KEK in `vault_keys` without revealing it.
pub fn fingerprint(kek: &Kek) -> String {
    let d = Sha256::new()
        .chain_update(b"steve/kek-fp")
        .chain_update(kek.bytes())
        .finalize();
    d[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn checksum(kek: &[u8; 32]) -> [u8; CHECKSUM] {
    let d = Sha256::new()
        .chain_update(b"steve/recovery")
        .chain_update(kek)
        .finalize();
    let mut c = [0u8; CHECKSUM];
    c.copy_from_slice(&d[..CHECKSUM]);
    c
}

/// The recovery code for a KEK: base32 (RFC 4648) of `KEK || checksum`, eight dash-separated groups
/// of seven, e.g. `ABCDEFG-HIJKLMN-…`. The checksum catches typos before the fingerprint lookup.
pub fn recovery_code(kek: &Kek) -> RecoveryCode {
    let mut raw = Zeroizing::new([0u8; 32 + CHECKSUM]);
    raw[..32].copy_from_slice(kek.bytes());
    raw[32..].copy_from_slice(&checksum(kek.bytes()));
    let enc = Zeroizing::new(BASE32_NOPAD.encode(raw.as_ref()));
    let group = enc.len() / GROUPS;
    let mut out = Zeroizing::new(String::with_capacity(enc.len() + GROUPS));
    for (i, ch) in enc.chars().enumerate() {
        if i > 0 && i % group == 0 {
            out.push('-');
        }
        out.push(ch);
    }
    RecoveryCode::new(out)
}

/// Parses what the user typed: case, spaces and dashes don't matter, and the look-alikes `0`, `1`, `8`
/// (not in the base32 alphabet) are read as `O`, `I`, `B`. `None` on any error, checksum included.
pub fn parse_recovery_code(input: &str) -> Option<Kek> {
    let mut norm = Zeroizing::new(String::with_capacity(input.len()));
    for c in input.chars() {
        match c {
            ' ' | '-' | '\t' | '\n' | '\r' => {}
            '0' => norm.push('O'),
            '1' => norm.push('I'),
            '8' => norm.push('B'),
            c => norm.push(c.to_ascii_uppercase()),
        }
    }
    let raw = Zeroizing::new(BASE32_NOPAD.decode(norm.as_bytes()).ok()?);
    if raw.len() != 32 + CHECKSUM {
        return None;
    }
    let kek = Kek::from_slice(&raw[..32])?;
    (checksum(kek.bytes()) == raw[32..]).then_some(kek)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Kek {
        Kek::generate().expect("random")
    }

    #[test]
    fn field_round_trip_and_fresh_nonces() {
        let dek = Dek::generate().expect("random");
        let a = seal_field(&dek, "ws", "vi_1", "vf_1", b"hunter2").expect("seal");
        let b = seal_field(&dek, "ws", "vi_1", "vf_1", b"hunter2").expect("seal");
        assert_ne!(a, b, "every write uses a new nonce");
        assert_ne!(a[..NONCE], b[..NONCE]);
        assert_eq!(a.len(), NONCE + 7 + TAG);
        let p = open_field(&dek, "ws", "vi_1", "vf_1", &a).expect("open");
        assert_eq!(p.expose(), b"hunter2");
    }

    #[test]
    fn field_bound_to_workspace_item_and_field() {
        let dek = Dek::generate().expect("random");
        let ct = seal_field(&dek, "ws", "vi_1", "vf_1", b"secret").expect("seal");
        assert_eq!(
            open_field(&dek, "ws", "vi_2", "vf_1", &ct).err(),
            Some(CryptoError::Open)
        );
        assert!(open_field(&dek, "ws", "vi_1", "vf_2", &ct).is_err());
        assert!(open_field(&dek, "w2", "vi_1", "vf_1", &ct).is_err());
        let other = Dek::generate().expect("random");
        assert!(open_field(&other, "ws", "vi_1", "vf_1", &ct).is_err());
        let mut bad = ct.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(open_field(&dek, "ws", "vi_1", "vf_1", &bad).is_err());
        assert!(open_field(&dek, "ws", "vi_1", "vf_1", &ct[..30]).is_err());
        assert!(open_field(&dek, "ws", "vi_1", "vf_1", &[]).is_err());
    }

    #[test]
    fn dek_bound_to_item_and_kek_version() {
        let kek = key();
        let dek = Dek::generate().expect("random");
        let w = wrap_dek(&kek, "ws", "vi_1", 1, &dek).expect("wrap");
        let back = unwrap_dek(&kek, "ws", "vi_1", 1, &w).expect("unwrap");
        assert_eq!(back.bytes(), dek.bytes());
        assert!(unwrap_dek(&kek, "ws", "vi_2", 1, &w).is_err());
        assert!(unwrap_dek(&kek, "ws", "vi_1", 2, &w).is_err());
        assert!(unwrap_dek(&kek, "w2", "vi_1", 1, &w).is_err());
        assert!(unwrap_dek(&key(), "ws", "vi_1", 1, &w).is_err());
    }

    #[test]
    fn recovery_code_round_trip() {
        let kek = key();
        let code = recovery_code(&kek);
        let groups: Vec<&str> = code.expose().split('-').collect();
        assert_eq!(groups.len(), 8);
        assert!(groups.iter().all(|g| g.len() == 7));
        let back = parse_recovery_code(code.expose()).expect("parse");
        assert_eq!(back.bytes(), kek.bytes());
        // lower case, spaces instead of dashes
        let typed = code.expose().to_lowercase().replace('-', " ");
        assert!(parse_recovery_code(&typed).is_some());
        // O typed as 0
        let o = code.expose().replace('O', "0");
        assert!(parse_recovery_code(&o).is_some());
        assert_eq!(fingerprint(&back), fingerprint(&kek));
        assert_eq!(fingerprint(&kek).len(), 16);
    }

    #[test]
    fn recovery_code_typo_rejected() {
        let code = recovery_code(&key());
        let s = code.expose();
        let first = s.chars().next().expect("non-empty");
        let swapped = if first == 'A' { 'B' } else { 'A' };
        let typo = format!("{swapped}{}", &s[1..]);
        assert!(parse_recovery_code(&typo).is_none());
        assert!(parse_recovery_code(&s[..s.len() - 1]).is_none());
        assert!(parse_recovery_code("").is_none());
        assert!(parse_recovery_code("not a code!").is_none());
    }

    #[test]
    fn secrets_have_opaque_debug() {
        let kek = key();
        let hex: String = kek.bytes().iter().map(|b| format!("{b:02x}")).collect();
        let code = recovery_code(&kek);
        for s in [
            format!("{kek:?}"),
            format!("{:?}", Dek::generate().expect("random")),
            format!("{code:?}"),
            format!("{:?}", SecretBytes::new(b"hunter2".to_vec())),
            format!("{:?}", CryptoError::Open),
        ] {
            assert!(!s.contains(&hex) && !s.contains("hunter2") && !s.contains(code.expose()));
            assert!(s.len() < 40, "{s}");
        }
    }
}
