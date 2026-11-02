//! GitHub as an S1 event source, the module SSE notices, the Feishu directory events, the watcher
//! that follows connections coming and going, and the unauthenticated `POST /hooks/:connection_id`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Router};
use serde_json::json;
use steve_flow::{
    AdapterError, Batch, EventAdapter, EventTypeDef, FlowCx, IngestMode, PushRequest, RawEvent,
    ScopeDef,
};
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ConnCx, ConnectionProvider, FieldDef, Health, Minted, OAuthClient,
    OAuthSpec, ProviderError, StartCx, TokenKind, TokenSet,
};
use steve_modules::contacts::sync::DirectorySync;

use crate::bus::Bus;

// ---------------------------------------------------------------- GitHub (G2)

/// The eight event types of GitHub signals (github.md §5.2), for the source page.
pub const GITHUB_TYPES: &[EventTypeDef] = &[
    EventTypeDef::new(
        "github.check_failed",
        "检查失败",
        "PR 头或默认分支的检查没有通过",
    ),
    EventTypeDef::new("github.check_passed", "检查通过", "PR 的检查全部通过"),
    EventTypeDef::new("github.review_requested", "请你评审", "有人请你评审 PR"),
    EventTypeDef::new(
        "github.review",
        "收到评审",
        "别人对 Steve 开的或关联事项的 PR 提交了评审",
    ),
    EventTypeDef::new("github.pr_comment", "PR 评论", "关联事项的 PR 上有新评论"),
    EventTypeDef::new("github.pr_merged", "PR 已合并", "PR 合并了"),
    EventTypeDef::new("github.pr_closed", "PR 已关闭", "PR 没有合并就关闭了"),
    EventTypeDef::new("github.issue_opened", "新 Issue", "仓库里开了新 Issue"),
];

/// A GitHub connection provider as an S1 event source: the same provider (all connection calls
/// delegate to it) plus the `github` source and its types. Events come from G2 through
/// [`GhSink`], never from S1's poller or `parse_push`: `Push` mode keeps the poller away, and PAT
/// repository hooks are handed to G2 before S1 sees them.
pub struct GithubSource(pub Arc<dyn ConnectionProvider>);

#[async_trait]
impl ConnectionProvider for GithubSource {
    fn id(&self) -> &str {
        self.0.id()
    }
    fn system(&self) -> &str {
        self.0.system()
    }
    fn name(&self) -> &str {
        self.0.name()
    }
    fn caps(&self) -> &[&'static str] {
        self.0.caps()
    }
    fn auth(&self) -> AuthKind {
        self.0.auth()
    }
    fn oauth(&self) -> Option<&OAuthSpec> {
        self.0.oauth()
    }
    fn fields(&self) -> &[FieldDef] {
        self.0.fields()
    }
    fn token_kind(&self) -> TokenKind {
        self.0.token_kind()
    }
    fn available_caps(&self, granted_scopes: &[String]) -> Vec<String> {
        self.0.available_caps(granted_scopes)
    }
    fn reauth_question(&self, account: &str) -> String {
        self.0.reauth_question(account)
    }
    async fn oauth_client(&self, cx: &StartCx<'_>) -> Result<Option<OAuthClient>, ProviderError> {
        self.0.oauth_client(cx).await
    }
    async fn refresh_tokens(
        &self,
        http: &reqwest::Client,
        client: &OAuthClient,
        refresh_token: &str,
    ) -> Result<TokenSet, ProviderError> {
        self.0.refresh_tokens(http, client, refresh_token).await
    }
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        self.0.verify(cx).await
    }
    async fn health(&self, cx: &ConnCx) -> Health {
        self.0.health(cx).await
    }
    async fn mint(&self, cx: &ConnCx) -> Result<Minted, ProviderError> {
        self.0.mint(cx).await
    }
    async fn revoke(&self, cx: &ConnCx) -> Result<(), ProviderError> {
        self.0.revoke(cx).await
    }
}

#[async_trait]
impl EventAdapter for GithubSource {
    fn source(&self) -> &'static str {
        "github"
    }
    fn event_types(&self) -> &'static [EventTypeDef] {
        GITHUB_TYPES
    }
    async fn scopes(&self, _: &ConnCx) -> Result<Vec<ScopeDef>, AdapterError> {
        Ok(Vec::new())
    }
    fn mode(&self) -> IngestMode {
        IngestMode::Push
    }
    async fn poll(&self, _: &ConnCx, _: Option<&str>) -> Result<Batch, AdapterError> {
        Ok(Batch::default())
    }
    async fn parse_push(&self, _: &ConnCx, _: &PushRequest) -> Result<Vec<RawEvent>, AdapterError> {
        // G2 verifies and parses GitHub deliveries itself
        Err(AdapterError::BadSignature)
    }
}

/// `{type, id, ws}` notices of the modules that have no `ChangeEvent` variant: they are scoped to
/// their workspace by the bus (`sse::MODULE_TYPES`).
#[derive(Clone)]
pub struct Notices {
    pub bus: Bus,
}

impl Notices {
    fn send(&self, ty: &str, ws: &str, id: &str) {
        self.bus
            .publish_value(json!({ "type": ty, "id": id, "ws": ws }));
    }
}

impl steve_github::events::GhNotifier for Notices {
    fn gh(&self, ws: &str, id: &str) {
        self.send("gh", ws, id);
    }
}

impl steve_mail::MailNotify for Notices {
    fn thread(&self, ws: &str, thread_id: &str) {
        self.send("mail", ws, thread_id);
    }
}

// ---------------------------------------------------------------- Feishu directory (F5)

