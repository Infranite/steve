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
    common::role_home(role, selected.as_deref().map(Path::new))
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
        Some(tag) => {
            steve_i18n::match_tag(&tag).with_context(|| format!("--lang {tag}: zh-CN or en"))?
        }
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
        "setup" => setup_command(args),
        "hub" | "node" => service_command(&command, args),
        "adapters" => adapters_command(args),
        "upgrade" => upgrade_command(args),
        "login" => login_command(args),
        "publish" => publish_command(args),
        _ => bail!("unknown command; use steve --help"),
    }
}

fn setup_command(mut args: Vec<String>) -> Result<i32> {
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

fn service_command(command: &str, mut args: Vec<String>) -> Result<i32> {
    if args.is_empty() {
        bail!("{}", t!("install.cli.service_action_missing"));
    }
    let action = args.remove(0);
    if ![
        "install",
        "enable-boot",
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
    anyhow::ensure!(
        action != "enable-boot"
            || (cfg!(target_os = "macos") && command == "node" && release.is_none()),
        "enable-boot is only for a prepared macOS Node; it takes no --release"
    );
    let mut service = steve_install::service::Service::new(&command, &home, label.as_deref())?;
    let mut report = service.dispatch(&action, release.as_deref(), wait)?;
    print_service_report(command, &action, &home, label.as_deref(), &mut report, json)?;
    Ok(if report["ok"] == false { 1 } else { 0 })
}

fn adapters_command(mut args: Vec<String>) -> Result<i32> {
    let selected = option(&mut args, "--home")?;
    let action = if args
        .first()
        .is_some_and(|a| !matches!(a.as_str(), "hub" | "node"))
    {
        args.remove(0)
    } else {
        "status".into()
    };
    if !matches!(action.as_str(), "status" | "install" | "upgrade") {
        bail!("{}", t!("install.cli.adapters_action_missing"));
    }
    /* install <id>…: those adapters, added to what this home keeps */
    let mut ids = Vec::new();
    while action == "install"
        && args
            .first()
            .is_some_and(|a| !matches!(a.as_str(), "hub" | "node") && !a.starts_with("--"))
    {
        ids.push(args.remove(0));
    }
    /* The role is explicit or comes from this home's installation records. */
    let requested = match args.first().map(String::as_str) {
        Some(r @ ("hub" | "node")) => {
            let r = r.to_string();
            args.remove(0);
            Some(r)
        }
        _ => None,
    };
    if !args.is_empty() {
        bail!("unknown adapters arguments");
    }
    let home = match (selected, requested.as_deref()) {
        (Some(selected), _) => common::canonical(Path::new(&selected))?,
        (None, Some(role)) => role_home(role, None)?,
        (None, None) => {
            let hub = role_home("hub", None)?;
            if steve_install::service::has_active_registration(&hub, "hub")? {
                hub
            } else {
                role_home("node", None)?
            }
        }
    };
    let (role, current) = steve_install::service::adapter_target(&home, requested.as_deref())
        .with_context(|| t!("install.cli.adapters_home_invalid", home = home.display()))?;
    let mut passed = vec![if action == "upgrade" {
        "install"
    } else {
        action.as_str()
    }];
    passed.extend(ids.iter().map(String::as_str));
    setup::adapters(&role, &home, &current, &passed)
}

fn upgrade_command(mut args: Vec<String>) -> Result<i32> {
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
        bail!("{}", t!("install.cli.upgrade_unsupported"));
    }
    let status = common::clean_command(&current.join("bin/steve-hub"), Some(&home))
        .arg("--upgrade-cli")
        .arg(&home)
        .args(&args)
        .status()?;
    Ok(status.code().unwrap_or(1))
}

fn login_command(mut args: Vec<String>) -> Result<i32> {
    use steve_install::publish;
    let file = publish::login_file()?;
    let saved = publish::saved_login(&file)?;
    let hub = option(&mut args, "--hub")?
        .or_else(|| std::env::var("STEVE_HUB_URL").ok())
        .or_else(|| saved.as_ref().map(|l| l.hub.clone()))
        .or_else(|| {
            role_home("hub", None)
                .ok()
                .and_then(|h| setup::hub_urls(&h))
                .map(|(api, _)| api)
        })
        .context(t!("install.cli.login_hub_missing"))?;
    let user = option(&mut args, "--user")?
        .or_else(|| std::env::var("USER").ok())
        .context(t!("install.cli.login_user_missing"))?;
    if !args.is_empty() {
        bail!("unknown login arguments; use steve --help");
    }
    let token = match std::env::var("STEVE_ACCESS_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => publish::read_secret(&t!("install.cli.login_token_prompt"))?,
    };
    let login = publish::login(&hub, &user, &token)?;
    publish::save_login(&file, &login)?;
    println!(
        "{}",
        t!(
            "install.cli.login_done",
            user = login.user,
            hub = login.hub,
            file = file.display()
        )
    );
    Ok(0)
}

fn publish_command(mut args: Vec<String>) -> Result<i32> {
    use steve_install::publish;
    let title = option(&mut args, "--title")?;
    let entry = option(&mut args, "--entry")?;
    let workspace = option(&mut args, "--workspace")?;
    let hub = option(&mut args, "--hub")?;
    let interactive = flag(&mut args, "--interactive")?;
    let json = flag(&mut args, "--json")?;
    if args.len() != 1 || args[0].starts_with("--") {
        bail!("{}", t!("install.cli.publish_usage"));
    }
    let path = PathBuf::from(args.remove(0));
    let mut login = match std::env::var("STEVE_CLIENT_CREDENTIAL") {
        Ok(c) if !c.trim().is_empty() => publish::Login {
            hub: std::env::var("STEVE_HUB_URL").unwrap_or_default(),
            user: String::new(),
            credential: c.trim().to_string(),
        },
        _ => publish::saved_login(&publish::login_file()?)?
            .context(t!("install.cli.publish_login_missing"))?,
    };
    // The saved credential belongs to the Hub it signed in to; never send it elsewhere.
    if let Some(h) = hub {
        let h = publish::normalize_hub(&h)?;
        if !login.hub.is_empty() && login.hub != h {
            bail!("{}", t!("install.cli.publish_other_hub", hub = login.hub));
        }
        login.hub = h;
    }
    login.hub =
        publish::normalize_hub(&login.hub).context(t!("install.cli.publish_login_missing"))?;
    let d = publish::publish(
        &login,
        &publish::Publish {
            path: &path,
            title: title.as_deref(),
            entry: entry.as_deref(),
            interactive,
            workspace: workspace.as_deref(),
        },
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&d)?);
    } else {
        println!("{}", d["url"].as_str().unwrap_or_default());
        if d["reachability"] == "local" {
            eprintln!("{}", t!("install.cli.publish_local"));
        }
        if d["scripts"] == true && d["interactive"] != true {
            eprintln!("{}", t!("install.cli.publish_static_scripts"));
        }
    }
    Ok(0)
}

fn print_service_report(
    command: &str,
    action: &str,
    home: &Path,
    label: Option<&str>,
    report: &mut serde_json::Value,
    json: bool,
) -> Result<()> {
    if json || !std::io::stdout().is_terminal() {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        /* for a person, the version comes from the running Hub itself */
        if command == "hub"
            && matches!(action, "status" | "doctor")
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
            ui::summary(&output::service(command, action, home, label, &report))
        );
        /* the service manager has the job; say when the Hub itself answers */
        let started =
            command == "hub" && matches!(action, "start" | "restart") && report["ok"] != false;
        if let (true, Some((api, web))) = (started, setup::hub_urls(&home)) {
            let spinner = ui::Spinner::start(&t!("install.cli.hub_waiting"));
            match setup::hub_ready(&api, std::time::Duration::from_secs(45)) {
                Some(health) => spinner.done(
                    &t!("install.cli.hub_ready"),
                    &t!(
                        "install.cli.hub_ready_detail",
                        version = health["version"].as_str().unwrap_or("?"),
                        web = web
                    ),
                ),
                None => spinner.warn(&t!("install.cli.hub_silent")),
            }
        }
    }
    Ok(())
}
