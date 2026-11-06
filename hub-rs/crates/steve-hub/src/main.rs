//! `steve-hub`: one HTTP server — /mcp for agent tools, /auth and /api for the web app, static files for the rest.
//! The whole of it is [`steve_hub::run`] on the built-in extensions.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    steve_hub::run(steve_hub::Assembly::builtin()).await
}
