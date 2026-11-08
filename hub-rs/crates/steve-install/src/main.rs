use anyhow::{Context, Result, bail};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use steve_install::{common, setup};

mod output;

const HELP: &str = "Steve 原生管理 CLI\n\n  steve setup [hub|node] [--home PATH] [--listen 0.0.0.0:8440]\n              [--hub https://hub.example] [--pin sha256:FINGERPRINT]\n  steve setup [hub] --reconfigure [--home PATH] [--listen IP:port]\n  steve hub|node start|stop|restart|status|doctor|uninstall [--home PATH] [--label LABEL] [--json]\n  steve hub|node install --release PATH [--home PATH] [--label LABEL]\n  steve node update --release PATH [--wait SECONDS]\n  steve upgrade check|request|status|watch [--home PATH] [options]\n\nsetup 默认监听 0.0.0.0:8440，浏览器/OIDC 地址单独配置。\nsetup 注册服务，本次不启动服务；不导入登录凭据或授予 Node 工作目录。\n--reconfigure 只维护已安装 Hub 的网络与现有 OIDC 配置，保留 secret、身份和数据；不会自动重启。\n服务命令在终端显示摘要；管道或 --json 保持 JSON 输出。\n";

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
        Err(error) => {
            eprintln!("steve: {error:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run(mut args: Vec<String>) -> Result<i32> {
    if args.is_empty() || args.iter().any(|v| v == "--help" || v == "-h") {
        print!("{HELP}");
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
                home: option(&mut args, "--home")?.map(PathBuf::from),
                hub: option(&mut args, "--hub")?,
                pin: option(&mut args, "--pin")?,
                listen: option(&mut args, "--listen")?,
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
                bail!("请选择 start/stop/restart/status/doctor/update/uninstall");
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
            let report = service.dispatch(&action, release.as_deref(), wait)?;
            if json || !std::io::stdout().is_terminal() {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!(
                    "{}",
                    output::service(&command, &action, &home, label.as_deref(), &report)
                );
            }
            Ok(if report["ok"] == false { 1 } else { 0 })
        }
        "upgrade" => {
            let home = role_home("hub", option(&mut args, "--home")?)?;
            if args.is_empty() {
                bail!("请选择 upgrade check/request/status/watch");
            }
            let current = home
                .join("current")
                .canonicalize()
                .context("Hub 尚未初始化；请先运行 steve setup")?;
            let releases = home.join("releases").canonicalize()?;
            if current.parent() != Some(releases.as_path()) {
                bail!("current 不属于此安装的 releases");
            }
            let output = common::clean_command(&current.join("bin/steve-hub"), Some(&home))
                .arg("--build-info")
                .output()?;
            let info: serde_json::Value = serde_json::from_slice(&output.stdout)
                .context("无法核验已安装 Hub 的原生升级能力")?;
            if !output.status.success() || info["upgrade_runner"]["native"] != true {
                bail!(
                    "当前 Hub 尚不支持原生升级入口；请先在旧版 Steve 设置页完成这一次升级，再使用 steve upgrade。"
                );
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
