//! `SecretServiceStore` (`STEVE_VAULT_KEYSTORE=secret-service`): the freedesktop Secret Service on the
//! D-Bus session bus (GNOME Keyring, KWallet, KeePassXC), for Linux desktops. Items live in the default
//! collection with the attributes `service` = [`SERVICE`] and `account` = `<ws>/kek/v<n>`, the same
//! names as the macOS keychain entries; the value is the key as base64, sent over a DH-encrypted session.
//!
//! Like the keychain store it never waits on a person: a locked collection is `Locked` (it is not
//! unlocked, which would pop a dialog), a dismissed prompt is `Denied`, and every call gives up after
//! [`TIMEOUT`] (a prompt nobody answers) as `Denied`.
//!
//! zbus needs an async runtime and the trait is blocking (and may be called from inside the Hub's
//! runtime), so each call runs on its own short-lived thread with a current-thread runtime. The KEK is
//! cached by the vault, so this happens rarely.

use std::collections::HashMap;
use std::time::Duration;

use secret_service::{EncryptionType, Error as SsError, SecretService};

use super::{KeyStore, KeyStoreError, Kind, SERVICE, decode, encode};
use crate::vault::secret::Kek;

/// How long one call may take, prompts included.
const TIMEOUT: Duration = Duration::from_secs(10);
/// How long the startup probe of `auto` waits for an answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Default)]
pub struct SecretServiceStore;

fn map(e: SsError) -> KeyStoreError {
    match e {
        SsError::Locked => KeyStoreError::Locked,
        SsError::Prompt | SsError::PromptDisconnected => KeyStoreError::Denied,
        // org.freedesktop.DBus.Error.AccessDenied (a sandbox or a bus policy refusing us)
        SsError::ZbusFdo(zbus::fdo::Error::AccessDenied(_)) => KeyStoreError::Denied,
        SsError::Unavailable => KeyStoreError::Other(
            "no Secret Service on the D-Bus session bus (is a keyring daemon running?)".into(),
        ),
        SsError::NoResult => KeyStoreError::Other("the keyring has no default collection".into()),
        SsError::Crypto(m) => KeyStoreError::Other(format!("secret service session: {m}")),
        // zbus errors carry D-Bus names and paths, never the secret
        other => KeyStoreError::Other(format!("secret service: {other}")),
    }
}

/// Runs `f` on a fresh thread with its own runtime, giving up after `limit`.
fn run<T, F, Fut>(limit: Duration, f: F) -> Result<T, KeyStoreError>
where
    T: Send,
    F: FnOnce() -> Fut + Send,
    Fut: std::future::Future<Output = Result<T, KeyStoreError>>,
{
    std::thread::scope(|s| {
        s.spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| KeyStoreError::Other(format!("runtime: {}", e.kind())))?;
            rt.block_on(async move {
                tokio::time::timeout(limit, f())
                    .await
                    .unwrap_or(Err(KeyStoreError::Denied))
            })
        })
        .join()
        .unwrap_or_else(|_| Err(KeyStoreError::Other("secret service call panicked".into())))
    })
}

fn attrs(account: &str) -> HashMap<&str, &str> {
    HashMap::from([("service", SERVICE), ("account", account)])
}

impl SecretServiceStore {
    pub fn new() -> Self {
        Self
    }

    /// Whether a Secret Service answers on the session bus and has a default collection (locked or
    /// not). For `STEVE_VAULT_KEYSTORE=auto`.
    pub fn probe() -> bool {
        run(PROBE_TIMEOUT, || async {
            let ss = SecretService::connect(EncryptionType::Dh)
                .await
                .map_err(map)?;
            ss.get_default_collection().await.map_err(map)?;
            Ok(())
        })
        .is_ok()
    }
}

impl KeyStore for SecretServiceStore {
    fn kind(&self) -> &'static str {
        Kind::SecretService.as_str()
    }

    fn get(&self, account: &str) -> Result<Option<Kek>, KeyStoreError> {
        run(TIMEOUT, || async {
            let ss = SecretService::connect(EncryptionType::Dh)
                .await
                .map_err(map)?;
            let col = ss.get_default_collection().await.map_err(map)?;
            if col.is_locked().await.map_err(map)? {
                return Err(KeyStoreError::Locked);
            }
            let items = col.search_items(attrs(account)).await.map_err(map)?;
            let Some(item) = items.first() else {
                return Ok(None);
            };
            let raw = zeroize::Zeroizing::new(item.get_secret().await.map_err(map)?);
            decode(&raw).map(Some)
        })
    }

    fn set(&self, account: &str, key: &Kek) -> Result<(), KeyStoreError> {
        let value = encode(key);
        run(TIMEOUT, || async {
            let ss = SecretService::connect(EncryptionType::Dh)
                .await
                .map_err(map)?;
            let col = ss.get_default_collection().await.map_err(map)?;
            if col.is_locked().await.map_err(map)? {
                return Err(KeyStoreError::Locked);
            }
            col.create_item(
                &format!("Steve vault {account}"),
                attrs(account),
                value.as_bytes(),
                true,
                "text/plain",
            )
            .await
            .map_err(map)?;
            Ok(())
        })
    }

    fn delete(&self, account: &str) -> Result<(), KeyStoreError> {
        run(TIMEOUT, || async {
            let ss = SecretService::connect(EncryptionType::Dh)
                .await
                .map_err(map)?;
            let col = ss.get_default_collection().await.map_err(map)?;
            if col.is_locked().await.map_err(map)? {
                return Err(KeyStoreError::Locked);
            }
            for item in col.search_items(attrs(account)).await.map_err(map)? {
                item.delete().await.map_err(map)?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_the_vault_states() {
        assert_eq!(map(SsError::Locked), KeyStoreError::Locked);
        assert_eq!(map(SsError::Prompt), KeyStoreError::Denied);
        assert_eq!(map(SsError::PromptDisconnected), KeyStoreError::Denied);
        assert!(matches!(map(SsError::Unavailable), KeyStoreError::Other(_)));
    }

    #[test]
    fn a_hung_call_times_out_as_denied() {
        let r: Result<(), _> = run(Duration::from_millis(50), || async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(())
        });
        assert_eq!(r, Err(KeyStoreError::Denied));
    }

    /// Writes, reads, replaces and deletes a KEK under a throwaway account in the session keyring.
    /// Needs a running, unlocked Secret Service (a desktop session, or `gnome-keyring-daemon` under
    /// `dbus-run-session`). Run with `cargo test -p steve-modules -- --ignored secret_service`.
    #[test]
    #[ignore = "needs a Linux session keyring; run once by hand"]
    fn secret_service_round_trip() {
        let ks = SecretServiceStore::new();
        assert!(SecretServiceStore::probe(), "no Secret Service");
        let acct = format!("ws_test_{}/kek/v1", std::process::id());
        assert!(ks.get(&acct).expect("get").is_none());
        let k = Kek::generate().expect("random");
        ks.set(&acct, &k).expect("set");
        assert_eq!(
            ks.get(&acct).expect("get").expect("some").bytes(),
            k.bytes()
        );
        let k2 = Kek::generate().expect("random");
        ks.set(&acct, &k2).expect("replace");
        assert_eq!(
            ks.get(&acct).expect("get").expect("some").bytes(),
            k2.bytes()
        );
        ks.delete(&acct).expect("delete");
        ks.delete(&acct).expect("delete twice");
        assert!(ks.get(&acct).expect("get").is_none());
    }
}
