//! Where the workspace master keys live. One OS-native store per
//! platform, picked by [`from_env`]:
//!
//! | `STEVE_VAULT_KEYSTORE` | store | where |
//! | --- | --- | --- |
//! | `keychain` | [`KeychainStore`] | macOS login keychain (production) |
//! | `secret-service` | `SecretServiceStore` | Linux desktop keyring (GNOME Keyring, KWallet) over D-Bus |
//! | `dir` | [`DirKeyStore`] | one 0600 file per key in `STEVE_VAULT_KEY_DIR`, outside `STEVE_HOME` (headless Linux) |
//! | `file` | [`FileKeyStore`] | `<STEVE_HOME>/vault-keys`, tests and development only, never `~/.steve` |
//! | `auto` (default) | keychain on macOS, secret-service on Linux when one answers | otherwise a startup error |
//!
//! Entries are named `<workspace_id>/kek/v<n>` (the `vault_keys.keychain_account` column) and hold the
//! key as base64. The calls are blocking; callers run them off the async runtime.

#[cfg(unix)]
mod dir;
#[cfg(unix)]
mod file;
#[cfg(target_os = "macos")]
mod helper;
#[cfg(target_os = "macos")]
mod keychain;
#[cfg(target_os = "linux")]
mod secret_service;
pub mod signing;

use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(unix)]
pub use dir::DirKeyStore;
#[cfg(unix)]
pub use file::FileKeyStore;
#[cfg(target_os = "macos")]
pub use helper::HelperStore;
#[cfg(target_os = "macos")]
pub use keychain::KeychainStore;
#[cfg(target_os = "linux")]
pub use secret_service::SecretServiceStore;

use super::secret::Kek;

/// The keychain service of every vault entry (also the Secret Service `service` attribute).
pub const SERVICE: &str = match option_env!("STEVE_KEYHELPER_SERVICE") {
    Some(service) => service,
    None => "io.github.infranite.steve.vault",
};

/// Why a key store call failed. No variant carries key material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyStoreError {
    /// The store is locked (Mac mini rebooted and nobody logged in; the Linux keyring collection is
    /// locked) and user interaction is off.
    #[error("keychain locked")]
    Locked,
    /// The user or the access control list refused this binary (or dismissed / never answered a
    /// keyring prompt).
    #[error("keychain access denied")]
    Denied,
    /// An entry exists but does not hold a 32-byte base64 key.
    #[error("key store entry is malformed")]
    Malformed,
    /// Refused to start (`FileKeyStore` for `~/.steve`, unknown backend, no store on this system).
    #[error("{0}")]
    Refused(String),
    /// Anything else: an OS status code or an I/O error, never the value.
    #[error("key store: {0}")]
    Other(String),
}

/// Stores the KEKs. Implementations must never log or return the key bytes in an error.
pub trait KeyStore: Send + Sync {
    /// `"keychain"`, `"secret-service"`, `"dir"` or `"file"`, for logs and `GET /vault/status`.
    fn kind(&self) -> &'static str;
    /// Where the keys are, for people: `macOS 钥匙串`, `目录 /srv/steve-keys`, …
    fn label(&self) -> String {
        Kind::parse(self.kind())
            .map(|k| k.label())
            .unwrap_or_else(|| self.kind().to_string())
    }
    /// `Ok(None)` when there is no entry for `account`.
    fn get(&self, account: &str) -> Result<Option<Kek>, KeyStoreError>;
    /// Creates or replaces the entry.
    fn set(&self, account: &str, key: &Kek) -> Result<(), KeyStoreError>;
    /// Removes the entry; a missing entry is not an error.
    fn delete(&self, account: &str) -> Result<(), KeyStoreError>;
}

/// `vault_keys.keychain_account` for a version.
pub fn account_name(ws: &str, version: i64) -> String {
    format!("{ws}/kek/v{version}")
}

/// The concrete stores [`from_env`] can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Keychain,
    SecretService,
    Dir,
    File,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "keychain" => Self::Keychain,
            "secret-service" => Self::SecretService,
            "dir" => Self::Dir,
            "file" => Self::File,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keychain => "keychain",
            Self::SecretService => "secret-service",
            Self::Dir => "dir",
            Self::File => "file",
        }
    }

    /// For people, in the request's language (“macOS 钥匙串”).
    pub fn label(self) -> String {
        match self {
            Self::Keychain => steve_i18n::t!("modules.keystore.keychain"),
            Self::SecretService => steve_i18n::t!("modules.keystore.secret_service"),
            Self::Dir => steve_i18n::t!("modules.keystore.dir"),
            Self::File => steve_i18n::t!("modules.keystore.file"),
        }
    }
}

/// The operating system, for [`choose`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
    Windows,
    Other,
}

impl Os {
    pub const CURRENT: Os = if cfg!(target_os = "macos") {
        Os::MacOs
    } else if cfg!(target_os = "linux") {
        Os::Linux
    } else if cfg!(windows) {
        Os::Windows
    } else {
        Os::Other
    };
}

