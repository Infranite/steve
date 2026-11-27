//! `steve-node`: host Steve's harnesses on this machine for a Hub.
//!
//! ```text
//! steve-node pair <hub-url> [--pin sha256:…] [--name NAME]   (code on stdin or STEVE_PAIR_CODE)
//! steve-node run
//! steve-node status
//! steve-node forget
//! ```

use anyhow::{Result, bail};
use steve_node_cli::config::Home;
use steve_node_cli::dial;
use steve_node_wire::{Identity, identity};

fn usage() -> ! {
    eprintln!(
        "usage: steve-node pair <hub-url> [--pin sha256:…] [--name NAME] | run | rotate | status | adapters [status|install] | forget | --build-info | --version\n\
         The pairing code is read from STEVE_PAIR_CODE or stdin, never from the command line."
    );
    std::process::exit(2)
}

fn code() -> Result<String> {
    if let Ok(c) = std::env::var("STEVE_PAIR_CODE")
        && !c.is_empty()
    {
        return Ok(c.trim().to_string());
    }
    eprint!("pairing code: ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let c = line.trim().to_string();
    if c.is_empty() {
        bail!("no pairing code");
    }
    Ok(c)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("STEVE_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--build-info") {
        println!(
            "{}",
            serde_json::json!({
                "version": option_env!("STEVE_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
                "commit": option_env!("STEVE_BUILD_COMMIT").unwrap_or("unknown"),
                "target": env!("STEVE_BUILD_TARGET"),
            })
        );
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--version") {
        println!(
            "steve-node {}",
            option_env!("STEVE_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
        );
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--local-node") {
        return steve_node_cli::local_node::run().await;
    }
    if args.first().map(String::as_str) == Some("harness-probe") {
        let report = steve_node_cli::local_node::probe_harnesses(|k| std::env::var(k).ok());
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    let home = Home::from_env()?;
    match args.first().map(String::as_str) {
        // the switch helper a Hub-asked upgrade starts, outside the service (see `upgrade`)
        Some("upgrade-switch") => {
            let Some(op) = args.get(1) else { usage() };
            let mut svc = steve_node_cli::upgrade::InstalledService::open(&home.0)?;
            let s = steve_node_cli::upgrade::run_switch(
                &home.0,
                op,
                &mut svc,
                std::time::Duration::from_millis(500),
            )?;
            println!("{}", serde_json::to_string(&s)?);
        }
        // the helper a removal the Hub confirmed starts, outside the service (see `remove`)
        Some("remove-finish") => {
            let Some(op) = args.get(1) else { usage() };
            let mut svc = steve_node_cli::upgrade::InstalledService::open(&home.0)?;
            steve_node_cli::remove::run_remove(
                &home.0,
                op,
                &mut svc,
                std::time::Duration::from_secs(2),
                std::time::Duration::from_secs(1),
            )?;
            println!(
                "removed this node's service and its home {}",
                home.0.display()
            );
        }
        Some("pair") => pair_command(&home, &args).await?,
        Some("run") => dial::run(&home).await?,
        Some("rotate") => {
            let fp = dial::rotate(&home).await?;
            println!("new key in place; fingerprint {}", identity::grouped(&fp));
            println!(
                "restart the Node now (steve node restart): its running process still has the old key, which the hub no longer takes"
            );
        }
        Some("status") => print_status(&home)?,
        Some("adapters") => {
            let h = home.harness()?;
            let code = steve_acp::managed::cli(&home.0, h.node.as_deref(), &args[1..]);
            if code != 0 {
                std::process::exit(code);
            }
        }
        Some("forget") => forget_pairing(&home)?,
        _ => usage(),
    }
    Ok(())
}

/// Parse pairing options, claim the code and show the fingerprint the owner confirms.
async fn pair_command(home: &Home, args: &[String]) -> Result<()> {
    let Some(url) = args.get(1) else { usage() };
    let mut pin = None;
    let mut name = std::env::var("HOSTNAME").unwrap_or_else(|_| "node".into());
    let mut it = args[2..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pin" => pin = it.next().cloned(),
            "--name" => name = it.next().cloned().unwrap_or(name),
            _ => usage(),
        }
    }
    let p = dial::pair(home, url, &code()?, pin, &name).await?;
    let id = Identity::load(&home.identity())?;
    println!("paired as {} with hub {}", p.node_id, p.hub_id);
    println!("fingerprint {}", identity::grouped(&id.fingerprint()));
    println!(
        "confirm this fingerprint on the hub (Devices, Add device: paste it or type its eight groups), then run: steve-node run"
    );
    Ok(())
}

/// Report the local identity even when its pairing is missing.
fn print_status(home: &Home) -> Result<()> {
    let id = Identity::load(&home.identity())?;
    match home.pairing() {
        Ok(p) => println!(
            "node {} ({}) paired with {} at {}\nfingerprint {}",
            p.node_id,
            p.name,
            p.hub_id,
            p.hub_url,
            identity::grouped(&id.fingerprint())
        ),
        Err(e) => println!("{e}\nfingerprint {}", identity::grouped(&id.fingerprint())),
    }
    Ok(())
}

/// Forget local pairing and identity in order; revocation remains the Hub owner's action.
fn forget_pairing(home: &Home) -> Result<()> {
    // local only: revoking is the Hub's (its owner's) act
    for f in [home.pairing_file(), home.identity()] {
        match std::fs::remove_file(&f) {
            Ok(()) => println!("removed {}", f.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
