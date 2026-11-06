//! The signing keychain's password (ops/sign-setup.sh) moves out of `signing/keychain-pass`, a file
//! every agent could read, into the key store through steve-keyhelper
//! ([`steve_modules::vault::keystore::signing`]). The Hub moves it once at start; the one-click
//! upgrade's runner (the signed Hub again) reads it there and hands it to `local-release.py build` on
//! its stdin. An install not moved yet keeps working from the file.
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use steve_modules::vault::keystore::{KeyStore, signing};
use zeroize::Zeroizing;

/// Where sign-setup.sh writes the password, relative to `STEVE_HOME`.
pub const FILE: &str = "signing/keychain-pass";

/// What [`migrate`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moved {
    /// No file: nothing to move.
    Nothing,
    /// The password is in the key store and the file is gone.
    Moved,
    /// The file stays, and why.
    Kept(String),
}

/// Moves `signing/keychain-pass` into `store`: written, read back, then the file is removed. A file
/// whose password the key store already holds is removed; one that differs from what the store holds,
/// or is not of the storable form, stays.
pub fn migrate(home: &Path, store: &dyn KeyStore) -> Moved {
    let file = home.join(FILE);
    let text = match fs::read_to_string(&file) {
        Ok(t) => Zeroizing::new(t),
        Err(_) => return Moved::Nothing,
    };
    if !signing::storable(&text) {
        return Moved::Kept("not 64 lowercase hex digits (ops/sign-setup.sh)".into());
    }
    match signing::get(store) {
        Ok(Some(p)) if p.as_str() == text.trim() => {}
        Ok(Some(_)) => return Moved::Kept("the key store holds a different password".into()),
        Ok(None) => {
            if let Err(e) = signing::set(store, &text) {
                return Moved::Kept(e.to_string());
            }
        }
        Err(e) => return Moved::Kept(e.to_string()),
    }
    match fs::remove_file(&file) {
        Ok(()) => Moved::Moved,
        Err(e) => Moved::Kept(format!("remove {FILE}: {e}")),
    }
}