const DIR_HINT: &str = "set STEVE_VAULT_KEYSTORE=dir and STEVE_VAULT_KEY_DIR=<a 0700 directory outside STEVE_HOME, on separate protected storage>";

/// Which store `STEVE_VAULT_KEYSTORE` (`requested`) asks for on `os`. `secret_service_up` is only
/// called for `auto` on Linux. Pure, so the matrix is unit-tested on any host.
pub fn choose(
    requested: Option<&str>,
    os: Os,
    secret_service_up: impl FnOnce() -> bool,
) -> Result<Kind, KeyStoreError> {
    let requested = requested.map(str::trim).unwrap_or("");
    let kind = match requested {
        "" | "auto" => match os {
            Os::MacOs => Kind::Keychain,
            Os::Linux if secret_service_up() => Kind::SecretService,
            Os::Linux => {
                return Err(KeyStoreError::Refused(format!(
                    "the vault found no Secret Service (desktop keyring) on the D-Bus session bus; on a \
                     headless server {DIR_HINT}"
                )));
            }
            Os::Windows => return Err(windows_unsupported()),
            Os::Other => {
                return Err(KeyStoreError::Refused(format!(
                    "the vault has no native key store on this system; {DIR_HINT}"
                )));
            }
        },
        other => Kind::parse(other).ok_or_else(|| {
            KeyStoreError::Refused(format!(
                "STEVE_VAULT_KEYSTORE={other} is not supported (auto, keychain, secret-service, dir or file)"
            ))
        })?,
    };
    let ok = match kind {
        Kind::Keychain => os == Os::MacOs,
        Kind::SecretService => os == Os::Linux,
        Kind::Dir | Kind::File => matches!(os, Os::MacOs | Os::Linux | Os::Other),
    };
    if ok {
        Ok(kind)
    } else if os == Os::Windows {
        Err(windows_unsupported())
    } else {
        Err(KeyStoreError::Refused(format!(
            "STEVE_VAULT_KEYSTORE={} is not available on this system ({})",
            kind.as_str(),
            match kind {
                Kind::Keychain => "macOS only".to_string(),
                Kind::SecretService => format!("Linux only; elsewhere {DIR_HINT}"),
                Kind::Dir | Kind::File => "Unix only".to_string(),
            }
        )))
    }
}

fn windows_unsupported() -> KeyStoreError {
    KeyStoreError::Refused(
        "the vault does not support Windows yet (no Credential Manager store); run the Hub on macOS or Linux".into(),
    )
}

