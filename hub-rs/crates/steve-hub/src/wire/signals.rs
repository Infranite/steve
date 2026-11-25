//! GitHub as an S1 event source, Feishu directory events, the watcher
//! that follows connections coming and going, and the unauthenticated `POST /hooks/:connection_id`.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Router};
use steve_flow::{
    AdapterError, Batch, EventAdapter, EventTypeDef, FlowCx, IngestMode, PushRequest, RawEvent,
    ScopeDef,
};
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ConnCx, ConnectionProvider, FieldDef, Health, Minted, OAuthClient,
    OAuthSpec, ProviderError, StartCx, TokenKind, TokenSet,
};
use steve_modules::contacts::sync::DirectorySync;

// ---------------------------------------------------------------- GitHub (G2)

/// The event types of GitHub signals (github.md §5.2), for the source page.
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
    EventTypeDef::new(
        steve_github::automation::PR_TYPE,
        "PR 评审",
        "仓库启用自动评审并收到待评审提交",
    ),
    EventTypeDef::new(
        steve_github::automation::ISSUE_TYPE,
        "Issue 初筛",
        "仓库启用自动初筛并收到新 Issue",
    ),
];

/// The labels and descriptions of [`GITHUB_TYPES`] in every language (zh-CN: as declared there).
const GITHUB_TYPE_KEYS: &[&str] = &[
    "hub.github_type.check_failed",
    "hub.github_type.check_failed_desc",
    "hub.github_type.check_passed",
    "hub.github_type.check_passed_desc",
    "hub.github_type.review_requested",
    "hub.github_type.review_requested_desc",
    "hub.github_type.review",
    "hub.github_type.review_desc",
    "hub.github_type.pr_comment",
    "hub.github_type.pr_comment_desc",
    "hub.github_type.pr_merged",
    "hub.github_type.pr_merged_desc",
    "hub.github_type.pr_closed",
    "hub.github_type.pr_closed_desc",
    "hub.github_type.issue_opened",
    "hub.github_type.issue_opened_desc",
    "hub.github_type.pr_review",
    "hub.github_type.pr_review_desc",
    "hub.github_type.issue_triage",
    "hub.github_type.issue_triage_desc",
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
    fn localize(&self, text: &str) -> Option<String> {
        self.0
            .localize(text)
            .or_else(|| steve_i18n::reword!(text, GITHUB_TYPE_KEYS))
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

// ---------------------------------------------------------------- Feishu directory (F5)

/// Feishu address book events of the bot's long connection → one debounced directory sync.
pub struct DirectoryEvents(pub Arc<DirectorySync>);

impl steve_channels::OtherEvents for DirectoryEvents {
    fn on_event(&self, ws: &str, connection_id: &str, envelope: &serde_json::Value) -> bool {
        steve_channels::adapters::feishu::directory::on_event(&self.0, ws, connection_id, envelope)
    }
}

// ---------------------------------------------------------------- connections coming and going

fn connections(flow: &FlowCx) -> Vec<steve_store::connections::WatchedConnection> {
    flow.store()
        .read(|r| steve_store::connections::watched(r))
        .unwrap_or_default()
}

/// Mail follows its connections at once (E6 `Mail::on_event`), and a new mailbox gets the
/// “邮件分诊” template (E4; idempotent). Channel connections are followed by the connection
/// runtime (`steve_channels::lifecycle`).
pub async fn watch_mailboxes(
    mut changes: tokio::sync::mpsc::Receiver<steve_types::Notification>,
    flow: Arc<FlowCx>,
    mail: steve_mail::Mail,
) {
    let live = |s: &str| matches!(s, "ok" | "error");
    // what existed at boot is not “new”
    let mut known: std::collections::BTreeSet<String> =
        connections(&flow).into_iter().map(|c| c.id).collect();
    while changes.recv().await.is_some() {
        steve_runtime::round("mail.mailboxes", async {
            // E6: a mailbox that appears, recovers or goes away starts or stops its runner now (the
            // first sync of a new mailbox runs at once)
            if let Err(err) = mail.reconcile() {
                tracing::warn!(error = %err, "mail runners");
            }
            while changes.try_recv().is_ok() {}
            for conn in connections(&flow) {
                let steve_store::connections::WatchedConnection {
                    id,
                    workspace_id: ws,
                    adapter,
                    status,
                    created_by: by,
                } = conn;
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
        })
        .await;
    }
}

// ---------------------------------------------------------------- POST /hooks/:connection_id

/// What the hooks route needs: S1, G2, and a per-connection rate limit.
#[derive(Clone)]
pub struct Hooks {
    pub flow: Arc<FlowCx>,
    pub gh: steve_github::GitHub,
    recent: Arc<steve_runtime::policy::Limiter<String>>,
}

impl Hooks {
    pub fn new(flow: Arc<FlowCx>, gh: steve_github::GitHub) -> Self {
        let recent = Arc::new(steve_github::hooks::hook_limiter(
            flow.modules.clock.clone(),
        ));
        Self { flow, gh, recent }
    }

    fn allow(&self, conn: &str) -> bool {
        self.recent.check(&conn.to_owned()).is_ok()
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
        store.read(|r| steve_store::connections::connection_kind(r, &cid))
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
    if !h.allow(&id) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use steve_modules::accounts::connections::shown;

    /// The source page shows the GitHub types and the adapter's name in the reader's language.
    #[test]
    fn github_types_by_language() {
        let src = GithubSource(Arc::new(steve_github::adapters::GitHubPat::default()));
        let texts = || {
            let mut out = vec![shown(&src, src.name())];
            for t in GITHUB_TYPES {
                out.push(shown(&src, t.label));
                out.push(shown(&src, t.desc));
            }
            out
        };
        let review = GITHUB_TYPES
            .iter()
            .position(|t| t.key == steve_github::automation::PR_TYPE)
            .unwrap();
        let triage = GITHUB_TYPES
            .iter()
            .position(|t| t.key == steve_github::automation::ISSUE_TYPE)
            .unwrap();
        let en = steve_i18n::with(steve_i18n::EN, texts);
        assert_eq!(en[0], "GitHub personal token");
        assert_eq!(
            en[1..3],
            [
                "Checks failed",
                "Checks on a PR head or the default branch did not pass"
            ]
        );
        assert_eq!(
            [
                &en[1 + review * 2],
                &en[2 + review * 2],
                &en[1 + triage * 2],
                &en[2 + triage * 2]
            ],
            [
                "PR review",
                "A repository opted into reviewing this PR revision",
                "Issue triage",
                "A repository opted into triaging this new issue",
            ]
        );
        assert!(
            en.iter()
                .all(|t| !t.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))),
            "{en:?}"
        );
        let zh = steve_i18n::with(steve_i18n::ZH_CN, texts);
        assert_eq!(zh[0], "GitHub 个人令牌");
        assert_eq!(zh[1..3], [GITHUB_TYPES[0].label, GITHUB_TYPES[0].desc]);
        assert_eq!(
            [
                &zh[1 + review * 2],
                &zh[2 + review * 2],
                &zh[1 + triage * 2],
                &zh[2 + triage * 2]
            ],
            [
                "PR 评审",
                "仓库启用自动评审并收到待评审提交",
                "Issue 初筛",
                "仓库启用自动初筛并收到新 Issue",
            ]
        );
    }
}

#[cfg(test)]
mod rate_tables {
    use super::*;
    #[test]
    fn hook_window_table_keeps_later_hits_and_rejections_do_not_count() {
        // Construct the same rate table without the unrelated flow/GitHub services.
        let clock = Arc::new(steve_modules::FakeClock::new(0));
        let recent = steve_github::hooks::hook_limiter(clock.clone());
        for (at, count, allowed) in [
            (0, 60, true),
            (30_000, 60, true),
            (59_999, 3, false),
            (60_000, 60, true),
            (60_000, 1, false),
            (89_999, 1, false),
            (90_000, 60, true),
        ] {
            clock.set(at);
            for _ in 0..count {
                assert_eq!(recent.check(&"a".to_owned()).is_ok(), allowed, "at {at}");
            }
        }
        assert!(recent.check(&"b".to_owned()).is_ok());
        clock.advance(60_000);
        assert!(recent.is_empty());
    }
}
