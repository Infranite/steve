//! Serves the Steve tools on 127.0.0.1:<port> (default 18979) with a recording backend that
//! accepts the bearer token `tok` and prints every call. For checking real agents by hand.
use std::{net::SocketAddr, sync::Arc};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port = std::env::args().nth(1).unwrap_or_else(|| "18979".into());
    let app = steve_mcp::router(Arc::new(steve_mcp::testing::Recorder::default()));
    let l = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    eprintln!("listening on http://127.0.0.1:{port}/mcp");
    axum::serve(l, app.into_make_service_with_connect_info::<SocketAddr>()).await
}