/// Feishu address book events of the bot's long connection → one debounced directory sync.
pub struct DirectoryEvents(pub Arc<DirectorySync>);

impl steve_channels::OtherEvents for DirectoryEvents {
    fn on_event(&self, ws: &str, connection_id: &str, envelope: &serde_json::Value) -> bool {
        steve_channels::adapters::feishu::directory::on_event(&self.0, ws, connection_id, envelope)
    }
}

// ---------------------------------------------------------------- connections coming and going

/// The `connections` rows a watcher acts on: `(id, ws, adapter, status, created_by)`.
fn connections(flow: &FlowCx) -> Vec<(String, String, String, String, String)> {
    flow.store()
        .reader(|c| -> steve_store::Result<Vec<_>> {
            let mut st = c.prepare(
                "SELECT id, workspace_id, adapter, status, created_by FROM connections ORDER BY created, id",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap_or_default()
}

/// Mail follows its connections at once (E6 `Mail::on_event`), and a new mailbox gets the
/// “邮件分诊” template (E4; idempotent). Feishu's connections are
/// followed by capi's `FeishuRuntime`.
pub async fn watch_mailboxes(
    mut changes: tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    flow: Arc<FlowCx>,
    mail: steve_mail::Mail,
) {
    let live = |s: &str| matches!(s, "ok" | "error");
    // what existed at boot is not “new”
    let mut known: std::collections::BTreeSet<String> =
        connections(&flow).into_iter().map(|c| c.0).collect();
    while let Some(v) = changes.recv().await {
        // E6: a mailbox that appears, recovers or goes away starts or stops its runner now (the
        // first sync of a new mailbox runs at once)
        let field = |k: &str| {
            v.get(k)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        mail.on_event(&steve_store::ChangeEvent::Connection {
            id: field("id"),
            ws: field("ws"),
        });
        while changes.try_recv().is_ok() {}
        for (id, ws, adapter, status, by) in connections(&flow) {
            if !known.insert(id.clone()) || !live(&status) {
                continue;
            }
            if matches!(adapter.as_str(), "imap" | "gmail" | "outlook") {
                let (f, cid) = (flow.clone(), id.clone());
                let r = tokio::task::spawn_blocking(move || {
                    steve_mail::template::install(&f, &ws, &cid, &by)
                })
                .await;
                if let Ok(Err(e)) = r {
                    tracing::warn!(connection = %id, "mail triage template: {e}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------- POST /hooks/:connection_id

/// Deliveries per connection per minute (signals-automation.md §8).
const HOOKS_PER_MINUTE: usize = 120;

/// What the hooks route needs: S1, G2, and a per-connection rate limit.
#[derive(Clone)]
pub struct Hooks {
    pub flow: Arc<FlowCx>,
    pub gh: steve_github::GitHub,
    recent: Arc<Mutex<HashMap<String, Vec<i64>>>>,
}

impl Hooks {
    pub fn new(flow: Arc<FlowCx>, gh: steve_github::GitHub) -> Self {
        Self {
            flow,
            gh,
            recent: Arc::default(),
        }
    }

    fn allow(&self, conn: &str, now: i64) -> bool {
        let mut m = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let v = m.entry(conn.to_string()).or_default();
        v.retain(|t| now - t < 60_000);
        if v.len() >= HOOKS_PER_MINUTE {
            return false;
        }
        v.push(now);
        true
    }
}

fn empty(code: StatusCode) -> Response {
    code.into_response()
}

/// `POST /hooks/:connection_id`, at the root and without a session: `github_pat` connections go to
/// G2 (same rules as `/hooks/github`); every other push connection to S1's adapter: body ≤1 MiB
/// (`413`), ≤120 a minute (`429`), signature checked before anything is written (`401`, empty
/// body), then `202`. Unknown connections are `401` like a bad signature.
pub fn routes<S: Clone + Send + Sync + 'static>(h: Hooks) -> Router<S> {
    Router::new()
        .route("/hooks/{id}", post(hook))
        .layer(Extension(h.gh.clone()))
        .with_state(h)
}

async fn hook(State(h): State<Hooks>, Path(id): Path<String>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let store = h.flow.store().clone();
    let cid = id.clone();
    let conn = tokio::task::spawn_blocking(move || {
        store.reader(|c| -> steve_store::Result<Option<(String, String)>> {
            use rusqlite::OptionalExtension;
            Ok(c.query_row(
                "SELECT workspace_id, adapter FROM connections WHERE id = ?",
                [&cid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
        })
    })
    .await
    .ok()
    .and_then(Result::ok)
    .flatten();
    let Some((ws, adapter)) = conn else {
        return empty(StatusCode::UNAUTHORIZED);
    };
    if steve_github::hooks::handles(&adapter) {
        return steve_github::hooks::connection_hook(
            Extension(h.gh.clone()),
            Path(id),
            parts.headers,
            body,
        )
        .await;
    }
    let headers: HeaderMap = parts.headers;
    let bytes = match steve_github::hooks::read_body(&headers, Body::new(body)).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    let push = PushRequest {
        headers,
        body: bytes,
    };
    // S1's `push` verifies and ingests in one step, so the limit is counted before it: a flood is
    // cut off before any signature work or write
    if !h.allow(&id, h.flow.now()) {
        return empty(StatusCode::TOO_MANY_REQUESTS);
    }
    match steve_flow::ingest::push(&h.flow, &ws, &id, &push).await {
        Ok(_) => empty(StatusCode::ACCEPTED),
        Err(AdapterError::BadSignature) => empty(StatusCode::UNAUTHORIZED),
        Err(e) => {
            tracing::warn!(connection = %id, "hook: {e}");
            empty(StatusCode::BAD_REQUEST)
        }
    }
}
