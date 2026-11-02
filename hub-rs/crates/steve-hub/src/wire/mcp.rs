//! steve-mcp's [`Backend`](steve_mcp::Backend): sessions by bearer token from steve-store; the five tools
//! are steve-core operations and answer an error until the core is wired.

use steve_mcp::{Check, Output, PlanStep, ToolError};
use steve_store::{Session, Store};

pub struct McpBackend {
    pub store: Store,
}

fn not_wired(tool: &str) -> Result<(), ToolError> {
    Err(format!("{tool}: steve-core is not wired into this Hub yet").into())
}

impl steve_mcp::Backend for McpBackend {
    type Session = Session;

    async fn session_by_token(&self, token: &str) -> Option<Session> {
        let (store, token) = (self.store.clone(), token.to_string());
        tokio::task::spawn_blocking(move || store.sessions().by_token(&token).ok().flatten())
            .await
            .ok()
            .flatten()
    }
    async fn update(
        &self,
        _s: &Session,
        _now: String,
        _plan: Vec<PlanStep>,
    ) -> Result<(), ToolError> {
        not_wired("steve_update")
    }
    async fn ask(
        &self,
        _s: &Session,
        _q: String,
        _options: Vec<String>,
        _hint: steve_mcp::AskHint,
    ) -> Result<(), ToolError> {
        not_wired("steve_ask")
    }
    async fn request_approval(
        &self,
        _s: &Session,
        _action: String,
        _detail: String,
    ) -> Result<(), ToolError> {
        not_wired("steve_request_approval")
    }
    async fn deliver(
        &self,
        _s: &Session,
        _summary: String,
        _outputs: Vec<Output>,
        _checks: Vec<Check>,
        _unfinished: Option<Vec<String>>,
    ) -> Result<(), ToolError> {
        not_wired("steve_deliver")
    }
    async fn publish(
        &self,
        _s: &Session,
        _branch: String,
        _summary: String,
    ) -> Result<(), ToolError> {
        not_wired("steve_publish")
    }
}
