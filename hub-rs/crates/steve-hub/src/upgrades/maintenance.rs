//! Explicit terminal maintenance. No password is fetched on behalf of a caller.
use super::*;

pub fn main(args: &[String]) -> i32 {
    match dispatch(args) {
        Ok((code, value)) => {
            println!("{value}");
            code
        }
        Err(error) => {
            eprintln!("{}", json!({"error":format!("{error:#}")}));
            1
        }
    }
}

fn dispatch(args: &[String]) -> Result<(i32, Value)> {
    let command = args.first().map(String::as_str).unwrap_or("--help");
    if command == "--help" {
        return Ok((
            0,
            json!({"usage":"--local-release setup-keychain|prepare-bundle|deploy <bundle> --home PATH; upgrade-macos [<bundle>] [--home PATH] [--label LABEL] [--timeout SECS] [--keychain-pass-stdin]; rollback-macos [--home PATH] [--label LABEL] [--timeout SECS] [--confirm-data-loss]; confirm-macos [--home PATH]; prune --home PATH [--keep PATH] [--dry-run]"}),
        ));
    }
    if command == "schema-state" && args.len() == 2 {
        return Ok((
            0,
            json!(steve_store::db::inspect_schema(Path::new(&args[1]))?),
        ));
    }
    if command == "schema-check" && args.len() == 3 {
        steve_install::release::check_schema(Path::new(&args[1]), Path::new(&args[2]))?;
        return Ok((0, json!({"compatible":true})));
    }
    if command == "provision-signing" && args.len() == 2 {
        let mut password = zeroize::Zeroizing::new(String::new());
        std::io::stdin().read_line(&mut password)?;
        super::signing::provision(Path::new(&args[1]), password.trim())?;
        return Ok((0, json!({"stored":true})));
    }
    if ![
        "setup-keychain",
        "prepare-bundle",
        "deploy",
        "upgrade-macos",
        "rollback-macos",
        "confirm-macos",
        "prune",
    ]
    .contains(&command)
    {
        bail!("unsupported native maintenance command")
    }
    let mut index = 1;
    let candidate = if matches!(command, "setup-keychain" | "prepare-bundle" | "deploy") {
        let path = args
            .get(index)
            .filter(|s| !s.starts_with('-'))
            .context("bundle path required")?;
        index += 1;
        Some(PathBuf::from(path))
    } else if command == "upgrade-macos" {
        if let Some(path) = args.get(index).filter(|s| !s.starts_with('-')) {
            index += 1;
            Some(PathBuf::from(path))
        } else {
            None
        }
    } else {
        None
    };
    let mut home = None;
    let mut label: Option<String> = None;
    let (mut version, mut commit, mut operation) = (None, None, None);
    let mut timeout = 600_u64;
    let (mut password_stdin, mut dry, mut confirm_data_loss) = (false, false, false);
    let mut keep = vec![];
    while index < args.len() {
        let name = args[index].as_str();
        index += 1;
        if name == "--keychain-pass-stdin" && matches!(command, "prepare-bundle" | "upgrade-macos")
        {
            password_stdin = true;
            continue;
        }
        if name == "--confirm-data-loss" && command == "rollback-macos" {
            confirm_data_loss = true;
            continue;
        }
        if name == "--dry-run" && command == "prune" {
            dry = true;
            continue;
        }
        let value = args.get(index).context("option requires a value")?.clone();
        index += 1;
        match name {
            "--home" => home = Some(PathBuf::from(value)),
            "--label"
                if matches!(
                    command,
                    "setup-keychain" | "upgrade-macos" | "rollback-macos"
                ) =>
            {
                label = Some(value)
            }
            "--version" if command == "prepare-bundle" => version = Some(value),
            "--commit" if command == "prepare-bundle" => commit = Some(value),
            "--operation" if command == "deploy" => operation = Some(value),
            "--timeout" | "--drain-s"
                if matches!(command, "deploy" | "upgrade-macos" | "rollback-macos") =>
            {
                timeout = value.parse()?
            }
            "--keep" if command == "prune" => keep.push(PathBuf::from(value)),
            _ => bail!("unsupported option {name}"),
        }
    }
    let home = home
        .or_else(|| std::env::var_os("STEVE_HOME").map(PathBuf::from))
        .or_else(|| runner::account_home().map(|p| p.join(".steve")))
        .context("home unavailable")?;
    match command {
        "setup-keychain" => Ok((
            0,
            steve_install::signing::setup_keychain(
                &home,
                &candidate.unwrap(),
                label.as_deref().unwrap_or("io.github.infranite.steve.hub"),
            )?,
        )),
        "prepare-bundle" => {
            if !password_stdin {
                bail!("prepare-bundle requires an explicit password on stdin")
            }
            let mut password = zeroize::Zeroizing::new(String::new());
            std::io::stdin().read_line(&mut password)?;
            Ok((
                0,
                steve_install::signing::prepare_bundle(
                    &home,
                    &candidate.unwrap(),
                    &version.context("--version required")?,
                    &commit.context("--commit required")?,
                    password.trim(),
                )?,
            ))
        }
        "deploy" => {
            if timeout == 0 || timeout > 86400 {
                bail!("timeout must be within 1..86400 seconds")
            }
            let result = steve_install::release::deploy(
                &home,
                &candidate.unwrap(),
                operation.as_deref(),
                Duration::from_secs(timeout),
            )?;
            Ok((result.exit_code, result.receipt))
        }
        "upgrade-macos" => {
            if timeout == 0 || timeout > 86400 {
                bail!("timeout must be within 1..86400 seconds")
            }
            let bundle = match candidate {
                Some(path) => path,
                None => {
                    let exe = std::env::current_exe()?.canonicalize()?;
                    exe.parent()
                        .and_then(Path::parent)
                        .context("steve-hub executable is not inside a release bundle")?
                        .to_path_buf()
                }
            };
            let stdin_password = if password_stdin {
                let mut raw = zeroize::Zeroizing::new(String::new());
                std::io::stdin().read_line(&mut raw)?;
                let normalized =
                    steve_modules::vault::keystore::signing::normalize_stdin_password(&raw);
                anyhow::ensure!(
                    !normalized.is_empty(),
                    "--keychain-pass-stdin received an empty password"
                );
                Some(normalized)
            } else {
                None
            };
            let caller_env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
            let merged_env = steve_install::release::read_home_env(&home, &caller_env)?;
            let home_for_pw = home.clone();
            // Resolve lazily inside upgrade_macos so resuming an already-signed staged bundle
            // (or refusing a duplicate upgrade) never triggers an unnecessary Keychain prompt.
            let result = steve_install::release::upgrade_macos(
                &home,
                &bundle,
                label.as_deref(),
                Duration::from_secs(timeout),
                move || {
                    super::signing::resolve_maintenance_password(
                        &home_for_pw,
                        &|k| merged_env.get(k).cloned(),
                        stdin_password,
                    )
                },
            )?;
            Ok((result.exit_code, result.receipt))
        }
        "rollback-macos" => {
            if timeout == 0 || timeout > 86400 {
                bail!("timeout must be within 1..86400 seconds")
            }
            let result = steve_install::release::rollback_macos(
                &home,
                label.as_deref(),
                Duration::from_secs(timeout),
                confirm_data_loss,
            )?;
            Ok((result.exit_code, result.receipt))
        }
        "confirm-macos" => Ok((0, steve_install::release::confirm_macos(&home)?)),
        "prune" => Ok((
            0,
            json!({"removed":steve_install::release::prune(&home, &keep, dry)?, "dry_run":dry}),
        )),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use steve_modules::vault::keystore::{DirKeyStore, KeyStoreError, signing as ks_signing};

    #[test]
    fn invalid_maintenance_never_reaches_an_installation() {
        for input in [
            vec!["build"],
            vec!["prepare-bundle", "/unused"],
            vec!["deploy", "/unused", "--timeout", "0"],
        ] {
            assert!(dispatch(&input.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err());
        }
        assert_eq!(dispatch(&[]).unwrap().0, 0);
    }

    #[test]
    fn review593_3_unsigned_binary_falls_back_to_interactive_keychain_and_reads_steve_env() {
        // Case A: STEVE_VAULT_KEYSTORE=dir + STEVE_VAULT_KEY_DIR defined ONLY in steve.env
        // (absent from process env) must be loaded and used to retrieve the signing password.
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("hub-home");
        let keys_dir = root.path().join("vault-keys");
        fs::create_dir_all(home.join("signing")).unwrap();
        fs::write(
            home.join("signing/identity.json"),
            r#"{"label":"test.hub","hub_identifier":"h","keyhelper_identifier":"k","vault_service":"test.vault"}"#,
        )
        .unwrap();
        let store = DirKeyStore::open(&keys_dir, &home).unwrap();
        let expected_pass = "0123456789abcdef".repeat(4);
        ks_signing::set(&store, &expected_pass).unwrap();
        fs::write(
            home.join("steve.env"),
            format!(
                "STEVE_VAULT_KEYSTORE=dir\nSTEVE_VAULT_KEY_DIR={}\n",
                keys_dir.display()
            ),
        )
        .unwrap();

        let caller_env = BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]);
        let merged = steve_install::release::read_home_env(&home, &caller_env).unwrap();
        let got_dir = super::signing::resolve_maintenance_password_with(
            &home,
            &|k| merged.get(k).cloned(),
            None,
            true,
            |_| panic!("dir keystore must not print interactive keychain prompt notice"),
            || panic!("dir keystore from steve.env must not invoke steve-keyhelper"),
            || panic!("dir keystore from steve.env must not prompt login keychain"),
        )
        .unwrap();
        assert_eq!(
            got_dir.as_deref().map(String::as_str),
            Some(expected_pass.as_str())
        );

        // Case B: Signed macOS Keychain installation run from unsigned /tmp binary in a TTY:
        // steve-keyhelper refuses with Denied -> prints Allow-only notice and falls back to interactive Keychain.
        let kc_home = root.path().join("kc-home");
        fs::create_dir_all(kc_home.join("signing")).unwrap();
        fs::write(
            kc_home.join("signing/identity.json"),
            r#"{"label":"test.hub","hub_identifier":"h","keyhelper_identifier":"k","vault_service":"test.vault"}"#,
        )
        .unwrap();
        let mut interactive_called = false;
        let mut prompt_notice = String::new();
        let got_kc = super::signing::resolve_maintenance_password_with(
            &kc_home,
            &|_| None,
            None,
            true,
            |notice| prompt_notice = notice.to_owned(),
            || Err(KeyStoreError::Denied),
            || {
                interactive_called = true;
                Ok(Some(zeroize::Zeroizing::new(expected_pass.clone())))
            },
        )
        .unwrap();
        assert!(
            interactive_called,
            "must fall back to interactive keychain on helper denial"
        );
        assert!(
            prompt_notice.contains("Allow") && prompt_notice.contains("Always Allow"),
            "interactive fallback must warn operator to click Allow only: {prompt_notice}"
        );
        assert_eq!(
            got_kc.as_deref().map(String::as_str),
            Some(expected_pass.as_str())
        );

        // Case C: --keychain-pass-stdin accepts both raw 64-hex and the 32-byte base64 Keychain
        // item printed by `security find-generic-password -a steve/signing/keychain-pass -w`.
        use base64::Engine as _;
        let raw_bytes = hex::decode(&expected_pass).unwrap();
        let keychain_b64 = base64::engine::general_purpose::STANDARD.encode(&raw_bytes);
        for stdin_val in [expected_pass.clone(), keychain_b64] {
            let got_stdin = super::signing::resolve_maintenance_password_with(
                &kc_home,
                &|_| None,
                Some(zeroize::Zeroizing::new(stdin_val)),
                false,
                |_| panic!("stdin password must not print interactive notice"),
                || panic!("stdin password must bypass helper"),
                || panic!("stdin password must bypass interactive keychain"),
            )
            .unwrap();
            assert_eq!(
                got_stdin.as_deref().map(String::as_str),
                Some(expected_pass.as_str())
            );
        }
    }

    #[test]
    fn review593_re_keychain_requires_tty_warns_allow_only() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("kc-home");
        fs::create_dir_all(home.join("signing")).unwrap();
        fs::write(
            home.join("signing/identity.json"),
            r#"{"label":"test.hub","hub_identifier":"h","keyhelper_identifier":"k","vault_service":"custom.vault"}"#,
        )
        .unwrap();

        // Non-TTY stdin must REFUSE interactive Keychain fallback and never invoke it.
        let non_tty = super::signing::resolve_maintenance_password_with(
            &home,
            &|_| None,
            None,
            false,
            |_| panic!("non-TTY must refuse before emitting interactive prompt notice"),
            || Err(KeyStoreError::Denied),
            || panic!("non-TTY must refuse before calling interactive Keychain"),
        );
        assert!(
            non_tty
                .as_ref()
                .is_err_and(|e| e.to_string().contains("TTY")
                    && e.to_string().contains("--keychain-pass-stdin")),
            "expected TTY refusal pointing to --keychain-pass-stdin, got: {non_tty:?}"
        );

        // TTY stdin emits the Allow-only / no-Always-Allow warning BEFORE invoking Keychain.
        let order = std::cell::RefCell::new(Vec::new());
        let pass = "0123456789abcdef".repeat(4);
        let got = super::signing::resolve_maintenance_password_with(
            &home,
            &|_| None,
            None,
            true,
            |notice| {
                assert!(
                    notice.contains("允许")
                        && notice.contains("始终允许")
                        && notice.contains("Allow")
                        && notice.contains("Always Allow"),
                    "notice must warn Allow-only in both English and Chinese: {notice}"
                );
                order.borrow_mut().push("notice");
            },
            || Err(KeyStoreError::Denied),
            || {
                order.borrow_mut().push("keychain");
                Ok(Some(zeroize::Zeroizing::new(pass.clone())))
            },
        )
        .unwrap();
        assert_eq!(*order.borrow(), vec!["notice", "keychain"]);
        assert_eq!(got.as_deref().map(String::as_str), Some(pass.as_str()));
    }
}
