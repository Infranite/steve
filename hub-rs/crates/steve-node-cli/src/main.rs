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
use steve_node_wire::Identity;

fn usage() -> ! {
    eprintln!(
        "usage: steve-node pair <hub-url> [--pin sha256:…] [--name NAME] | run | rotate | status | forget | --build-info | --version\n\
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
    let home = Home::from_env()?;
    match args.first().map(String::as_str) {
        Some("pair") => {
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
            let p = dial::pair(&home, url, &code()?, pin, &name).await?;
            let id = Identity::load(&home.identity())?;
            println!("paired as {} with hub {}", p.node_id, p.hub_id);
            println!("fingerprint {}", id.fingerprint());
            println!("confirm this fingerprint on the hub, then run: steve-node run");
        }
        Some("run") => dial::run(&home).await?,
        Some("rotate") => {
            let fp = dial::rotate(&home).await?;
            println!("new key in place; fingerprint {fp}");
            println!(
                "restart the Node now (steve node restart): its running process still has the old key, which the hub no longer takes"
            );
        }
        Some("status") => {
            let id = Identity::load(&home.identity())?;
            match home.pairing() {
                Ok(p) => println!(
                    "node {} ({}) paired with {} at {}\nfingerprint {}",
                    p.node_id,
                    p.name,
                    p.hub_id,
                    p.hub_url,
                    id.fingerprint()
                ),
                Err(e) => println!("{e}\nfingerprint {}", id.fingerprint()),
            }
        }
        Some("forget") => {
            // local only: revoking is the Hub's (its owner's) act
            for f in [home.pairing_file(), home.identity()] {
                match std::fs::remove_file(&f) {
                    Ok(()) => println!("removed {}", f.display()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        _ => usage(),
    }
    Ok(())
}
