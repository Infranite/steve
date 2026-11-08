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
            json!({"usage":"--local-release setup-keychain|prepare-bundle|deploy <bundle> --home PATH; prune --home PATH [--keep PATH] [--dry-run]"}),
        ));
    }
    if command == "schema-state" && args.len() == 2 {
        return Ok((
            0,
            steve_install::release::schema_state(Path::new(&args[1]))?,
        ));
    }
    if command == "schema-check" && args.len() == 3 {
        steve_install::release::check_schema(Path::new(&args[1]), Path::new(&args[2]))?;
        return Ok((0, json!({"compatible":true})));
    }
    if !["setup-keychain", "prepare-bundle", "deploy", "prune"].contains(&command) {
        bail!("unsupported native maintenance command")
    }
    let mut index = 1;
    let candidate = if command != "prune" {
        let path = args
            .get(index)
            .filter(|s| !s.starts_with('-'))
            .context("bundle path required")?;
        index += 1;
        Some(PathBuf::from(path))
    } else {
        None
    };
    let mut home = None;
    let mut label = "io.github.infranite.steve.hub".to_owned();
    let (mut version, mut commit, mut operation) = (None, None, None);
    let mut timeout = 600_u64;
    let (mut password_stdin, mut dry) = (false, false);
    let mut keep = vec![];
    while index < args.len() {
        let name = args[index].as_str();
        index += 1;
        if name == "--keychain-pass-stdin" && command == "prepare-bundle" {
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
            "--label" if command == "setup-keychain" => label = value,
            "--version" if command == "prepare-bundle" => version = Some(value),
            "--commit" if command == "prepare-bundle" => commit = Some(value),
            "--operation" if command == "deploy" => operation = Some(value),
            "--timeout" | "--drain-s" if command == "deploy" => timeout = value.parse()?,
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
            steve_install::signing::setup_keychain(&home, &candidate.unwrap(), &label)?,
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
