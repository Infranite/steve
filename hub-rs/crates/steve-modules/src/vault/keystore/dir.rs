//! `DirKeyStore` (`STEVE_VAULT_KEYSTORE=dir`): one 0600 file per key in `STEVE_VAULT_KEY_DIR`, for
//! headless servers without a keyring. The keys are only as safe as that directory, so it must be a
//! 0700 directory owned by the Hub's user and must not be inside (or contain) `STEVE_HOME`: the
//! database and its backups must never sit next to the key that opens them. Put it on separate
//! protected storage (an encrypted volume, a root-managed mount, a systemd `LoadCredential` dir, …).

use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::file::{FileKeyStore, same_path};
use super::{KeyStore, KeyStoreError, Kind};
use crate::vault::secret::Kek;

#[derive(Debug)]
pub struct DirKeyStore(FileKeyStore);

fn refused(msg: String) -> KeyStoreError {
    KeyStoreError::Refused(format!("STEVE_VAULT_KEY_DIR: {msg}"))
}

/// The path with symlinks resolved; for a path that does not exist yet, its resolved parent plus the
/// last component.
fn resolve(p: &Path) -> Option<PathBuf> {
    if let Ok(c) = fs::canonicalize(p) {
        return Some(c);
    }
    let parent = fs::canonicalize(p.parent()?).ok()?;
    Some(parent.join(p.file_name()?))
}

impl DirKeyStore {
    /// Opens (creating it 0700 if missing; its parent must exist) the key directory `dir`. Refuses a
    /// relative path, a directory inside or containing `steve_home`, one owned by another user, or
    /// one that group or others can access.
    pub fn open(dir: &Path, steve_home: &Path) -> Result<Self, KeyStoreError> {
        if !dir.is_absolute() {
            return Err(refused(format!(
                "{} must be an absolute path",
                dir.display()
            )));
        }
        let resolved = resolve(dir)
            .ok_or_else(|| refused(format!("the parent of {} does not exist", dir.display())))?;
        let home = fs::canonicalize(steve_home).unwrap_or_else(|_| steve_home.to_path_buf());
        if same_path(&resolved, &home) || resolved.starts_with(&home) || home.starts_with(&resolved)
        {
            return Err(refused(format!(
                "{} must be outside STEVE_HOME ({}), on separate protected storage",
                dir.display(),
                steve_home.display()
            )));
        }
        match fs::symlink_metadata(&resolved) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&resolved)
                    .map_err(|e| refused(format!("cannot create {}: {e}", resolved.display())))?;
                // the umask may have narrowed it further; make it exactly 0700
                fs::set_permissions(&resolved, fs::Permissions::from_mode(0o700))
                    .map_err(|e| refused(format!("{}: {e}", resolved.display())))?;
            }
            Err(e) => return Err(refused(format!("{}: {e}", resolved.display()))),
            Ok(_) => {}
        }
        let meta =
            fs::metadata(&resolved).map_err(|e| refused(format!("{}: {e}", resolved.display())))?;
        if !meta.is_dir() {
            return Err(refused(format!(
                "{} is not a directory",
                resolved.display()
            )));
        }
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        if meta.uid() != me {
            return Err(refused(format!(
                "{} is owned by uid {}, not by the Hub's user (uid {me})",
                resolved.display(),
                meta.uid()
            )));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(refused(format!(
                "{} is accessible to group or others (mode {:o}); chmod 700 it",
                resolved.display(),
                meta.mode() & 0o777
            )));
        }
        Ok(Self(FileKeyStore::existing(resolved)))
    }

    pub fn dir(&self) -> &Path {
        self.0.dir()
    }
}

impl KeyStore for DirKeyStore {
    fn kind(&self) -> &'static str {
        Kind::Dir.as_str()
    }

    fn label(&self) -> String {
        format!("{} {}", Kind::Dir.label(), self.dir().display())
    }

    fn get(&self, account: &str) -> Result<Option<Kek>, KeyStoreError> {
        self.0.get(account)
    }

    fn set(&self, account: &str, key: &Kek) -> Result<(), KeyStoreError> {
        self.0.set(account, key)
    }

    fn delete(&self, account: &str) -> Result<(), KeyStoreError> {
        self.0.delete(account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::keystore::from_env;

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).expect("meta").permissions().mode() & 0o777
    }

    #[test]
    fn creates_a_private_dir_and_round_trips() {
        let t = tempfile::tempdir().expect("tmp");
        let home = t.path().join("steve-home");
        fs::create_dir(&home).expect("mkdir");
        let keys = t.path().join("keys");
        let ks = DirKeyStore::open(&keys, &home).expect("open");
        assert_eq!(ks.kind(), "dir");
        assert!(ks.label().starts_with("密钥目录 /"), "{}", ks.label());
        assert_eq!(mode(&keys), 0o700);
        let k = Kek::generate().expect("random");
        ks.set("ws/kek/v1", &k).expect("set");
        assert_eq!(mode(&keys.join("ws__kek__v1")), 0o600);
        assert_eq!(
            ks.get("ws/kek/v1").expect("get").expect("some").bytes(),
            k.bytes()
        );
        ks.delete("ws/kek/v1").expect("delete");
        assert!(ks.get("ws/kek/v1").expect("get").is_none());
        assert!(ks.get("../x").is_err());
        // reopening an existing private dir is fine
        DirKeyStore::open(&keys, &home).expect("reopen");
    }

    #[test]
    fn refuses_steve_home_and_loose_dirs() {
        let t = tempfile::tempdir().expect("tmp");
        let home = t.path().join("steve-home");
        fs::create_dir(&home).expect("mkdir");
        let refused = |dir: &Path| match DirKeyStore::open(dir, &home) {
            Err(KeyStoreError::Refused(m)) => m,
            other => panic!("{dir:?}: {other:?}"),
        };
        // inside, equal to, or containing STEVE_HOME
        assert!(refused(&home.join("vault-keys")).contains("outside STEVE_HOME"));
        assert!(!home.join("vault-keys").exists(), "nothing created");
        refused(&home);
        refused(t.path());
        // through a symlink into STEVE_HOME
        let link = t.path().join("link");
        std::os::unix::fs::symlink(&home, &link).expect("symlink");
        refused(&link.join("keys"));
        // relative, missing parent
        assert!(refused(Path::new("keys")).contains("absolute"));
        assert!(refused(&t.path().join("no/such/keys")).contains("parent"));
        // group/world readable
        let loose = t.path().join("loose");
        fs::create_dir(&loose).expect("mkdir");
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o750)).expect("chmod");
        assert!(refused(&loose).contains("chmod 700"));
        // a file, not a directory
        let f = t.path().join("plain");
        fs::write(&f, "x").expect("write");
        fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).expect("chmod");
        assert!(refused(&f).contains("not a directory"));
    }

    #[test]
    fn from_env_dir_needs_the_variable() {
        let t = tempfile::tempdir().expect("tmp");
        let home = t.path().join("steve-home");
        let keys = t.path().join("keys");
        let k = keys.to_string_lossy().to_string();
        let env = |with_dir: bool| {
            let k = k.clone();
            move |v: &str| match v {
                "STEVE_VAULT_KEYSTORE" => Some("dir".to_string()),
                "STEVE_VAULT_KEY_DIR" if with_dir => Some(k.clone()),
                _ => None,
            }
        };
        let e = from_env(&home, env(false)).err().expect("refused");
        assert!(e.to_string().contains("STEVE_VAULT_KEY_DIR"), "{e}");
        let ks = from_env(&home, env(true)).expect("dir store");
        assert_eq!(ks.kind(), "dir");
        assert!(keys.is_dir());
    }
}
