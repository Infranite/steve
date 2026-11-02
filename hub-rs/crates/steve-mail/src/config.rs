//! Where the mail module keeps its files and which certificate roots it trusts (email.md §1, §13).
//!
//! Only encrypted connections are made, verified against the system roots (`rustls-native-certs`).
//! A self-signed test root can be added through `STEVE_MAIL_TEST_CA` (a PEM file), with the same
//! guard as `FileKeyStore`: refused when `STEVE_HOME` is the real `~/.steve` (or unknown).
//!
//! The conversational configuration of mailboxes (email.md §10, slice E6) is [`targets`].

pub mod targets;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

/// The mail module's environment. Cheap to clone.
#[derive(Clone)]
pub struct MailConfig {
    home: PathBuf,
    test_roots: Arc<Vec<CertificateDer<'static>>>,
}

impl std::fmt::Debug for MailConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailConfig")
            .field("home", &self.home)
            .field("test_roots", &self.test_roots.len())
            .finish()
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
        || a.to_string_lossy().trim_end_matches('/') == b.to_string_lossy().trim_end_matches('/')
}

impl MailConfig {
    /// Files under `steve_home` (`data/mail/…`), system roots only.
    pub fn new(steve_home: impl Into<PathBuf>) -> Self {
        Self {
            home: steve_home.into(),
            test_roots: Arc::new(Vec::new()),
        }
    }

    /// From the environment: `STEVE_HOME` (default `~/.steve`) and the optional `STEVE_MAIL_TEST_CA`.
    /// The test root is refused for the real `~/.steve` and when `HOME` is unknown.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let user_home = var("HOME").map(PathBuf::from);
        let home = var("STEVE_HOME")
            .filter(|h| !h.trim().is_empty())
            .map(PathBuf::from)
            .or_else(|| user_home.as_ref().map(|h| h.join(".steve")))
            .ok_or("STEVE_HOME or HOME must be set")?;
        let mut cfg = Self::new(&home);
        if let Some(ca) = var("STEVE_MAIL_TEST_CA").filter(|v| !v.trim().is_empty()) {
            let user_home = user_home.ok_or("STEVE_MAIL_TEST_CA needs HOME to be set")?;
            if same_path(&home, &user_home.join(".steve")) {
                return Err("STEVE_MAIL_TEST_CA is not allowed for ~/.steve".into());
            }
            let pem = std::fs::read(&ca).map_err(|e| format!("STEVE_MAIL_TEST_CA: {e}"))?;
            let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&pem)
                .collect::<Result<_, _>>()
                .map_err(|e| format!("STEVE_MAIL_TEST_CA: {e}"))?;
            if certs.is_empty() {
                return Err("STEVE_MAIL_TEST_CA: no certificate in the file".into());
            }
            cfg.test_roots = Arc::new(certs);
        }
        Ok(cfg)
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The test roots as PEM-decoded DER (for the SMTP client).
    pub(crate) fn test_roots(&self) -> &[CertificateDer<'static>] {
        &self.test_roots
    }

    /// The rustls client configuration for IMAP: system roots plus the test roots.
    pub(crate) fn client_config(&self) -> Result<Arc<rustls::ClientConfig>, String> {
        let mut roots = native_roots().clone();
        for c in self.test_roots.iter() {
            roots.add(c.clone()).map_err(|e| e.to_string())?;
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Arc::new(cfg))
    }
}

/// The system roots, loaded once (loading reads the keychain / cert files).
fn native_roots() -> &'static RootCertStore {
    static ROOTS: OnceLock<RootCertStore> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut store = RootCertStore::empty();
        let res = rustls_native_certs::load_native_certs();
        let (added, _) = store.add_parsable_certificates(res.certs);
        if added == 0 {
            tracing::warn!("no system root certificates found; mail TLS will fail");
        }
        store
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ca_is_refused_for_the_real_home() {
        let dir = tempfile::tempdir().unwrap();
        let pem = dir.path().join("ca.pem");
        std::fs::write(&pem, "not used").unwrap();
        let user = dir.path().to_string_lossy().to_string();
        let real = format!("{user}/.steve");
        let p = pem.to_string_lossy().to_string();
        let env = |home: Option<String>| {
            let (u, p) = (user.clone(), p.clone());
            move |k: &str| match k {
                "HOME" => Some(u.clone()),
                "STEVE_HOME" => home.clone(),
                "STEVE_MAIL_TEST_CA" => Some(p.clone()),
                _ => None,
            }
        };
        assert!(MailConfig::from_env(env(Some(real))).is_err());
        assert!(
            MailConfig::from_env(env(None)).is_err(),
            "default home is ~/.steve"
        );
        // a temp home is fine, but the file must hold a certificate
        let e = MailConfig::from_env(env(Some(format!("{user}/tmp-home")))).unwrap_err();
        assert!(e.contains("no certificate"), "{e}");
    }
}
