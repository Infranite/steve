//! The signing keychain's password (ops/sign-setup.sh) moves out of `signing/keychain-pass`, a file
//! every agent could read, into the key store through steve-keyhelper
//! ([`steve_modules::vault::keystore::signing`]). The Hub moves it once at start; the one-click
//! upgrade's runner (the signed Hub again) reads it there and hands it to `local-release.py build` on
//! its stdin. An install not moved yet keeps working from the file.
//!
//! Moving the password does not take back copies of the file: the daily backups made before carry
//! it, and an agent may have read it already. So once the store holds it, the Hub changes the
//! keychain's password to a new one (`security -i`, the passwords on stdin), and the old one opens
//! nothing.
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    /// The password is in the key store and the file is gone; whether it was changed after.
    Moved(Rotated),
    /// The file stays, and why.
    Kept(String),
}

/// What [`rotate`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rotated {
    /// The keychain has a new password, the one in the key store; the old one opens nothing.
    Yes,
    /// The keychain keeps the password it had, and why.
    No(String),
}

/// The signing keychain, as far as a rotation needs it.
pub trait Keychain {
    /// Changes its password from `old` to `new`.
    fn change(&self, old: &str, new: &str) -> Result<()>;
    /// Whether `pass` unlocks it (locked again right after).
    fn opens(&self, pass: &str) -> bool;
}

/// `signing/steve-signing.keychain-db` through `security -i`: the passwords go on its stdin, never
/// on a command line (`ps` shows those to every process of the user).
pub struct Security {
    pub bin: PathBuf,
    pub keychain: PathBuf,
}

impl Security {
    pub fn of(home: &Path) -> Self {
        Self {
            bin: PathBuf::from("/usr/bin/security"),
            keychain: home.join(KEYCHAIN),
        }
    }

