//! The local release runner reads its signing password from the installation's key store.
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use steve_modules::vault::keystore::{KeyStore, signing};
use zeroize::Zeroizing;

pub(super) fn provision(home: &Path, password: &str) -> Result<()> {
    let store = installed_helper(home).context("signing key store unavailable")?;
    if let Some(existing) = signing::get(store.as_ref())? {
        ensure!(
            existing.as_str() == password,
            "the signing key store already belongs to another installation"
        );
        Ok(())
    } else {
        Ok(signing::set(store.as_ref(), password)?)
    }
}

pub(super) fn password(store: Option<&dyn KeyStore>) -> Result<Option<Zeroizing<String>>> {
    store
        .map(signing::get)
        .transpose()
        .map(Option::flatten)
        .map_err(Into::into)
}

/// The vault's own key directory, when the vault keeps its keys in one (`STEVE_VAULT_KEYSTORE=dir`
/// or `file`): the password goes there too, never into the login keychain, where another
/// installation under this user keeps its own (`steve setup --vault-dir`).
#[cfg(target_os = "macos")]
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

/// steve-keyhelper, when it is installed: its items stay
/// readable across Hub releases, and it answers only a parent signed as the Hub. With the vault's
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
