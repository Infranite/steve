use anyhow::{Context, Result, bail};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use steve_i18n::t;
use steve_install::{common, setup, ui};

mod output;

steve_i18n::init!();

fn option(args: &mut Vec<String>, name: &str) -> Result<Option<String>> {
    let mut found = None;
    while let Some(at) = args.iter().position(|v| v == name) {
        if found.is_some() || at + 1 == args.len() || args[at + 1].starts_with("--") {
            bail!("{name} requires one value and cannot repeat");
        }
        args.remove(at);
        found = Some(args.remove(at));
    }
    Ok(found)
}

fn flag(args: &mut Vec<String>, name: &str) -> Result<bool> {
    let Some(at) = args.iter().position(|value| value == name) else {
        return Ok(false);
    };
    args.remove(at);
    if args.iter().any(|value| value == name) {
        bail!("{name} cannot repeat");
    }
    Ok(true)
}

fn role_home(role: &str, selected: Option<String>) -> Result<PathBuf> {
    common::canonical(
        &selected
            .map(PathBuf::from)
            .unwrap_or(common::user_home()?.join(if role == "hub" {
                ".steve"
            } else {
                ".steve-node"
            })),
    )
}

fn bundle() -> Result<PathBuf> {
    let executable = std::env::current_exe()?.canonicalize()?;
    Ok(executable
        .parent()
        .and_then(Path::parent)
        .context("installed CLI is not inside a release bundle")?
        .to_path_buf())
}

fn main() {
    let code = match run(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        /* the prompt already said so */
        Err(error) if error.is::<ui::Cancelled>() => 130,
        Err(error) => {
            if ui::decorated() {
                ui::fail(&format!("{error:#}"));
            } else {
                eprintln!("steve: {error:#}");
            }
            1
        }
    };
    std::process::exit(code);
}

