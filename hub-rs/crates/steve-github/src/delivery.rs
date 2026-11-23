//! GitHub's read-only proof of the accepted PR ranges on current default branches.

use std::collections::{BTreeMap, BTreeSet};

use futures::{StreamExt, stream};

use axum::extract::Path;
use axum::{Extension, Json};
use serde::Deserialize;
use steve_modules::vault::{VaultError, VaultResult};
use steve_modules::{ApiError, Body, Caller};
use steve_store::github::{PrRow, RepoRow};
use steve_store::{Read, StoreError, UserRows, item_delivery};
use steve_types::{DeliveryBaseline, DeliveryRef, ItemId};

use crate::GitHub;
use crate::api::GhConn;

#[derive(Deserialize)]
pub(crate) struct Declaration {
    cycle: i64,
    revision: i64,
    pulls: Vec<String>,
}

pub(crate) async fn declare(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Path(item): Path<String>,
    Body(body): Body<Declaration>,
) -> VaultResult<Json<Vec<DeliveryRef>>> {
    if body.pulls.len() > 32 || body.pulls.iter().collect::<BTreeSet<_>>().len() != body.pulls.len()
    {
        return Err(ApiError::invalid("pulls").into());
    }
    let (scope, sources) = gh.store().read(|r| {
        if UserRows(r)
            .role(&who.me)
            .map_err(ApiError::from)?
            .as_deref()
            != Some("owner")
        {
            return Err(ApiError::forbidden("only the owner can declare item delivery").into());
        }
        if steve_store::item_repo::item_brief(r, &who.ws, &item)
            .map_err(ApiError::from)?
            .is_none()
        {
            return Err(ApiError::not_found("item").into());
        }
        let mut sources = Vec::with_capacity(body.pulls.len());
        for id in &body.pulls {
            let pr = steve_store::github::pr_get(r, id)
                .map_err(ApiError::from)?
                .filter(|pr| pr.workspace_id == who.ws)
                .ok_or_else(|| ApiError::not_found("PR"))?;
            let repo = repository(r, &who.ws, &pr.repo_id)
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::not_found("repository"))?;
            sources.push((repo, pr));
        }
        Ok::<_, VaultError>((
            item_delivery::scope(r, &item).map_err(ApiError::from)?,
            sources,
        ))
    })?;
    let mut refs = Vec::with_capacity(sources.len());
    for (repo, pr) in &sources {
        let frozen = gh
            .freeze(repo, pr)
            .await
            .ok_or_else(|| ApiError::conflict("cannot verify the pull request range"))?;
        refs.push(frozen);
    }
    refs.sort_by(|a, b| (&a.repository, a.number).cmp(&(&b.repository, b.number)));
    let pinned = DeliveryBaseline {
        item: ItemId::from(item),
        cycle: body.cycle,
        revision: body.revision,
        scope,
        refs,
    };
    gh.store().write(|w| {
        if UserRows(w)
            .role(&who.me)
            .map_err(ApiError::from)?
            .as_deref()
            != Some("owner")
        {
            return Err(ApiError::forbidden("only the owner can declare item delivery").into());
        }
        for (repo, pr) in &sources {
            let still_authorized = repository(w, &who.ws, &repo.id)
                .map_err(ApiError::from)?
                .is_some_and(|r| r.connection_id == repo.connection_id && r.gh_id == repo.gh_id);
            let same_head = steve_store::github::pr_get(w, &pr.id)
                .map_err(ApiError::from)?
                .is_some_and(|row| row.head_sha == pr.head_sha && row.repo_id == pr.repo_id);
            if !still_authorized || !same_head {
                return Err(ApiError::conflict("pull request changed").into());
            }
        }
        item_delivery::declare(w, &who.ws, &who.me, &pinned).map_err(ApiError::from)?;
        Ok::<_, VaultError>(())
    })?;
    Ok(Json(pinned.refs))
}

fn repository(r: &mut impl Read, ws: &str, id: &str) -> Result<Option<RepoRow>, StoreError> {
    let Some(repo) =
        steve_store::github::repo_get(r, id)?.filter(|r| r.workspace_id == ws && r.enabled)
    else {
        return Ok(None);
    };
    let allowed = steve_store::Connections(r)
        .get(&repo.connection_id)?
        .is_some_and(|c| {
            c.workspace_id == ws
                && matches!(c.status.as_str(), "ok" | "error")
                && c.caps.iter().any(|cap| cap == "repo.read")
        });
    Ok(allowed.then_some(repo))
}

#[derive(Deserialize)]
struct Pull {
    head: Ref,
    base: Ref,
    merged: bool,
    merge_commit_sha: Option<String>,
}

#[derive(Deserialize)]
struct Ref {
    sha: String,
    #[serde(rename = "ref")]
    branch: String,
    repo: Repo,
}