/// The store the Hub uses, from `STEVE_VAULT_KEYSTORE` (see the module table). `file` keeps
/// `<steve_home>/vault-keys` and is refused for the real `~/.steve`; `dir` needs
/// `STEVE_VAULT_KEY_DIR` outside `steve_home`. `env` is `std::env::var(..).ok()` in production.
pub fn from_env(
    steve_home: &Path,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Arc<dyn KeyStore>, KeyStoreError> {
    let kind = choose(
        env("STEVE_VAULT_KEYSTORE").as_deref(),
        Os::CURRENT,
        secret_service_up,
    )?;
    open(kind, steve_home, &env)
}

#[cfg(target_os = "linux")]
fn secret_service_up() -> bool {
    SecretServiceStore::probe()
}

#[cfg(not(target_os = "linux"))]
fn secret_service_up() -> bool {
    false
}

#[cfg(unix)]
fn open(
    kind: Kind,
    steve_home: &Path,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Arc<dyn KeyStore>, KeyStoreError> {
    match kind {
        Kind::File => {
            let home = env("HOME").map(PathBuf::from);
            Ok(Arc::new(FileKeyStore::for_steve_home(
                steve_home,
                home.as_deref(),
            )?))
        }
        Kind::Dir => {
            let dir = env("STEVE_VAULT_KEY_DIR")
                .filter(|d| !d.trim().is_empty())
                .ok_or_else(|| {
                    KeyStoreError::Refused(format!(
                        "STEVE_VAULT_KEYSTORE=dir needs STEVE_VAULT_KEY_DIR; {DIR_HINT}"
                    ))
                })?;
            Ok(Arc::new(DirKeyStore::open(Path::new(&dir), steve_home)?))
        }
        Kind::Keychain => keychain(steve_home, env("STEVE_KEYHELPER").as_deref()),
        Kind::SecretService => secret_service(),
    }
}

#[cfg(not(unix))]
fn open(
    _kind: Kind,
    _steve_home: &Path,
    _env: &impl Fn(&str) -> Option<String>,
) -> Result<Arc<dyn KeyStore>, KeyStoreError> {
    Err(windows_unsupported())
}

/// Through `steve-keyhelper` when it is installed (production: its fixed code signature keeps the items
/// readable across Hub releases); otherwise in-process, which only suits a Hub that is never rebuilt.
#[cfg(target_os = "macos")]
fn keychain(steve_home: &Path, helper: Option<&str>) -> Result<Arc<dyn KeyStore>, KeyStoreError> {
    match HelperStore::find(steve_home, helper) {
        Some(h) => {
            tracing::info!(helper = ?h, "vault: macOS keychain through steve-keyhelper");
            Ok(Arc::new(h))
        }
        None => {
            tracing::warn!(
                "vault: steve-keyhelper is not installed in {}/bin; using the keychain in-process, so a rebuilt Hub \
                 will be refused the keys; install steve-keyhelper with ops/release.sh",
                steve_home.display()
            );
            Ok(Arc::new(KeychainStore::login()))
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn keychain(_steve_home: &Path, _helper: Option<&str>) -> Result<Arc<dyn KeyStore>, KeyStoreError> {
    Err(KeyStoreError::Refused(
        "STEVE_VAULT_KEYSTORE=keychain is macOS only".into(),
    ))
}

#[cfg(target_os = "linux")]
fn secret_service() -> Result<Arc<dyn KeyStore>, KeyStoreError> {
    if !SecretServiceStore::probe() {
        return Err(KeyStoreError::Refused(format!(
            "STEVE_VAULT_KEYSTORE=secret-service: no Secret Service with a default collection on the \
             D-Bus session bus; on a headless server {DIR_HINT}"
        )));
    }
    Ok(Arc::new(SecretServiceStore::new()))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn secret_service() -> Result<Arc<dyn KeyStore>, KeyStoreError> {
    Err(KeyStoreError::Refused(
        "STEVE_VAULT_KEYSTORE=secret-service is Linux only".into(),
    ))
}

pub(crate) fn encode(key: &Kek) -> zeroize::Zeroizing<String> {
    zeroize::Zeroizing::new(data_encoding::BASE64.encode(key.bytes()))
}

pub(crate) fn decode(s: &[u8]) -> Result<Kek, KeyStoreError> {
    let raw = zeroize::Zeroizing::new(
        data_encoding::BASE64
            .decode(s.trim_ascii())
            .map_err(|_| KeyStoreError::Malformed)?,
    );
    Kek::from_slice(&raw).ok_or(KeyStoreError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(req: Option<&str>, os: Os, up: bool) -> Result<Kind, KeyStoreError> {
        choose(req, os, || up)
    }

    #[test]
    fn auto_picks_the_native_store() {
        for req in [None, Some(""), Some("auto"), Some(" auto ")] {
            assert_eq!(pick(req, Os::MacOs, false), Ok(Kind::Keychain));
            assert_eq!(pick(req, Os::Linux, true), Ok(Kind::SecretService));
            let e = pick(req, Os::Linux, false).expect_err("no keyring");
            let KeyStoreError::Refused(m) = e else {
                panic!("{e:?}")
            };
            assert!(m.contains("STEVE_VAULT_KEYSTORE=dir"), "{m}");
            assert!(m.contains("STEVE_VAULT_KEY_DIR"), "{m}");
            assert!(pick(req, Os::Windows, true).is_err());
            assert!(pick(req, Os::Other, true).is_err());
        }
    }

    #[test]
    fn auto_on_macos_does_not_probe_dbus() {
        let kind = choose(None, Os::MacOs, || panic!("probed")).expect("keychain");
        assert_eq!(kind, Kind::Keychain);
        assert_eq!(
            choose(Some("dir"), Os::Linux, || panic!("probed")),
            Ok(Kind::Dir)
        );
    }

    #[test]
    fn explicit_choices_per_platform() {
        let ok = |req, os| pick(Some(req), os, true);
        assert_eq!(ok("keychain", Os::MacOs), Ok(Kind::Keychain));
        assert!(ok("keychain", Os::Linux).is_err());
        assert_eq!(ok("secret-service", Os::Linux), Ok(Kind::SecretService));
        assert!(ok("secret-service", Os::MacOs).is_err());
        for os in [Os::MacOs, Os::Linux, Os::Other] {
            assert_eq!(ok("dir", os), Ok(Kind::Dir));
            assert_eq!(ok("file", os), Ok(Kind::File));
        }
        for k in ["keychain", "secret-service", "dir", "file"] {
            let e = ok(k, Os::Windows).expect_err("windows");
            assert!(e.to_string().contains("Windows"), "{e}");
        }
        let e = ok("s3", Os::Linux).expect_err("unknown");
        assert!(e.to_string().contains("STEVE_VAULT_KEYSTORE=s3"), "{e}");
    }

    #[test]
    fn kinds_round_trip_and_have_labels() {
        for k in [Kind::Keychain, Kind::SecretService, Kind::Dir, Kind::File] {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
            assert!(!k.label().is_empty());
        }
        assert_eq!(Kind::parse("auto"), None);
    }
}
