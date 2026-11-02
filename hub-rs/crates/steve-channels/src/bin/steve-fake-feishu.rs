//! Standalone fake Feishu for out-of-process tests (e2e against the Hub binary).
//!
//! `steve-fake-feishu [--port N] [--hook-url URL]` serves the open-platform endpoints on
//! 127.0.0.1 with the sample tenant, prints the base URL, and exposes localhost-only control
//! routes: `GET /_fake/sent`, `POST /_fake/push` (`{event_type, event}` or a full envelope),
//! `POST /_fake/url_verification`, `POST /_fake/redeliver/{event_id}`, `POST /_fake/expire_tokens`, `POST /_fake/device/approve`
//! and `/_fake/device/deny` (`{user_code?}`, default the newest pending device code), and for the long connection
//! `POST /_fake/ws/push`, `GET /_fake/ws/deliveries`, `POST /_fake/ws/disconnect`.

use std::net::SocketAddr;

use steve_channels::fake::feishu::{FakeFeishu, World};

#[tokio::main]
async fn main() {
    let mut port: u16 = 0;
    let mut hook: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => port = args.next().and_then(|p| p.parse().ok()).unwrap_or(0),
            "--hook-url" => hook = args.next(),
            "-h" | "--help" => {
                println!("usage: steve-fake-feishu [--port N] [--hook-url URL]");
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let fake =
        match FakeFeishu::bind(World::sample(), SocketAddr::from(([127, 0, 0, 1], port))).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("cannot bind: {e}");
                std::process::exit(1);
            }
        };
    if let Some(h) = hook {
        fake.set_hook_url(h);
    }
    let app = fake.app();
    println!("{}", fake.base_url());
    eprintln!(
        "fake feishu: app_id={} verification_token={} encrypt_key={}",
        app.app_id, app.verification_token, app.encrypt_key
    );
    let _ = tokio::signal::ctrl_c().await;
}