#[derive(Deserialize)]
struct Repo {
    id: i64,
    default_branch: String,
}

#[derive(Deserialize)]
struct Compare {
    status: String,
    merge_base_commit: Commit,
}

#[derive(Deserialize)]
struct Commit {
    sha: String,
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
struct Entry {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

#[derive(Deserialize)]
struct Tree {
    truncated: bool,
    tree: Vec<Entry>,
}

async fn read<T: serde::de::DeserializeOwned>(conn: &GhConn, path: &str) -> Option<T> {
    serde_json::from_value(conn.get(path).await.ok()?).ok()
}

fn oid(v: &str) -> bool {
    matches!(v.len(), 40 | 64)
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

async fn pull(conn: &GhConn, path: &str, n: i64) -> Option<Pull> {
    read(conn, &format!("{path}/pulls/{n}")).await
}

async fn compare(conn: &GhConn, path: &str, base: &str, head: &str) -> Option<Compare> {
    if !oid(base) || !oid(head) {
        return None;
    }
    read(conn, &format!("{path}/compare/{base}...{head}?per_page=1")).await
}

async fn tree(conn: &GhConn, path: &str, sha: &str) -> Option<BTreeMap<String, Entry>> {
    if !oid(sha) {
        return None;
    }
    #[derive(Deserialize)]
    struct GitCommit {
        tree: Commit,
    }
    let commit: GitCommit = read(conn, &format!("{path}/git/commits/{sha}")).await?;
    if !oid(&commit.tree.sha) {
        return None;
    }
    let tree: Tree = read(
        conn,
        &format!("{path}/git/trees/{}?recursive=1", commit.tree.sha),
    )
    .await?;
    if tree.truncated {
        return None;
    }
    let mut entries = BTreeMap::new();
    for e in tree.tree.into_iter().filter(|e| e.kind != "tree") {
        if !oid(&e.sha) || entries.insert(e.path.clone(), e).is_some() {
            return None;
        }
    }
    Some(entries)
}

/// Exact objects and mode on every changed path. A later change to the same path requires fresh
/// acceptance when exact equivalence cannot be proved.
fn present(
    base: &BTreeMap<String, Entry>,
    head: &BTreeMap<String, Entry>,
    current: &BTreeMap<String, Entry>,
) -> bool {
    let mut changed = false;
    for path in base.keys().chain(head.keys()) {
        let accepted = head.get(path);
        if base.get(path) != accepted {
            if current.get(path) != accepted {
                return false;
            }
            changed = true;
        }
    }
    changed
}

impl GitHub {
    async fn freeze(&self, repo: &RepoRow, pr: &PrRow) -> Option<DeliveryRef> {
        let conn = self.conn(&repo.workspace_id, &repo.connection_id).ok()?;
        let path = format!("/repos/{}", repo.full_name);
        let live = pull(&conn, &path, pr.number).await?;
        if live.base.repo.id != repo.gh_id
            || live.base.repo.default_branch != live.base.branch
            || live.head.sha != pr.head_sha
        {
            return None;
        }
        let comparison = compare(&conn, &path, &live.base.sha, &live.head.sha).await?;
        let base = if comparison.merge_base_commit.sha == live.head.sha && live.merged {
            // The head is already ancestral to the default branch. Use the parent of the
            // first PR commit, as GitHub recorded the range, instead of an empty merge base.
            let commits = conn
                .get_all_complete(
                    &format!("{path}/pulls/{}/commits?per_page=100", pr.number),
                    None,
                )
                .await
                .ok()?;
            if commits.len() >= 250 || commits.last()?["sha"].as_str()? != live.head.sha {
                return None;
            }
            commits.first()?["parents"][0]["sha"].as_str()?.to_owned()
        } else {
            comparison.merge_base_commit.sha
        };
        if !oid(&base) || base == live.head.sha {
            return None;
        }
        Some(DeliveryRef {
            repository: repo.id.clone(),
            number: pr.number,
            target: live.base.branch,
            head: live.head.sha,
            base: Some(base),
            required: true,
        })
    }

    async fn verified(&self, workspace: &str, baseline: &DeliveryBaseline) -> Option<()> {
        let mut targets = BTreeMap::new();
        for expected in &baseline.refs {
            let repo = self
                .store()
                .read(|r| repository(r, workspace, &expected.repository))
                .ok()??;
            let conn = self.conn(workspace, &repo.connection_id).ok()?;
            let path = format!("/repos/{}", repo.full_name);
            let live = pull(&conn, &path, expected.number).await?;
            if !live.merged
                || live.base.repo.id != repo.gh_id
                || live.base.branch != expected.target
                || live.head.sha != expected.head
                || live.base.repo.default_branch != expected.target
            {
                return None;
            }
            let accepted_base = expected.base.as_deref()?;
            if !targets.contains_key(&expected.repository) {
                let branch: String =
                    url::form_urlencoded::byte_serialize(expected.target.as_bytes()).collect();
                #[derive(Deserialize)]
                struct Target {
                    object: Commit,
                }
                let tip: Target = read(&conn, &format!("{path}/git/ref/heads/{branch}")).await?;
                let current = tree(&conn, &path, &tip.object.sha).await?;
                targets.insert(expected.repository.clone(), (tip.object.sha, current));
            }
            let (tip, current) = targets.get(&expected.repository)?;
            let landing = compare(&conn, &path, live.merge_commit_sha.as_deref()?, tip).await?;
            if !matches!(landing.status.as_str(), "ahead" | "identical") {
                return None;
            }
            let (base, head) = tokio::join!(
                tree(&conn, &path, accepted_base),
                tree(&conn, &path, &expected.head),
            );
            if !present(&base?, &head?, current) {
                return None;
            }
        }
        Some(())
    }

    pub(crate) async fn verify_deliveries(&self) {
        let pending = match self.store().read(|r| item_delivery::pending(r)) {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "reading pending item deliveries");
                return;
            }
        };
        stream::iter(pending)
            .map(|(workspace, baseline)| async move {
                let verified = tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    self.verified(&workspace, &baseline),
                )
                .await
                .is_ok_and(|result| result.is_some());
                metrics::counter!("steve_item_automatic_delivery_total",
                    "result" => if verified { "verified" } else { "unverified" })
                .increment(1);
                if verified && let Err(error) = self.core().delivery_verified(baseline).await {
                    tracing::warn!(%error, "recording verified item delivery");
                }
            })
            .buffer_unordered(4)
            .for_each(|()| async {})
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(path)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn commit(repo: &Path, message: &str) -> String {
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", message]);
        git(repo, &["rev-parse", "HEAD"])
    }

