//! Standalone fake GitHub for manual UI checks (W18) and scripts.
//!
//! ```text
//! steve-fake-github [--addr 127.0.0.1:8445] [--hook http://127.0.0.1:<hub>/hooks/github]
//! steve-fake-github emit <hook-url> <fixture> [<fixture>…]
//! steve-fake-github fixtures
//! ```
//! `serve` prints one JSON line (api base, App id, installation id, PAT,
//! webhook secret, private key) and runs until Ctrl-C. `emit` signs recorded
//! fixtures with the default webhook secret and POSTs them.

use std::net::SocketAddr;

use steve_github::fake::{FIXTURES, FakeGitHub, WebhookSender, defaults, fixture};

type Res = Result<(), Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Res {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("emit") => emit(&args[1..]).await,
        Some("fixtures") => {
            for (name, event, _) in FIXTURES {
                println!("{name}\t{event}");
            }
            Ok(())
        }
        Some("-h" | "--help") => {
            println!(
                "usage: steve-fake-github [--addr ADDR] [--hook URL] | emit URL FIXTURE… | fixtures"
            );
            Ok(())
        }
        _ => serve(&args).await,
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

async fn serve(args: &[String]) -> Res {
    let addr: SocketAddr = flag(args, "--addr").unwrap_or("127.0.0.1:0").parse()?;
    let gh = FakeGitHub::start_on(addr).await?;
    let code = gh.manifest_code();
    if let Some(hook) = flag(args, "--hook") {
        gh.set_webhook_target(hook);
    }
    let info = serde_json::json!({
        "api_base": gh.url(),
        "app_id": defaults::APP_ID,
        "app_slug": defaults::APP_SLUG,
        "installation_id": defaults::INSTALLATION_ID,
        "repo": defaults::REPO,
        "pat": defaults::PAT,
        "webhook_secret": defaults::WEBHOOK_SECRET,
        "manifest_code": code,
        "private_key_pem": defaults::APP_PRIVATE_KEY_PEM,
    });
    println!("{info}");
    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn emit(args: &[String]) -> Res {
    let (Some(url), names) = (args.first(), &args[1.min(args.len())..]) else {
        return Err("usage: steve-fake-github emit <hook-url> <fixture>…".into());
    };
    let sender = WebhookSender::new(url.as_str(), defaults::WEBHOOK_SECRET.as_bytes().to_vec());
    for name in names {
        let (event, payload) = fixture(name)?;
        let d = sender.send(event, &payload).await?;
        println!("{name}\t{event}\t{}\t{}", d.id, d.status);
    }
    Ok(())
}