    /// Runs one `security -i` line; its status.
    fn line(&self, line: &str) -> Result<bool> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let path = self.keychain.to_string_lossy();
        // what a `security -i` line can carry: no quoting inside it
        if path.contains(['"', '\\', '\n']) {
            anyhow::bail!(
                "{} can't be named on a security line",
                self.keychain.display()
            )
        }
        let mut child = Command::new(&self.bin)
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            let l = Zeroizing::new(format!("{line} \"{path}\"\n"));
            stdin.write_all(l.as_bytes())?;
        }
        Ok(child.wait()?.success())
    }

    fn lock(&self) {
        let _ = std::process::Command::new(&self.bin)
            .arg("lock-keychain")
            .arg(&self.keychain)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

impl Keychain for Security {
    fn change(&self, old: &str, new: &str) -> Result<()> {
        for p in [old, new] {
            if !signing::storable(p) {
                anyhow::bail!("not a password of sign-setup.sh's form")
            }
        }
        let changed = self.line(&format!("set-keychain-password -o {old} -p {new}"));
        self.lock();
        if !changed? {
            anyhow::bail!("security set-keychain-password failed")
        }
        Ok(())
    }

    fn opens(&self, pass: &str) -> bool {
        if !signing::storable(pass) {
            return false;
        }
        let opened = self
            .line(&format!("unlock-keychain -p {pass}"))
            .unwrap_or(false);
        self.lock();
        opened
    }
}

/// The signing keychain sign-setup.sh makes, relative to `STEVE_HOME`.
pub const KEYCHAIN: &str = "signing/steve-signing.keychain-db";

/// Gives `kc` a new password: kept in `store` first (as the next one), then set on the keychain,
/// checked to open it, and only then made the password. A rotation cut short (a crash, a failed
/// `security`) is finished or undone here on the next call: whichever of the two opens the keychain
/// stays. Never leaves the key store without a password that opens it.
pub fn rotate(home: &Path, store: &dyn KeyStore, kc: &dyn Keychain) -> Rotated {
    let no = |why: String| Rotated::No(why);
    // one rotation at a time (a second Hub on this home, a probe during a deploy)
    let _guard = match lock(&home.join("signing/.rotate.lock")) {
        Ok(g) => g,
        Err(e) => return no(format!("lock: {e}")),
    };
    let old = match signing::get(store) {
        Ok(Some(p)) => p,
        Ok(None) => return no("the key store holds no password".into()),
        Err(e) => return no(e.to_string()),
    };
    match signing::get_next(store) {
        Ok(Some(next)) if kc.opens(&next) => {
            // cut short after the keychain changed: finish it
            return match signing::set(store, &next) {
                Ok(()) => {
                    let _ = signing::clear_next(store);
                    Rotated::Yes
                }
                Err(e) => no(e.to_string()),
            };
        }
        Ok(Some(_)) if !kc.opens(&old) => {
            return no("neither the password nor the next one opens the keychain".into());
        }
        Ok(Some(_)) => {
            if let Err(e) = signing::clear_next(store) {
                return no(e.to_string());
            }
        }
        Ok(None) => {}
        Err(e) => return no(e.to_string()),
    }
    if !kc.opens(&old) {
        return no("the stored password does not open the keychain".into());
    }
    let new = match signing::generate() {
        Ok(p) => p,
        Err(e) => return no(e.to_string()),
    };
    if let Err(e) = signing::set_next(store, &new) {
        return no(e.to_string());
    }
    let changed = kc.change(&old, &new);
    if !kc.opens(&new) {
        // unchanged: the next one goes; changed but not opening is left for the next start to sort out
        if kc.opens(&old) {
            let _ = signing::clear_next(store);
        }
        return no(match changed {
            Err(e) => e.to_string(),
            Ok(()) => "the new password does not open the keychain".into(),
        });
    }
    if let Err(e) = signing::set(store, &new) {
        // the keychain takes the next one: the next start finishes this
        return no(e.to_string());
    }
    let _ = signing::clear_next(store);
    Rotated::Yes
}

fn lock(p: &Path) -> std::io::Result<fs::File> {
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(p)?;
    f.lock()?;
    Ok(f)
}

/// Moves `signing/keychain-pass` into `store`: written, read back, then (with `kc`) the keychain's
/// password changed, then the file removed. A file whose password the key store already holds is
/// removed; one that is not of the storable form stays, and so does one that differs from what the
/// store holds, unless it opens the keychain: then the store's is an earlier installation's and is
/// replaced. The file goes even when the change fails: it is the copy every agent can read.
pub fn migrate(home: &Path, store: &dyn KeyStore, kc: Option<&dyn Keychain>) -> Moved {
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
        // an earlier installation's password, left in the key store: this keychain is a new one, and
        // the file's password is the one that opens it
        Ok(Some(_)) if kc.is_some_and(|kc| kc.opens(text.trim())) => {
            if let Err(e) = signing::set(store, &text) {
                return Moved::Kept(e.to_string());
            }
        }
        Ok(Some(_)) => return Moved::Kept("the key store holds a different password".into()),
        Ok(None) => {
            if let Err(e) = signing::set(store, &text) {
                return Moved::Kept(e.to_string());
            }
        }
        Err(e) => return Moved::Kept(e.to_string()),
    }
    drop(text);
    let rotated = match kc {
        Some(kc) => rotate(home, store, kc),
        None => Rotated::No("no keychain to change".into()),
    };
    match fs::remove_file(&file) {
        Ok(()) => Moved::Moved(rotated),
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

/// The vault's own key directory, when the vault keeps its keys in one (`STEVE_VAULT_KEYSTORE=dir`
/// or `file`): the password goes there too, never into the login keychain, where another
/// installation under this user keeps its own (`steve setup --vault-dir`).
fn vault_dir_store(home: &Path) -> Option<Option<Arc<dyn KeyStore>>> {
    let kind = std::env::var("STEVE_VAULT_KEYSTORE").ok()?;
    if !matches!(kind.trim(), "dir" | "file") {
        return None;
    }
    Some(
        steve_modules::vault::keystore::from_env(home, |k| std::env::var(k).ok())
            .inspect_err(|e| tracing::warn!("signing: the vault's key store: {e}"))
            .ok(),
    )
}

/// steve-keyhelper, when it is installed: the only key store the password goes into (its items stay
/// readable across Hub releases, and it answers only a parent signed as the Hub). With the vault's
/// keys in a directory, that directory instead.
#[cfg(target_os = "macos")]
pub fn helper(home: &Path) -> Option<Arc<dyn KeyStore>> {
    if let Some(store) = vault_dir_store(home) {
        return store;
    }
    let configured = std::env::var("STEVE_KEYHELPER").ok();
    steve_modules::vault::keystore::HelperStore::find(home, configured.as_deref())
        .map(|h| Arc::new(h) as Arc<dyn KeyStore>)
}

#[cfg(not(target_os = "macos"))]
pub fn helper(_home: &Path) -> Option<Arc<dyn KeyStore>> {
    None
}

/// steve-keyhelper of `home` itself (`bin/steve-keyhelper`), whatever `STEVE_KEYHELPER` says: the
/// upgrade runner's, whose environment is its caller's to choose. With the vault's keys in a
/// directory, that directory instead.
#[cfg(target_os = "macos")]
pub fn installed_helper(home: &Path) -> Option<Arc<dyn KeyStore>> {
    if let Some(store) = vault_dir_store(home) {
        return store;
    }
    steve_modules::vault::keystore::HelperStore::find(home, None)
        .map(|h| Arc::new(h) as Arc<dyn KeyStore>)
}

#[cfg(not(target_os = "macos"))]
pub fn installed_helper(_home: &Path) -> Option<Arc<dyn KeyStore>> {
    None
}

/// At start: moves the password when there is a file and a helper, and changes it (off the
/// runtime); finishes a change cut short. Not while a release holds `releases/.release.lock`: its
/// build may be about to unlock the keychain with the password it has.
pub fn spawn_migrate(home: PathBuf) {
    let file = home.join(FILE).is_file();
    tokio::task::spawn_blocking(move || {
        if super::runner::held(&home.join("releases/.release.lock")) {
            return;
        }
        let Some(store) = helper(&home) else {
            return;
        };
        let kc = Security::of(&home);
        if !file {
            if matches!(signing::get_next(store.as_ref()), Ok(Some(_))) {
                match rotate(&home, store.as_ref(), &kc) {
                    Rotated::Yes => {
                        tracing::info!("signing: the keychain password change finished")
                    }
                    Rotated::No(why) => tracing::warn!("signing: keychain password change: {why}"),
                }
            }
            return;
        }
        match migrate(&home, store.as_ref(), Some(&kc)) {
            Moved::Nothing => {}
            Moved::Moved(Rotated::Yes) => tracing::info!(
                "signing: the keychain password moved from {FILE} into the key store and was changed"
            ),
            Moved::Moved(Rotated::No(why)) => tracing::warn!(
                "signing: the keychain password moved from {FILE} into the key store, not changed: {why}"
            ),
            Moved::Kept(why) => tracing::warn!("signing: {FILE} stays: {why}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
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

    /// A keychain with a password, that may refuse to change it.
    struct Fake {
        pass: Mutex<String>,
        refuse: Mutex<bool>,
    }

    impl Fake {
        fn with(p: &str) -> Self {
            Self {
                pass: Mutex::new(p.into()),
                refuse: Mutex::new(false),
            }
        }
    }

    impl Keychain for Fake {
        fn change(&self, old: &str, new: &str) -> Result<()> {
            let mut p = self.pass.lock().unwrap();
            if *self.refuse.lock().unwrap() || *p != old {
                anyhow::bail!("refused")
            }
            *p = new.into();
            Ok(())
        }
        fn opens(&self, pass: &str) -> bool {
            *self.pass.lock().unwrap() == pass
        }
    }

    fn stored(store: &dyn KeyStore) -> String {
        signing::get(store).unwrap().unwrap().as_str().to_owned()
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
        assert_eq!(migrate(h.path(), &store, None), Moved::Nothing);
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
        let kc = Fake::with(&pass());
        assert_eq!(
            migrate(h.path(), &store, Some(&kc)),
            Moved::Moved(Rotated::Yes)
        );
        assert!(
            !h.path().join(FILE).exists(),
            "no agent-readable copy is left"
        );
        // a copy of the file (a backup from before, one an agent took) opens nothing any more
        let now = stored(&store);
        assert_ne!(now, pass());
        assert!(signing::storable(&now));
        assert!(kc.opens(&now) && !kc.opens(&pass()));
        assert!(signing::get_next(&store).unwrap().is_none());
        assert!(present(h.path(), Some(&store)));
        assert!(!present(h.path(), None), "only the key store has it now");
        assert_eq!(
            password(h.path(), Some(&store))
                .unwrap()
                .as_deref()
                .map(|p| p.as_str()),
            Some(now.as_str())
        );
        // once moved, a second start has nothing to do
        assert_eq!(migrate(h.path(), &store, Some(&kc)), Moved::Nothing);
    }

    #[test]
    fn a_keychain_that_refuses_the_change_keeps_its_password() {
        let (h, store) = home();
        fs::write(h.path().join(FILE), pass()).unwrap();
        let kc = Fake::with(&pass());
        *kc.refuse.lock().unwrap() = true;
        assert!(matches!(
            migrate(h.path(), &store, Some(&kc)),
            Moved::Moved(Rotated::No(_))
        ));
        // the file still goes; the store keeps the password that opens the keychain
        assert!(!h.path().join(FILE).exists());
        assert_eq!(stored(&store), pass());
        assert!(kc.opens(&pass()));
        assert!(signing::get_next(&store).unwrap().is_none());
        // a later rotation does it
        *kc.refuse.lock().unwrap() = false;
        assert_eq!(rotate(h.path(), &store, &kc), Rotated::Yes);
        assert!(kc.opens(&stored(&store)) && !kc.opens(&pass()));
    }

    #[test]
    fn a_change_cut_short_is_finished_or_undone_next_time() {
        // the keychain changed, the store was not told (the Hub stopped in between)
        let (h, store) = home();
        signing::set(&store, &pass()).unwrap();
        let next = "f".repeat(64);
        signing::set_next(&store, &next).unwrap();
        let kc = Fake::with(&next);
        assert_eq!(rotate(h.path(), &store, &kc), Rotated::Yes);
        assert_eq!(stored(&store), next, "the one the keychain takes is kept");
        assert!(signing::get_next(&store).unwrap().is_none());
        // the keychain did not change: the next one goes, and this rotation goes through
        let (h, store) = home();
        signing::set(&store, &pass()).unwrap();
        signing::set_next(&store, &next).unwrap();
        let kc = Fake::with(&pass());
        assert_eq!(rotate(h.path(), &store, &kc), Rotated::Yes);
        let now = stored(&store);
        assert!(now != pass() && now != next && kc.opens(&now));
        // neither opens it: nothing is thrown away
        let (h, store) = home();
        signing::set(&store, &pass()).unwrap();
        signing::set_next(&store, &next).unwrap();
        let kc = Fake::with(&"e".repeat(64));
        assert!(matches!(rotate(h.path(), &store, &kc), Rotated::No(_)));
        assert_eq!(stored(&store), pass());
        assert!(signing::get_next(&store).unwrap().is_some());
    }

    /// A `security` that records its arguments and stdin, for [`Security`].
    #[cfg(unix)]
    #[test]
    fn security_gets_the_passwords_on_stdin_never_as_arguments() {
        use std::os::unix::fs::PermissionsExt;
        let h = tempfile::tempdir().unwrap();
        let bin = h.path().join("security");
        let log = h.path().join("log");
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"args: $*\" >> {log}\nif [ \"$1\" = -i ]; then cat >> {log}; fi\n",
                log = log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let kc = Security {
            bin,
            keychain: h.path().join("signing/steve-signing.keychain-db"),
        };
        let new = "f".repeat(64);
        kc.change(&pass(), &new).unwrap();
        assert!(kc.opens(&new));
        let got = fs::read_to_string(&log).unwrap();
        for line in got.lines().filter(|l| l.starts_with("args:")) {
            assert!(!line.contains(&pass()) && !line.contains(&new), "{line}");
        }
        assert!(got.contains(&format!(
            "set-keychain-password -o {} -p {new} \"{}\"",
            pass(),
            kc.keychain.display()
        )));
        assert!(got.contains("args: lock-keychain"), "locked again: {got}");
        // a keychain path a security line can't carry is refused, not passed
        let odd = Security {
            bin: kc.bin.clone(),
            keychain: h.path().join("a\"b"),
        };
        assert!(odd.change(&pass(), &new).is_err());
        assert!(!odd.opens(&new));
    }

    #[test]
    fn a_file_the_store_already_holds_goes() {
        let (h, store) = home();
        signing::set(&store, &pass()).unwrap();
        fs::write(h.path().join(FILE), pass()).unwrap();
        assert_eq!(
            migrate(h.path(), &store, None),
            Moved::Moved(Rotated::No("no keychain to change".into()))
        );
        assert!(!h.path().join(FILE).exists());
    }

    #[test]
    fn the_file_stays_when_it_cannot_move() {
        // a password sign-setup.sh did not make
        let (h, store) = home();
        fs::write(h.path().join(FILE), "hunter22").unwrap();
        assert!(matches!(migrate(h.path(), &store, None), Moved::Kept(_)));
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
        assert!(matches!(migrate(h.path(), &Locked, None), Moved::Kept(m) if m.contains("locked")));
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
        assert!(matches!(migrate(h.path(), &store, None), Moved::Kept(_)));
        assert!(h.path().join(FILE).exists());
    }
}