    fn entries(repo: &Path, commit: &str) -> BTreeMap<String, Entry> {
        git(repo, &["ls-tree", "-r", commit])
            .lines()
            .map(|line| {
                let (header, path) = line.split_once('\t').unwrap();
                let mut parts = header.split_whitespace();
                let entry = Entry {
                    path: path.to_string(),
                    mode: parts.next().unwrap().into(),
                    kind: parts.next().unwrap().into(),
                    sha: parts.next().unwrap().into(),
                };
                (path.into(), entry)
            })
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn content_proof_across_merges_and_follow_up_changes() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["config", "user.name", "Fixture"]);
        git(repo, &["config", "user.email", "fixture@example.test"]);
        std::fs::write(repo.join("old"), "rename me").unwrap();
        let base = commit(repo, "base");
        git(repo, &["switch", "-qc", "one"]);
        std::fs::rename(repo.join("old"), repo.join("new")).unwrap();
        std::fs::write(repo.join("binary"), [0, 255, 1]).unwrap();
        std::fs::write(repo.join("script"), "run").unwrap();
        let head = commit(repo, "first");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(repo.join("script"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let executable = commit(repo, "mode");
        git(repo, &["switch", "-qc", "two", &base]);
        std::fs::write(repo.join("second"), "independent delivery").unwrap();
        let second = commit(repo, "second");
        git(repo, &["switch", "-q", "main"]);
        git(repo, &["merge", "--no-ff", "-qm", "merge", "one"]);
        let merged = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["merge", "--squash", "-q", "two"]);
        let both = commit(repo, "squash");
        git(repo, &["switch", "-qc", "rebased", &base]);
        git(repo, &["cherry-pick", &second]);
        git(repo, &["rebase", &merged]);
        let rebased = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["switch", "-q", "main"]);
        std::fs::write(repo.join("unrelated"), "unrelated").unwrap();
        let newer = commit(repo, "unrelated edit");
        std::fs::write(repo.join("binary"), [0, 255, 2]).unwrap();
        let edited = commit(repo, "binary changed");
        git(repo, &["revert", "--no-edit", &edited]);
        git(repo, &["revert", "--no-edit", &both]);
        let reverted = git(repo, &["rev-parse", "HEAD"]);
        let base = entries(repo, &base);
        for (accepted, target, expected) in [
            (&executable, &merged, true),
            (&second, &both, true),
            (&executable, &both, true),
            (&second, &merged, false),
            (&second, &rebased, true),
            (&executable, &rebased, true),
            (&second, &newer, true),
            (&executable, &newer, true),
            (&head, &newer, false),
            (&executable, &edited, false),
            (&second, &edited, true),
            (&second, &reverted, false),
            (&executable, &reverted, true),
        ] {
            assert_eq!(
                present(&base, &entries(repo, accepted), &entries(repo, target)),
                expected
            );
        }
        assert!(!present(&base, &base, &base));
    }
}
