//! The local release runner reads its signing password from the installation's key store.
use std::io::IsTerminal as _;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use steve_modules::vault::keystore::{KeyStore, KeyStoreError, signing};
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

/// Resolve the signing keychain password for an explicit terminal maintenance command (such as
/// `steve-hub --local-release upgrade-macos`). Respects `STEVE_VAULT_KEYSTORE` / `STEVE_VAULT_KEY_DIR`
/// loaded from `<home>/steve.env`, tries `steve-keyhelper` first, and on macOS falls back to an
/// interactive login-keychain read when running from an unsigned release tarball binary.
pub(super) fn resolve_maintenance_password(
    home: &Path,
    vars: &impl Fn(&str) -> Option<String>,
    stdin_password: Option<Zeroizing<String>>,
) -> Result<Option<Zeroizing<String>>> {
    let vault_service = vault_service_for_home(home);
    resolve_maintenance_password_with(
        home,
        vars,
        stdin_password,
        std::io::stdin().is_terminal(),
        |notice| eprintln!("{notice}"),
        || {
            #[cfg(target_os = "macos")]
            {
                let configured = vars("STEVE_KEYHELPER");
                let Some(helper) =
                    steve_modules::vault::keystore::HelperStore::find(home, configured.as_deref())
                else {
                    return Ok(None);
                };
                signing::get(&helper)
            }
            #[cfg(not(target_os = "macos"))]
            {
                Ok(None)
            }
        },
        || {
            #[cfg(target_os = "macos")]
            {
                signing::get_interactive_keychain(&vault_service)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = &vault_service;
                Ok(None)
            }
        },
    )
}

fn vault_service_for_home(home: &Path) -> String {
    super::read::<serde_json::Value>(&home.join("signing/identity.json"))
        .ok()
        .and_then(|v| v["vault_service"].as_str().map(str::to_owned))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| steve_modules::vault::keystore::SERVICE.to_owned())
}

pub(super) fn resolve_maintenance_password_with(
    home: &Path,
    vars: &impl Fn(&str) -> Option<String>,
    stdin_password: Option<Zeroizing<String>>,
    is_tty: bool,
    on_interactive_prompt: impl FnOnce(&str),
    helper_lookup: impl FnOnce() -> Result<Option<Zeroizing<String>>, KeyStoreError>,
    interactive_lookup: impl FnOnce() -> Result<Option<Zeroizing<String>>, KeyStoreError>,
) -> Result<Option<Zeroizing<String>>> {
    if let Some(pass) = stdin_password {
        let normalized = signing::normalize_stdin_password(&pass);
        ensure!(
            !normalized.is_empty(),
            "--keychain-pass-stdin received an empty password"
        );
        return Ok(Some(normalized));
    }
    if !home.join("signing/identity.json").is_file() {
        return Ok(None);
    }
    if let Some(kind) = vars("STEVE_VAULT_KEYSTORE")
        && matches!(kind.trim(), "dir" | "file")
    {
        let store = steve_modules::vault::keystore::from_env(home, vars)
            .context("open installation key store configured in steve.env")?;
        return signing::get(store.as_ref()).map_err(Into::into);
    }
    if let Ok(Some(pass)) = helper_lookup() {
        return Ok(Some(pass));
    }
    ensure!(
        is_tty,
        "interactive macOS Keychain access requires a terminal (stdin is not a TTY); run in Terminal or pipe the signing password with --keychain-pass-stdin"
    );
    let notice = steve_i18n::t!("hub.upgrade.interactive_keychain_notice");
    on_interactive_prompt(&notice);
    match interactive_lookup() {
        Ok(Some(pass)) => Ok(Some(pass)),
        Ok(None) => Ok(None),
        Err(err) => bail!(
            "cannot read signing keychain password interactively ({err}); run in a macOS GUI session or pass --keychain-pass-stdin"
        ),
    }
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
