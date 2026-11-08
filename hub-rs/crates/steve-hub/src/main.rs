//! `steve-hub`: one HTTP server — /mcp for agent tools, /auth and /api for the web app, static files for the rest.
//! The whole of it is [`steve_hub::run`] on the built-in extensions.

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("--upgrade-cli") => Some(steve_hub::upgrades::cli::main(&args[1..])),
        Some("--local-release") => Some(steve_hub::upgrades::maintenance::main(&args[1..])),
        Some("--upgrade-runner") if args.len() == 3 => Some(steve_hub::upgrades::runner::main(
            std::path::Path::new(&args[1]),
            &args[2],
        )),
        Some("--build-info") => {
            println!(
                "{}",
                serde_json::json!({
                    "version": option_env!("STEVE_BUILD_VERSION"),
                    "commit": option_env!("STEVE_BUILD_COMMIT"),
                    "target": env!("STEVE_BUILD_TARGET"),
                    "schema": steve_store::db::SCHEMA_VERSION,
                    "check_config": true,
                    "upgrade_runner": {"journal": 1, "kinds": ["release_bundle", "local_release"], "native": true},
                    "native_install": 1
                })
            );
            Some(0)
        }
        _ => None,
    };
    if let Some(code) = code {
        std::process::exit(code);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(steve_hub::run(steve_hub::Assembly::builtin()))
}
