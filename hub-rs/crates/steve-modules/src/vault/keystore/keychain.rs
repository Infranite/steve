//! `KeychainStore`: generic-password items (service [`SERVICE`]) in the login keychain, or in an
//! explicit keychain file (tests use a throwaway one).
//!
//! The Hub runs under launchd with nobody at the screen, so every call runs with keychain user
//! interaction disabled: a locked keychain or a missing ACL entry fails fast (`Locked` / `Denied`)
//! instead of waiting forever on a dialog. Making releases not prompt at all needs the stable code
//! signature (manual step on the Mac mini).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use core_foundation::base::TCFType;
use security_framework::base::Error as SecError;
use security_framework::os::macos::keychain::SecKeychain;

use super::{KeyStore, KeyStoreError, SERVICE, decode, encode};
use crate::vault::secret::Kek;

const ERR_ITEM_NOT_FOUND: i32 = -25300;
const ERR_INTERACTION_NOT_ALLOWED: i32 = -25308;
const ERR_AUTH_FAILED: i32 = -25293;
const ERR_USER_CANCELED: i32 = -128;
const ERR_NO_ACCESS: i32 = -25243; // errSecNoAccessForItem
const ERR_INTERACTION_REQUIRED: i32 = -25315;

/// Disabling user interaction is process-wide and not counted, so calls are serialized.
static INTERACTION: Mutex<()> = Mutex::new(());

pub struct KeychainStore {
    /// `None`: the user's default (login) keychain.
    file: Option<PathBuf>,
}

impl std::fmt::Debug for KeychainStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeychainStore")
            .field("file", &self.file)
            .finish()
    }
}

// Not bound by security-framework. Deprecated like the whole file-keychain API, which is the one that
// supports explicit keychain files and the login keychain of a launchd agent.
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecKeychainGetStatus(keychain: *const std::ffi::c_void, status: *mut u32) -> i32;
}
const UNLOCKED: u32 = 1; // kSecUnlockStateStatus

/// `Some(true)` if the keychain is locked.
fn is_locked(kc: &SecKeychain) -> Option<bool> {
    let mut status = 0u32;
    // SAFETY: `kc` is a live SecKeychainRef for the duration of the call; `status` is a valid out pointer.
    let rc = unsafe { SecKeychainGetStatus(kc.as_concrete_TypeRef().cast(), &mut status) };
    (rc == 0).then_some(status & UNLOCKED == 0)
}

/// With user interaction off, a locked keychain answers errSecAuthFailed or errSecInteractionNotAllowed,
/// the same codes an ACL refusal can give, so the lock state is asked for explicitly. An unlocked
/// keychain that still wants interaction is the ACL asking whether to trust this binary (a release not
/// signed with the stable identity), not a lock.
fn classify(kc: &SecKeychain, e: SecError) -> KeyStoreError {
    match is_locked(kc) {
        Some(true) => KeyStoreError::Locked,
        Some(false)
            if matches!(
                e.code(),
                ERR_INTERACTION_NOT_ALLOWED | ERR_INTERACTION_REQUIRED
            ) =>
        {
            KeyStoreError::Denied
        }
        _ => map(e),
    }
}

fn map(e: SecError) -> KeyStoreError {
    match e.code() {
        ERR_INTERACTION_NOT_ALLOWED | ERR_INTERACTION_REQUIRED => KeyStoreError::Locked,
        ERR_AUTH_FAILED | ERR_USER_CANCELED | ERR_NO_ACCESS => KeyStoreError::Denied,
        code => KeyStoreError::Other(format!("OSStatus {code}")),
    }
}

impl KeychainStore {
    /// The production store: the login keychain of the user the Hub runs as.
    pub fn login() -> Self {
        Self { file: None }
    }

    /// A specific keychain file (must already exist and be unlocked), for the ignored integration test.
    pub fn at(path: impl AsRef<Path>) -> Self {
        Self {
            file: Some(path.as_ref().to_path_buf()),
        }
    }

