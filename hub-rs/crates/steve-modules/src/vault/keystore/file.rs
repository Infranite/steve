//! `FileKeyStore`: one 0600 file per entry in a 0700 directory. For tests and development only; the
//! keys sit unprotected on disk, so it refuses the real `~/.steve`.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::{KeyStore, KeyStoreError, decode, encode};
use crate::vault::secret::Kek;

pub struct FileKeyStore {
    dir: PathBuf,
}

impl std::fmt::Debug for FileKeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileKeyStore")
            .field("dir", &self.dir)
            .finish()
    }
}

fn io(e: std::io::Error) -> KeyStoreError {
    KeyStoreError::Other(e.kind().to_string())
}

impl FileKeyStore {
    /// A store in `dir` (created 0700 if missing). Prefer [`FileKeyStore::for_steve_home`], which
    /// applies the `~/.steve` guard.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, KeyStoreError> {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(io)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(io)?;
        Ok(Self { dir })
    }

    /// A store over a directory the caller has already checked ([`super::DirKeyStore`]).
    pub(super) fn existing(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub(super) fn dir(&self) -> &Path {
        &self.dir
    }

    /// The store at `<steve_home>/vault-keys`. Refuses (before touching the disk) when
    /// `steve_home` is `<user_home>/.steve`, or when the user home is unknown.
    pub fn for_steve_home(
        steve_home: &Path,
        user_home: Option<&Path>,
    ) -> Result<Self, KeyStoreError> {
        let user_home = user_home.ok_or_else(|| {
            KeyStoreError::Refused("STEVE_VAULT_KEYSTORE=file needs HOME to be set".into())
        })?;
        if same_path(steve_home, &user_home.join(".steve")) {
            return Err(KeyStoreError::Refused(
                "STEVE_VAULT_KEYSTORE=file is not allowed for ~/.steve; use the keychain".into(),
            ));
        }
        Self::open(steve_home.join("vault-keys"))
    }

    fn path(&self, account: &str) -> Result<PathBuf, KeyStoreError> {
        // `<ws>/kek/v<n>` → `<ws>__kek__v<n>`; anything that could leave the directory is refused
        let ok = !account.is_empty()
            && account
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/' | '.'))
            && !account
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..");
        if !ok {
            return Err(KeyStoreError::Other("invalid account name".into()));
        }
        Ok(self.dir.join(account.replace('/', "__")))
    }
}

/// Equal after resolving symlinks where the paths exist, lexically otherwise.
pub(super) fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    a == b || canon(a) == canon(b)
}

impl KeyStore for FileKeyStore {
    fn kind(&self) -> &'static str {
        "file"
    }

    fn get(&self, account: &str) -> Result<Option<Kek>, KeyStoreError> {
        match fs::read(self.path(account)?) {
            Ok(b) => {
                let b = zeroize::Zeroizing::new(b);
                decode(&b).map(Some)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(e)),
        }
    }

    fn set(&self, account: &str, key: &Kek) -> Result<(), KeyStoreError> {
        let path = self.path(account)?;
        let tmp = path.with_extension("tmp");
        let _ = fs::remove_file(&tmp);
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(io)?;
        f.write_all(encode(key).as_bytes()).map_err(io)?;
        f.sync_all().map_err(io)?;
        fs::rename(&tmp, &path).map_err(io)
    }

    fn delete(&self, account: &str) -> Result<(), KeyStoreError> {
        match fs::remove_file(self.path(account)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_delete_with_private_permissions() {
        let dir = tempfile::tempdir().expect("tmp");
        let ks = FileKeyStore::open(dir.path().join("keys")).expect("open");
        assert!(ks.get("default/kek/v1").expect("get").is_none());
        let k = Kek::generate().expect("random");
        ks.set("default/kek/v1", &k).expect("set");
        let back = ks.get("default/kek/v1").expect("get").expect("some");
        assert_eq!(back.bytes(), k.bytes());
        let file = dir.path().join("keys/default__kek__v1");
        let mode = fs::metadata(&file).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dmode = fs::metadata(dir.path().join("keys"))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dmode, 0o700);
        // replace
        let k2 = Kek::generate().expect("random");
        ks.set("default/kek/v1", &k2).expect("set");
        assert_eq!(
            ks.get("default/kek/v1")
                .expect("get")
                .expect("some")
                .bytes(),
            k2.bytes()
        );
        ks.delete("default/kek/v1").expect("delete");
        ks.delete("default/kek/v1").expect("delete twice");
        assert!(ks.get("default/kek/v1").expect("get").is_none());
    }

    #[test]
    fn malformed_entry_and_bad_names() {
        let dir = tempfile::tempdir().expect("tmp");
        let ks = FileKeyStore::open(dir.path()).expect("open");
        fs::write(dir.path().join("w__kek__v1"), "not base64 at all").expect("write");
        assert_eq!(ks.get("w/kek/v1").err(), Some(KeyStoreError::Malformed));
        for bad in ["", "../x", "a/../b", "a//b", "a b", "/abs"] {
            assert!(ks.get(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn refuses_the_real_steve_home() {
        // a fake user home, so the guard is tested without going near the real ~/.steve
        let home = tempfile::tempdir().expect("tmp");
        let real = home.path().join(".steve");
        let e = FileKeyStore::for_steve_home(&real, Some(home.path())).expect_err("refused");
        assert!(matches!(e, KeyStoreError::Refused(_)));
        assert!(!real.exists(), "nothing created before refusing");
        // trailing slash / symlink to it are the same directory
        fs::create_dir(&real).expect("mkdir");
        let link = home.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        assert!(FileKeyStore::for_steve_home(&link, Some(home.path())).is_err());
        assert!(FileKeyStore::for_steve_home(&real.join(""), Some(home.path())).is_err());
        assert!(!real.join("vault-keys").exists());
        assert!(FileKeyStore::for_steve_home(&real, None).is_err());
        // a temp STEVE_HOME is fine
        let other = home.path().join("dev-home");
        let ks = FileKeyStore::for_steve_home(&other, Some(home.path())).expect("ok");
        assert_eq!(ks.kind(), "file");
        assert!(other.join("vault-keys").is_dir());
    }

    #[test]
    fn from_env_selects_and_guards() {
        let home = tempfile::tempdir().expect("tmp");
        let h = home.path().to_string_lossy().to_string();
        let env = |file: bool| {
            let h = h.clone();
            move |k: &str| match k {
                "STEVE_VAULT_KEYSTORE" if file => Some("file".to_string()),
                "HOME" => Some(h.clone()),
                _ => None,
            }
        };
        let ks = crate::vault::keystore::from_env(&home.path().join("dev"), env(true)).expect("ok");
        assert_eq!(ks.kind(), "file");
        assert!(crate::vault::keystore::from_env(&home.path().join(".steve"), env(true)).is_err());
        let bad = |k: &str| (k == "STEVE_VAULT_KEYSTORE").then(|| "s3".to_string());
        assert!(crate::vault::keystore::from_env(home.path(), bad).is_err());
    }
}
