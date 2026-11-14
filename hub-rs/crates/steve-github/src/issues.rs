//! Issue → item: an item from the issue's title
//! and body (`source:["GitHub","sig"]`, project = the repo's project), linked
//! `links(item, 'from', 'gh_issue', ghi)`, and `gh_issues.item_id` set so the issue list shows it.
//! An issue whose item still exists answers that item instead of making a second one.

use axum::body::Bytes;
use axum::extract::Path;
use axum::routing::post;
use axum::{Extension, Json, Router};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_flow::ItemRequest;
use steve_modules::vault::{VaultError, VaultResult};
use steve_modules::{ApiError, Caller, Modules, parse};
use steve_store::base::links::{self, NewLink};
use steve_store::base::{self, ObjRef};

use crate::GitHub;
use crate::repo::{self, IssueRow, RepoRow};

/// `links.object_type` of an issue.
pub const ISSUE: &str = "gh_issue";
/// The source written on the links this module makes.
pub const LINK_SOURCE: &str = "github";
/// How much of the issue body goes into the goal.
const BODY_MAX: usize = 4000;
const TITLE_MAX: usize = 80;

/// `POST /github/issues/:id/to-item`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToItem {
    pub bot: Option<String>,
    /// `start` (default) | `propose`.
    pub mode: Option<String>,
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// A bot of `ws` that is not deleted, by id.
fn bot_in(c: &rusqlite::Connection, ws: &str, bot: &str) -> rusqlite::Result<bool> {
    c.prepare_cached(
        "SELECT 1 FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id AND w.workspace_id = ?
          WHERE b.id = ? AND b.deleted = 0",
    )?
    .query_row(params![ws, bot], |_| Ok(()))
    .optional()
    .map(|x| x.is_some())
}

fn main_bot(c: &rusqlite::Connection) -> rusqlite::Result<Option<String>> {
    c.prepare_cached("SELECT id FROM bots WHERE main = 1 AND deleted = 0 ORDER BY updated LIMIT 1")?
        .query_row([], |r| r.get(0))
        .optional()
}

/// The goal: where it comes from, the body (cut), the link.
pub fn goal_of(r: &RepoRow, i: &IssueRow, body: &str) -> String {
    let head = format!(
        "GitHub Issue {}#{}：{}",
        r.full_name,
        i.number,
        i.title.trim()
    );
    let body = body.trim();
    if body.is_empty() {
        format!("{head}\n\n{}", i.url)
    } else {
        format!("{head}\n\n{}\n\n{}", cut(body, BODY_MAX), i.url)
    }
}

enum Prep {
    Existing(String),
    Make(Box<(RepoRow, IssueRow, String)>),
}

impl GitHub {
    /// “转成事项” for an issue. `404 没有这个 Issue` / `404 没有这个 Bot`; the body is read from GitHub
    /// (only the title when GitHub can't be reached).
    pub async fn issue_to_item(&self, who: &Caller, id: &str, p: ToItem) -> VaultResult<String> {
        let propose = match p.mode.as_deref() {
            None | Some("start") => false,
            Some("propose") => true,
            Some(_) => return Err(ApiError::invalid("mode").into()),
        };
        let ws = who.ws.clone();
        let prep = self.store().reader(|c| -> Result<Prep, ApiError> {
            let i = repo::issue_get(c, id)?
                .filter(|i| i.workspace_id == ws)
                .ok_or_else(|| ApiError::not_found("Issue"))?;
            if let Some(item) = &i.item_id
                && repo::item_brief(c, &ws, item)?.is_some()
            {
                return Ok(Prep::Existing(item.clone()));
            }
            let r = repo::repo_get(c, &i.repo_id)?
                .filter(|r| r.workspace_id == ws)
                .ok_or_else(|| ApiError::not_found("Issue"))?;
            let bot = match &p.bot {
                Some(b) if bot_in(c, &ws, b)? => b.clone(),
                Some(_) => return Err(ApiError::not_found("Bot")),
                None => main_bot(c)?
                    .ok_or_else(|| ApiError::rule(steve_i18n::t!("github.issue.no_main_bot")))?,
            };
            Ok(Prep::Make(Box::new((r, i, bot))))
        })?;
        let (r, i, bot) = match prep {
            Prep::Existing(item) => return Ok(item),
            Prep::Make(x) => *x,
        };
        let body = match self.conn(&ws, &r.connection_id) {
            Ok(conn) => match conn
                .get(&format!("/repos/{}/issues/{}", r.full_name, i.number))
                .await
            {
                Ok(v) => v["body"].as_str().unwrap_or("").to_string(),
                Err(e) => {
                    tracing::warn!(issue = %i.id, error = %e, "issue body not read");
                    String::new()
                }
            },
            Err(_) => String::new(),
        };
        let flow = self
            .flow()
            .ok_or_else(|| ApiError::rule(steve_i18n::t!("github.issue.not_wired")))?;
        let item = flow
            .core
            .create_item(ItemRequest {
                requester: None,
                ws: ws.clone(),
                title: cut(i.title.trim(), TITLE_MAX),
                // the issue is somebody else's text: the agent reads it as data
                goal: steve_modules::outbound::quote_untrusted(&goal_of(&r, &i, &body)),
                bot,
                project: r.project_id.clone(),
                tag_ids: Vec::new(),
                source: ("GitHub".into(), "sig".into()),
                propose,
                accept_auto: false,
                run_id: String::new(),
            })
            .await
            .map_err(|e| ApiError::rule(steve_i18n::t!("github.issue.create_failed", error = e)))?;
        let now = self.now();
        self.store().transaction(|tx| -> Result<(), VaultError> {
            links::add(
                tx,
                &ws,
                &NewLink::new(
                    ObjRef::new("item", &item),
                    "from",
                    ObjRef::new(ISSUE, &i.id),
                    LINK_SOURCE,
                ),
            )
            .map_err(ApiError::from)?;
            repo::set_issue_item(tx, &i.id, Some(&item), now).map_err(ApiError::from)?;
            base::audit(
                tx,
                &ws,
                &who.me,
                "github.issue_to_item",
                &i.id,
                &json!({"item": item, "repo": r.full_name, "number": i.number}),
            )
            .map_err(ApiError::from)?;
            Ok(())
        })?;
        self.notifier().gh(&ws, &i.id);
        tracing::info!(issue = %i.id, item = %item, "github issue turned into an item");
        Ok(item)
    }
}

/// `POST /github/issues/{id}/to-item`, relative to `/api`, behind the `Caller` layer.
pub fn routes(gh: GitHub) -> Router<Modules> {
    Router::new()
        .route("/github/issues/{id}/to-item", post(to_item))
        .layer(Extension(gh))
}

async fn to_item(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Path(id): Path<String>,
    body: Bytes,
) -> VaultResult<Json<Value>> {
    // `{bot?, mode?}`; an empty body is `{}`.
    let p: ToItem = if body.iter().all(u8::is_ascii_whitespace) {
        ToItem::default()
    } else {
        parse(&body)?
    };
    let item = gh.issue_to_item(&who, &id, p).await?;
    Ok(Json(json!({ "item_id": item })))
}
