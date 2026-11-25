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
            json!({"usage":"--local-release setup-keychain|prepare-bundle|deploy <bundle> --home PATH; upgrade-macos [<bundle>] [--home PATH] [--label LABEL] [--timeout SECS] [--keychain-pass-stdin]; rollback-macos [--home PATH] [--label LABEL] [--timeout SECS]; prune --home PATH [--keep PATH] [--dry-run]"}),
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
    let (mut password_stdin, mut dry) = (false, false);
    let mut keep = vec![];
    while index < args.len() {
        let name = args[index].as_str();
        index += 1;
        if name == "--keychain-pass-stdin" && matches!(command, "prepare-bundle" | "upgrade-macos")
        {
            password_stdin = true;
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
            let password = if password_stdin {
                let mut raw = zeroize::Zeroizing::new(String::new());
                std::io::stdin().read_line(&mut raw)?;
                Some(zeroize::Zeroizing::new(raw.trim().to_owned()))
            } else if home.join("signing/identity.json").is_file() {
                let store = super::signing::installed_helper(&home);
                super::signing::password(store.as_deref()).ok().flatten()
            } else {
                None
            };
            let result = steve_install::release::upgrade_macos(
                &home,
                &bundle,
                label.as_deref(),
                Duration::from_secs(timeout),
                password.as_deref().map(String::as_str),
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
            )?;
            Ok((result.exit_code, result.receipt))
        }
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
}