/// The password for a build: the key store's, else the file's (an install not moved yet).
/// A key store that can't be read now (locked) still leaves the file, when there is one.
pub fn password(home: &Path, store: Option<&dyn KeyStore>) -> Result<Option<Zeroizing<String>>> {
    let stored = match store.map(signing::get) {
        Some(Ok(Some(p))) => return Ok(Some(p)),
        Some(Err(e)) => Some(e),
        Some(Ok(None)) | None => None,
    };
    match fs::read_to_string(home.join(FILE)) {
        Ok(t) => Ok(Some(Zeroizing::new(t.trim().to_owned())).filter(|p| !p.is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match stored {
            Some(e) => Err(anyhow::anyhow!(
                "the signing password in the key store: {e}"
            )),
            None => Ok(None),
        },
        Err(e) => Err(anyhow::anyhow!("read {FILE}: {e}")),
    }
}

/// Whether there is a password, in the file or the key store (what a local upgrade needs).
pub fn present(home: &Path, store: Option<&dyn KeyStore>) -> bool {
    home.join(FILE).is_file() || store.is_some_and(|s| matches!(signing::get(s), Ok(Some(_))))
}

/// steve-keyhelper, when it is installed: the only key store the password goes into (its items stay
/// readable across Hub releases, and it answers only a parent signed as the Hub).
#[cfg(target_os = "macos")]
pub fn helper(home: &Path) -> Option<Box<dyn KeyStore>> {
    let configured = std::env::var("STEVE_KEYHELPER").ok();
    steve_modules::vault::keystore::HelperStore::find(home, configured.as_deref())
        .map(|h| Box::new(h) as Box<dyn KeyStore>)
}

#[cfg(not(target_os = "macos"))]
pub fn helper(_home: &Path) -> Option<Box<dyn KeyStore>> {
    None
}

/// At start: moves the password when there is a file and a helper (off the runtime).
pub fn spawn_migrate(home: PathBuf) {
    if !home.join(FILE).is_file() {
        return;
    }
    tokio::task::spawn_blocking(move || {
        let Some(store) = helper(&home) else {
            return;
        };
        match migrate(&home, store.as_ref()) {
            Moved::Nothing => {}
            Moved::Moved => tracing::info!(
                "signing: the keychain password moved from {FILE} into the key store"
            ),
            Moved::Kept(why) => tracing::warn!("signing: {FILE} stays: {why}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_modules::vault::keystore::{FileKeyStore, KeyStoreError};
    use steve_modules::vault::secret::Kek;

    /// A key store that fails like a locked keychain.
    struct Locked;

    impl KeyStore for Locked {
        fn kind(&self) -> &'static str {
            "keychain"
        }
        fn get(&self, _: &str) -> Result<Option<Kek>, KeyStoreError> {
            Err(KeyStoreError::Locked)
        }
        fn set(&self, _: &str, _: &Kek) -> Result<(), KeyStoreError> {
            Err(KeyStoreError::Locked)
        }
        fn delete(&self, _: &str) -> Result<(), KeyStoreError> {
            Err(KeyStoreError::Locked)
        }
    }

    fn pass() -> String {
        "0123456789abcdef".repeat(4)
    }

    /// A Steve home with a signing directory, and a key store of its own (a temp directory).
    fn home() -> (tempfile::TempDir, FileKeyStore) {
        let h = tempfile::tempdir().unwrap();
        fs::create_dir_all(h.path().join("signing")).unwrap();
        let store = FileKeyStore::open(h.path().join("keys")).unwrap();
        (h, store)
    }

    #[test]
    fn nothing_to_move_without_the_file() {
        let (h, store) = home();
        assert_eq!(migrate(h.path(), &store), Moved::Nothing);
        assert!(!present(h.path(), Some(&store)));
        assert!(password(h.path(), Some(&store)).unwrap().is_none());
    }

    #[test]
    fn the_file_moves_into_the_key_store() {
        let (h, store) = home();
        fs::write(h.path().join(FILE), format!("{}\n", pass())).unwrap();
        assert!(
            present(h.path(), None),
            "the file alone is enough before the move"
        );
        assert_eq!(migrate(h.path(), &store), Moved::Moved);
        assert!(
            !h.path().join(FILE).exists(),
            "no agent-readable copy is left"
        );
        assert_eq!(
            signing::get(&store).unwrap().as_deref().map(|p| p.as_str()),
            Some(pass().as_str())
        );
        assert!(present(h.path(), Some(&store)));
        assert!(!present(h.path(), None), "only the key store has it now");
        assert_eq!(
            password(h.path(), Some(&store))
                .unwrap()
                .as_deref()
                .map(|p| p.as_str()),
            Some(pass().as_str())
        );
        // once moved, a second start has nothing to do
        assert_eq!(migrate(h.path(), &store), Moved::Nothing);
    }

    #[test]
    fn a_file_the_store_already_holds_goes() {
        let (h, store) = home();
        signing::set(&store, &pass()).unwrap();
        fs::write(h.path().join(FILE), pass()).unwrap();
        assert_eq!(migrate(h.path(), &store), Moved::Moved);
        assert!(!h.path().join(FILE).exists());
    }

    #[test]
    fn the_file_stays_when_it_cannot_move() {
        // a password sign-setup.sh did not make
        let (h, store) = home();
        fs::write(h.path().join(FILE), "hunter22").unwrap();
        assert!(matches!(migrate(h.path(), &store), Moved::Kept(_)));
        assert!(h.path().join(FILE).exists());
        assert_eq!(
            password(h.path(), Some(&store))
                .unwrap()
                .as_deref()
                .map(|p| p.as_str()),
            Some("hunter22"),
            "the file still serves a build"
        );
        // a locked keychain: the file stays and still serves a build; without it, the error says why
        let (h, _) = home();
        fs::write(h.path().join(FILE), pass()).unwrap();
        assert!(matches!(migrate(h.path(), &Locked), Moved::Kept(m) if m.contains("locked")));
        assert!(h.path().join(FILE).exists());
        assert_eq!(
            password(h.path(), Some(&Locked))
                .unwrap()
                .as_deref()
                .map(|p| p.as_str()),
            Some(pass().as_str())
        );
        fs::remove_file(h.path().join(FILE)).unwrap();
        let e = password(h.path(), Some(&Locked)).unwrap_err().to_string();
        assert!(e.contains("locked"), "{e}");
        // the key store holds another password: neither is thrown away
        let (h, store) = home();
        signing::set(&store, &"f".repeat(64)).unwrap();
        fs::write(h.path().join(FILE), pass()).unwrap();
        assert!(matches!(migrate(h.path(), &store), Moved::Kept(_)));
        assert!(h.path().join(FILE).exists());
    }
}