    fn with<T>(
        &self,
        f: impl FnOnce(&SecKeychain) -> Result<T, KeyStoreError>,
    ) -> Result<T, KeyStoreError> {
        let _serial = INTERACTION.lock().unwrap_or_else(|e| e.into_inner());
        let _no_ui = SecKeychain::disable_user_interaction().map_err(map)?;
        let kc = match &self.file {
            Some(p) => SecKeychain::open(p),
            None => SecKeychain::default(),
        }
        .map_err(map)?;
        f(&kc)
    }
}

impl KeyStore for KeychainStore {
    fn kind(&self) -> &'static str {
        "keychain"
    }

    fn get(&self, account: &str) -> Result<Option<Kek>, KeyStoreError> {
        self.with(|kc| match kc.find_generic_password(SERVICE, account) {
            // the Security buffer is freed by the framework; we only keep a zeroizing decode
            Ok((pw, _item)) => decode(pw.as_ref()).map(Some),
            Err(e) if e.code() == ERR_ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(classify(kc, e)),
        })
    }

    fn set(&self, account: &str, key: &Kek) -> Result<(), KeyStoreError> {
        let value = encode(key);
        self.with(|kc| {
            kc.set_generic_password(SERVICE, account, value.as_bytes())
                .map_err(|e| classify(kc, e))
        })
    }

    fn delete(&self, account: &str) -> Result<(), KeyStoreError> {
        self.with(|kc| match kc.find_generic_password(SERVICE, account) {
            Ok((_pw, item)) => {
                item.delete();
                Ok(())
            }
            Err(e) if e.code() == ERR_ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(classify(kc, e)),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    /// Creates a throwaway keychain file in a temp dir with `security create-keychain`, writes, reads and
    /// deletes a KEK through `KeychainStore::at`, then deletes the keychain. Never touches the login
    /// keychain or the search list. Run with `cargo test -p steve-modules -- --ignored keychain`.
    #[test]
    #[ignore = "uses the macOS security tool; run once by hand"]
    fn keychain_round_trip_in_throwaway_keychain() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("steve-test.keychain-db");
        let pw = "steve-test-only";
        let run = |args: &[&str]| {
            let out = Command::new("/usr/bin/security")
                .args(args)
                .output()
                .expect("security");
            assert!(
                out.status.success(),
                "security {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let before = Command::new("/usr/bin/security")
            .args(["list-keychains", "-d", "user"])
            .output()
            .expect("list");
        let p = path.to_str().expect("utf-8 path");
        /// Deletes the throwaway keychain even when an assertion fails.
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Command::new("/usr/bin/security")
                    .args(["delete-keychain", &self.0])
                    .output();
            }
        }
        run(&["create-keychain", "-p", pw, p]);
        let cleanup = Cleanup(p.to_string());
        run(&["unlock-keychain", "-p", pw, p]);
        // no auto-lock while the test runs
        run(&["set-keychain-settings", p]);

        let ks = KeychainStore::at(&path);
        let acct = "ws_test/kek/v1";
        assert!(ks.get(acct).expect("get").is_none());
        let k = Kek::generate().expect("random");
        ks.set(acct, &k).expect("set");
        assert_eq!(ks.get(acct).expect("get").expect("some").bytes(), k.bytes());
        let k2 = Kek::generate().expect("random");
        ks.set(acct, &k2).expect("replace");
        assert_eq!(
            ks.get(acct).expect("get").expect("some").bytes(),
            k2.bytes()
        );
        ks.delete(acct).expect("delete");
        assert!(ks.get(acct).expect("get").is_none());

        // locked keychain: reading an existing entry fails fast as Locked instead of prompting
        ks.set(acct, &k).expect("set");
        run(&["lock-keychain", p]);
        assert_eq!(ks.get(acct).err(), Some(KeyStoreError::Locked));

        drop(cleanup);
        assert!(!path.exists(), "throwaway keychain deleted");
        let after = Command::new("/usr/bin/security")
            .args(["list-keychains", "-d", "user"])
            .output()
            .expect("list");
        assert_eq!(before.stdout, after.stdout, "search list unchanged");
    }
}