fn run(mut args: Vec<String>) -> Result<i32> {
    /* the language of everything printed: --lang, else STEVE_LANG, the locale, the system's */
    let lang = match option(&mut args, "--lang")? {
        Some(tag) => steve_i18n::match_tag(&tag).with_context(|| format!("--lang {tag}: zh-CN or en"))?,
        None => steve_i18n::from_env(),
    };
    steve_i18n::set_process(lang);
    if args.is_empty() || args.iter().any(|v| v == "--help" || v == "-h") {
        print!("{}", t!("install.help"));
        return Ok(0);
    }
    if args == ["--version"] {
        println!(
            "steve CLI/package {}",
            steve_install::build_info()["version"].as_str().unwrap()
        );
        return Ok(0);
    }
    if args == ["--build-info"] {
        println!("{}", steve_install::build_info());
        return Ok(0);
    }
    let command = args.remove(0);
    match command.as_str() {
        "setup" => {
            let options = setup::Options {
                reconfigure: flag(&mut args, "--reconfigure")?,
                fresh: flag(&mut args, "--fresh")?,
                home: option(&mut args, "--home")?.map(PathBuf::from),
                hub: option(&mut args, "--hub")?,
                pin: option(&mut args, "--pin")?,
                listen: option(&mut args, "--listen")?,
                public_url: option(&mut args, "--public-url")?,
                adapters: option(&mut args, "--adapters")?,
                label: option(&mut args, "--label")?,
                vault_dir: option(&mut args, "--vault-dir")?.map(PathBuf::from),
                role: if args
                    .first()
                    .is_some_and(|v| matches!(v.as_str(), "hub" | "node"))
                {
                    Some(args.remove(0))
                } else {
                    None
                },
            };
            if !args.is_empty() {
                bail!("unknown setup arguments; use steve --help");
            }
            if options.reconfigure {
                setup::reconfigure_existing(options)?;
            } else {
                setup::run(&bundle()?, options)?;
            }
            Ok(0)
        }
        "hub" | "node" => {
            if args.is_empty() {
                bail!("{}", t!("install.cli.service_action_missing"));
            }
            let action = args.remove(0);
            if ![
                "install",
                "start",
                "stop",
                "restart",
                "status",
                "doctor",
                "update",
                "uninstall",
            ]
            .contains(&action.as_str())
            {
                bail!("unknown service action");
            }
            let json = if let Some(at) = args.iter().position(|v| v == "--json") {
                args.remove(at);
                true
            } else {
                false
            };
            let home = role_home(&command, option(&mut args, "--home")?)?;
            let label = option(&mut args, "--label")?;
            let release = option(&mut args, "--release")?.map(PathBuf::from);
            let wait = option(&mut args, "--wait")?
                .map(|v| v.parse::<u64>())
                .transpose()?
                .unwrap_or(30);
            if !args.is_empty() {
                bail!("unknown service arguments");
            }
            let mut service =
                steve_install::service::Service::new(&command, &home, label.as_deref())?;
            let mut report = service.dispatch(&action, release.as_deref(), wait)?;
            if json || !std::io::stdout().is_terminal() {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                /* for a person, the version comes from the running Hub itself */
                if command == "hub"
                    && matches!(action.as_str(), "status" | "doctor")
                    && report["service"]["running"] == true
                    && report["running_build"].is_null()
                {
                    let health = setup::hub_urls(&home)
                        .and_then(|(api, _)| setup::hub_ready(&api, std::time::Duration::from_secs(2)));
                    if let Some(health) = health {
                        report["running_build"] = serde_json::json!({
                            "version": health["version"], "commit": health["build_commit"]});
                    }
                }
                print!(
                    "{}",
                    ui::summary(&output::service(&command, &action, &home, label.as_deref(), &report))
                );
                /* the service manager has the job; say when the Hub itself answers */
                let started = command == "hub"
                    && matches!(action.as_str(), "start" | "restart")
                    && report["ok"] != false;
                if let (true, Some((api, web))) = (started, setup::hub_urls(&home)) {
                    let spinner = ui::Spinner::start(&t!("install.cli.hub_waiting"));
                    match setup::hub_ready(&api, std::time::Duration::from_secs(45)) {
                        Some(health) => spinner.done(
                            &t!("install.cli.hub_ready"),
                            &t!("install.cli.hub_ready_detail", version = health["version"].as_str().unwrap_or("?"), web = web),
                        ),
                        None => spinner.warn(&t!("install.cli.hub_silent")),
                    }
                }
            }
            Ok(if report["ok"] == false { 1 } else { 0 })
        }
        "adapters" => {
            let selected = option(&mut args, "--home")?;
            let action = if args.first().is_some_and(|a| !matches!(a.as_str(), "hub" | "node")) {
                args.remove(0)
            } else {
                "status".into()
            };
            let action = if action == "upgrade" { "install".to_string() } else { action };
            if !matches!(action.as_str(), "status" | "install") {
                bail!("{}", t!("install.cli.adapters_action_missing"));
            }
            /* install <id>…: those adapters, added to what this home keeps */
            let mut ids = Vec::new();
            while action == "install"
                && args.first().is_some_and(|a| !matches!(a.as_str(), "hub" | "node") && !a.starts_with("--"))
            {
                ids.push(args.remove(0));
            }
            /* hub or node: as given, else the Hub's home when there is one */
            let role = match args.first().map(String::as_str) {
                Some(r @ ("hub" | "node")) => {
                    let r = r.to_string();
                    args.remove(0);
                    r
                }
                _ if selected.is_none() && !role_home("hub", None)?.join("current").exists() => {
                    "node".into()
                }
                _ => "hub".into(),
            };
            if !args.is_empty() {
                bail!("unknown adapters arguments");
            }
            let home = role_home(&role, selected)?;
            let current = home
                .join("current")
                .canonicalize()
                .with_context(|| t!("install.cli.role_not_set_up", role = role))?;
            let mut passed = vec![action.as_str()];
            passed.extend(ids.iter().map(String::as_str));
            setup::adapters(&role, &home, &current, &passed)
        }
        "upgrade" => {
            let home = role_home("hub", option(&mut args, "--home")?)?;
            if args.is_empty() {
                bail!("{}", t!("install.cli.upgrade_action_missing"));
            }
            let current = home
                .join("current")
                .canonicalize()
                .context(t!("install.cli.hub_not_set_up"))?;
            let releases = home.join("releases").canonicalize()?;
            if current.parent() != Some(releases.as_path()) {
                bail!("{}", t!("install.cli.current_outside_releases"));
            }
            let output = common::clean_command(&current.join("bin/steve-hub"), Some(&home))
                .arg("--build-info")
                .output()?;
            let info: serde_json::Value = serde_json::from_slice(&output.stdout)
                .context(t!("install.cli.upgrade_capability_unknown"))?;
            if !output.status.success() || info["upgrade_runner"]["native"] != true {
                bail!(
                    "{}", t!("install.cli.upgrade_unsupported"));
            }
            let status = common::clean_command(&current.join("bin/steve-hub"), Some(&home))
                .arg("--upgrade-cli")
                .arg(&home)
                .args(&args)
                .status()?;
            Ok(status.code().unwrap_or(1))
        }
        _ => bail!("unknown command; use steve --help"),
    }
}
