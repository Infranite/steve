//! The password of the release signing keychain (`$STEVE_HOME/signing/steve-signing.keychain-db`,
//! ops/sign-setup.sh), kept as one more entry of the key store instead of a plain file.
//!
//! Whoever holds that password can sign a binary as the Hub, and steve-keyhelper hands the vault's
//! master keys to anything signed as the Hub. As a 0600 file it was readable by every agent (they run
//! as the same user). In the key store, through steve-keyhelper, only the signed Hub reads it; the
//! one-click upgrade gets it from the Hub over a pipe, and anything else meets a macOS prompt.
//!
//! sign-setup.sh makes the password with `openssl rand -hex 32`: 64 lowercase hex digits, stored as
//! the 32 bytes they spell (the helper stores 32-byte values only). Any other form stays in its file.

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

/// Whether `pass` can be kept in the key store (see the module docs).
pub fn storable(pass: &str) -> bool {
    encode(pass).is_some()
}

/// Where a new password waits while the keychain's own password changes to it (the Hub rotates
/// it once it has moved out of the file, since copies of the file may outlive it): read back at
/// start when a rotation was cut short, to find which of the two the keychain takes.
pub const NEXT_ACCOUNT: &str = "steve/signing/keychain-pass-next";

/// The stored password, `None` when there is none.
pub fn get(store: &dyn KeyStore) -> Result<Option<Zeroizing<String>>, KeyStoreError> {
    get_at(store, ACCOUNT)
}

/// Stores `pass` and reads it back; `Malformed` when it is not of the storable form.
pub fn set(store: &dyn KeyStore, pass: &str) -> Result<(), KeyStoreError> {
    set_at(store, ACCOUNT, pass)
}

/// The password a rotation was moving to, `None` when none was cut short.
pub fn get_next(store: &dyn KeyStore) -> Result<Option<Zeroizing<String>>, KeyStoreError> {
    get_at(store, NEXT_ACCOUNT)
}

/// Keeps `pass` as the one a rotation moves to, read back.
pub fn set_next(store: &dyn KeyStore, pass: &str) -> Result<(), KeyStoreError> {
    set_at(store, NEXT_ACCOUNT, pass)
}

/// Forgets the password a rotation was moving to.
pub fn clear_next(store: &dyn KeyStore) -> Result<(), KeyStoreError> {
    store.delete(NEXT_ACCOUNT)
}

/// A new password of the form sign-setup.sh makes (64 lowercase hex digits).
pub fn generate() -> Result<Zeroizing<String>, KeyStoreError> {
    let mut raw = Zeroizing::new([0u8; 32]);
    getrandom::fill(raw.as_mut())
        .map_err(|e| KeyStoreError::Other(format!("random signing password: {e}")))?;
    Ok(Zeroizing::new(data_encoding::HEXLOWER.encode(raw.as_ref())))
}

fn get_at(store: &dyn KeyStore, account: &str) -> Result<Option<Zeroizing<String>>, KeyStoreError> {
    Ok(store
        .get(account)?
        .map(|k| Zeroizing::new(data_encoding::HEXLOWER.encode(k.bytes()))))
}

fn set_at(store: &dyn KeyStore, account: &str, pass: &str) -> Result<(), KeyStoreError> {
    let k = encode(pass).ok_or(KeyStoreError::Malformed)?;
    store.set(account, &k)?;
    match get_at(store, account)? {
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
        // the one a rotation moves to is kept apart, and goes
        let next = generate().unwrap();
        assert!(storable(&next));
        assert_ne!(next.as_str(), generate().unwrap().as_str());
        set_next(&s, &next).unwrap();
        assert_eq!(
            get_next(&s).unwrap().as_deref().map(|p| p.as_str()),
            Some(next.as_str())
        );
        assert_eq!(
            get(&s).unwrap().as_deref().map(|p| p.as_str()),
            Some(pass.as_str())
        );
        clear_next(&s).unwrap();
        assert!(get_next(&s).unwrap().is_none());
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
            assert!(!storable(p), "{p}");
            assert_eq!(set(&s, p), Err(KeyStoreError::Malformed), "{p}");
        }
        assert!(s.0.lock().unwrap().is_empty());
    }
}
