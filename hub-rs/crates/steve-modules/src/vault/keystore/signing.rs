//! The signing password lives in the installation's key store as a 32-byte value.
//! The installer provisions it through the signed Hub and steve-keyhelper. The runner
//! reads it for bundle signing, and a manual signing tool needs the operator's approval.

use zeroize::Zeroizing;

use super::{KeyStore, KeyStoreError};
use crate::vault::secret::Kek;

/// The key store account of the signing keychain's password.
pub const ACCOUNT: &str = "steve/signing/keychain-pass";

/// The 32 bytes `pass` spells, if it is 64 lowercase hex digits (surrounding whitespace allowed).
fn encode(pass: &str) -> Option<Kek> {
    let pass = pass.trim();
    if pass.len() != 64 || !pass.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let raw = Zeroizing::new(data_encoding::HEXLOWER.decode(pass.as_bytes()).ok()?);
    Kek::from_slice(&raw)
}

/// The stored password, `None` when there is none.
pub fn get(store: &dyn KeyStore) -> Result<Option<Zeroizing<String>>, KeyStoreError> {
    Ok(store
        .get(ACCOUNT)?
        .map(|k| Zeroizing::new(data_encoding::HEXLOWER.encode(k.bytes()))))
}

/// Interactive macOS login-keychain lookup for an operator-invoked terminal upgrade command
/// running from an unsigned release binary (which `steve-keyhelper` refuses by code signature).
#[cfg(target_os = "macos")]
pub fn get_interactive_keychain(service: &str) -> Result<Option<Zeroizing<String>>, KeyStoreError> {
    Ok(super::KeychainStore::login()
        .get_interactive(service, ACCOUNT)?
        .map(|k| Zeroizing::new(data_encoding::HEXLOWER.encode(k.bytes()))))
}

/// Normalize a signing keychain password passed via `--keychain-pass-stdin`: accepts either the
/// raw 64-hex signing keychain password or the 32-byte base64 Keychain item value printed by
/// `security find-generic-password -a steve/signing/keychain-pass -w`.
pub fn normalize_stdin_password(raw: &str) -> Zeroizing<String> {
    let trimmed = raw.trim();
    if let Ok(kek) = super::decode(trimmed.as_bytes()) {
        return Zeroizing::new(data_encoding::HEXLOWER.encode(kek.bytes()));
    }
    Zeroizing::new(trimmed.to_owned())
}

/// Stores `pass` and reads it back; `Malformed` when it is not 64 lowercase hex digits.
pub fn set(store: &dyn KeyStore, pass: &str) -> Result<(), KeyStoreError> {
    let k = encode(pass).ok_or(KeyStoreError::Malformed)?;
    store.set(ACCOUNT, &k)?;
    match get(store)? {
        Some(back) if back.as_str() == pass.trim() => Ok(()),
        _ => Err(KeyStoreError::Other(
            "the signing password did not read back".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Mem(Mutex<HashMap<String, [u8; 32]>>);

    impl KeyStore for Mem {
        fn kind(&self) -> &'static str {
            "file"
        }
        fn get(&self, account: &str) -> Result<Option<Kek>, KeyStoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(account)
                .and_then(|b| Kek::from_slice(b)))
        }
        fn set(&self, account: &str, key: &Kek) -> Result<(), KeyStoreError> {
            self.0.lock().unwrap().insert(account.into(), *key.bytes());
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<(), KeyStoreError> {
            self.0.lock().unwrap().remove(account);
            Ok(())
        }
    }

    #[test]
    fn a_sign_setup_password_round_trips() {
        let s = Mem::default();
        assert!(get(&s).unwrap().is_none());
        let pass = "0123456789abcdef".repeat(4);
        set(&s, &format!("{pass}\n")).unwrap();
        assert_eq!(
            get(&s).unwrap().as_deref().map(|p| p.as_str()),
            Some(pass.as_str())
        );
        // its own account, apart from the workspace keys
        assert!(s.0.lock().unwrap().contains_key(ACCOUNT));
        assert!(!ACCOUNT.contains("/kek/"));
    }

    #[test]
    fn other_passwords_are_not_storable() {
        let s = Mem::default();
        for p in [
            "",
            "hunter2",
            &"0123456789ABCDEF".repeat(4),
            &"0123456789abcdef".repeat(3),
            &format!("{}g", "0".repeat(63)),
        ] {
            assert_eq!(set(&s, p), Err(KeyStoreError::Malformed), "{p}");
        }
        assert!(s.0.lock().unwrap().is_empty());
    }
}
