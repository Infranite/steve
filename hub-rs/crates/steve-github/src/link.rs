//! PRs and items: which item a PR belongs to, and what linking writes.
//!
//! Order, for a new PR or one that is not linked yet:
//! 1. a PR Steve opened is that item's (`steve`), set by [`crate::accept`];
//! 2. `head_ref` is an item's `branch` and the item is in the repo's project (`branch`);
//! 3. the title or body names an existing item of the workspace, `\b<prefix>-\d+\b` (`ref`);
//! 4. you link it by hand (`manual`), which overrides the rest; unlinking by hand sticks too
//!    (`item_id = NULL`, `link_source = manual`), so the next sync does not link it again.
//!
//! One PR, one item; an item can have many PRs. Linking writes `links(item, output, gh_pr, ghp)`,
//! the `PR` artifact the first time, and `steve sys 关联了 PR #<n>（<repo>）` on the timeline.

use axum::extract::Path;
use axum::routing::post;
use axum::{Extension, Json, Router};
use regex::Regex;
use serde_json::{Value, json};
use steve_modules::vault::VaultResult;
use steve_modules::{ApiError, Body, Caller, Modules};
use steve_store::base::links::{self, LinkKey, NewLink};
use steve_store::base::{self, ObjRef};
use steve_store::{ChangeEvent, Tx};

use crate::GitHub;
use crate::repo::{self, PrRow, RepoRow};
use crate::repos::pr_out;

/// `links.source` of the links this module writes.
pub const LINK_SOURCE: &str = "github";

/// The item an unlinked PR belongs to, by branch, else by a reference in its title or body.
pub fn match_item(
    c: &rusqlite::Connection,
    repo: &RepoRow,
    pr: &PrRow,
) -> rusqlite::Result<Option<(String, &'static str)>> {
    if let Some(project) = &repo.project_id
        && !pr.head_ref.is_empty()
        && let Some(item) = repo::item_for_branch(c, &repo.workspace_id, &pr.head_ref, project)?
    {
        return Ok(Some((item, "branch")));
    }
    let prefix = repo::item_prefix(c, &repo.workspace_id)?;
    let Ok(re) = Regex::new(&format!(r"\b{}-(\d{{1,9}})\b", regex::escape(&prefix))) else {
        return Ok(None);
    };
    for text in [&pr.title, &pr.body_excerpt] {
        for cap in re.captures_iter(text) {
            let id = format!("{prefix}-{}", &cap[1]);
            if repo::item_brief(c, &repo.workspace_id, &id)?.is_some() {
                return Ok(Some((id, "ref")));
            }
        }
    }
    Ok(None)
}

fn pr_ref(pr: &PrRow) -> ObjRef {
    ObjRef::new("gh_pr", &pr.id)
}

fn item_ref(item: &str) -> ObjRef {
    ObjRef::new("item", item)
}

/// What linking `pr` (already stored with its `item_id`) to `item` writes besides the row: the
/// `output` link, the artifact (once), the timeline line (`timeline`, unless `None`).
pub fn attach(
    tx: &Tx,
    repo: &RepoRow,
    pr: &PrRow,
    item: &str,
    timeline: Option<&str>,
    now: i64,
) -> Result<(), ApiError> {
    let ws = &repo.workspace_id;
    links::add(
        tx,
        ws,
        &NewLink::new(item_ref(item), "output", pr_ref(pr), LINK_SOURCE),
    )?;
    repo::add_artifact_once(
        tx,
        item,
        "PR",
        &format!("#{} {}", pr.number, pr.title),
        &pr.url,
        now,
    )?;
    if let Some(t) = timeline {
        repo::add_timeline(tx, item, t, now)?;
    }
    tx.publish(ChangeEvent::Item { id: item.into() });
    Ok(())
}

/// The timeline line of a link made by matching or by hand.
pub fn linked_line(repo: &RepoRow, pr: &PrRow) -> String {
    format!("关联了 PR #{}（{}）", pr.number, repo.full_name)
}

/// Removes the `output` link of `pr` from `item` (the artifact stays, like on unlinking a repo).
fn detach(tx: &Tx, ws: &str, pr: &PrRow, item: &str) -> Result<(), ApiError> {
    links::remove(
        tx,
        ws,
        &LinkKey {
            subject: item_ref(item),
            predicate: "output".into(),
            object: pr_ref(pr),
        },
    )?;
    tx.publish(ChangeEvent::Item { id: item.into() });
    Ok(())
}

impl GitHub {
    /// `POST /github/prs/:id/link {item_id: str|null}`: link by hand (`manual`), or unlink.
    pub fn link_pr(&self, who: &Caller, id: &str, item: Option<&str>) -> VaultResult<Value> {
        let ws = who.ws.clone();
        let now = self.now();
        let out = self.store().transaction(|tx| {
            let mut pr = repo::pr_get(tx, id)
                .map_err(ApiError::from)?
                .filter(|p| p.workspace_id == ws)
                .ok_or_else(|| ApiError::not_found("PR"))?;
            let r = repo::repo_get(tx, &pr.repo_id)
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::not_found("PR"))?;
            if let Some(i) = item
                && repo::item_brief(tx, &ws, i)
                    .map_err(ApiError::from)?
                    .is_none()
            {
                return Err(ApiError::not_found("事项").into());
            }
            let before = pr.item_id.clone();
            if before.as_deref() != item
                && let Some(old) = &before
            {
                detach(tx, &ws, &pr, old)?;
            }
            pr.item_id = item.map(String::from);
            pr.link_source = Some("manual".into());
            pr.updated = pr.updated.max(now);
            repo::upsert_pr(tx, &pr).map_err(ApiError::from)?;
            if let Some(i) = item
                && before.as_deref() != Some(i)
            {
                attach(tx, &r, &pr, i, Some(&linked_line(&r, &pr)), now)?;
            }
            base::audit(
                tx,
                &ws,
                &who.me,
                "github.pr_link",
                &pr.id,
                &json!({"item": item, "before": before}),
            )
            .map_err(ApiError::from)?;
            Ok::<_, steve_modules::vault::VaultError>(pr_out(tx, &r, &pr).map_err(ApiError::from)?)
        })?;
        self.notifier().gh(&ws, id);
        Ok(out)
    }
}

#[derive(serde::Deserialize)]
struct LinkBody {
    item_id: Option<String>,
}

/// `POST /github/prs/{id}/link`, relative to `/api`, behind the `Caller` layer.
pub fn routes(gh: GitHub) -> Router<Modules> {
    Router::new()
        .route("/github/prs/{id}/link", post(link))
        .layer(Extension(gh))
}

async fn link(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<LinkBody>,
) -> VaultResult<Json<Value>> {
    let mut c = steve_modules::Check::new();
    c.opt_len("item_id", b.item_id.as_deref(), 1, 100);
    c.done()?;
    Ok(Json(gh.link_pr(&who, &id, b.item_id.as_deref())?))
}
