use super::*;

pub(super) fn protected_files(
    home: &Path,
    record: &Value,
    guards: &mut BTreeMap<PathBuf, Guard>,
) -> Result<()> {
    for (name, expected) in record["files"].as_object().unwrap() {
        ensure!(
            !Path::new(name).is_absolute()
                && Path::new(name)
                    .components()
                    .all(|p| matches!(p, Component::Normal(_))),
            "{}",
            t!("install.reconfigure.receipt.path_invalid")
        );
        if name == "steve.env" {
            continue;
        }
        let path = home.join(name);
        owned(&path, false)?;
        ensure!(
            digest(&path)?
                == expected
                    .as_str()
                    .context(t!("install.reconfigure.receipt.file_digest_invalid"))?,
            "{}",
            t!("install.reconfigure.protected_file_modified", name = name)
        );
        guard(guards, &path)?;
    }
    Ok(())
}

pub(super) fn oidc(
    values: &BTreeMap<String, String>,
    guards: &mut BTreeMap<PathBuf, Guard>,
) -> Result<()> {
    /* OIDC is optional since 2026-11-09 (a Hub set up without it signs in with access tokens):
     * when it is configured, it is kept whole and maintained as before */
    let oidc = [
        "STEVE_OIDC_ISSUER",
        "STEVE_OIDC_CLIENT_ID",
        "STEVE_OIDC_CLIENT_SECRET_FILE",
    ]
    .iter()
    .any(|k| values.get(*k).is_some_and(|v| !v.is_empty()));
    if oidc {
        for name in ["STEVE_OIDC_ISSUER", "STEVE_OIDC_CLIENT_ID"] {
            ensure!(
                values.get(name).is_some_and(|v| !v.is_empty()),
                "{}",
                t!("install.reconfigure.oidc.field_missing", name = name)
            );
        }
        let secret = Path::new(
            values
                .get("STEVE_OIDC_CLIENT_SECRET_FILE")
                .context(t!("install.reconfigure.oidc.secret_by_file"))?,
        );
        ensure!(
            secret.is_absolute(),
            "{}",
            t!("install.reconfigure.oidc.secret_path_relative")
        );
        owned(secret, false)?;
        guard(guards, secret)?;
    }
    Ok(())
}

pub(super) fn vault(
    home: &Path,
    values: &BTreeMap<String, String>,
    guards: &mut BTreeMap<PathBuf, Guard>,
) -> Result<()> {
    match values.get("STEVE_VAULT_KEYSTORE").map(String::as_str) {
        Some("dir") => {
            let keys = Path::new(
                values
                    .get("STEVE_VAULT_KEY_DIR")
                    .context(t!("install.reconfigure.vault.dir_unset"))?,
            );
            ensure!(
                keys.is_absolute(),
                "{}",
                t!("install.reconfigure.vault.dir_relative")
            );
            owned(keys, true).context(t!("install.reconfigure.vault.dir_missing"))?;
            let resolved = fs::canonicalize(keys)?;
            ensure!(
                !resolved.starts_with(home) && !home.starts_with(&resolved),
                "{}",
                t!("install.reconfigure.vault.dir_inside_home")
            );
            guard(guards, keys)?;
        }
        Some("file") => {
            let keys = home.join("vault-keys");
            owned(&keys, true).context(t!("install.reconfigure.vault.keys_missing"))?;
            guard(guards, &keys)?;
        }
        Some("keychain") if cfg!(target_os = "macos") => {}
        _ => bail!("{}", t!("install.reconfigure.vault.backend_unsupported")),
    }
    Ok(())
}
