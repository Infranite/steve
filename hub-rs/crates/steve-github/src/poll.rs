//! The polling fallback: repos without webhooks (`ingest=poll`), and App/hook
//! repos whose last delivery is over 6 hours old while they have an open PR linked to an item (the
//! public entry may be down). Every 2 minutes: updated PRs in bounded pages (first-page ETag), then the
//! check runs and statuses of open PRs whose checks have not passed; every 10 minutes the issues
//! updated since the last look.
//!
//! What the poll sees goes through the same handler as webhooks ([`GitHub::apply`]) under a
//! synthetic delivery id `poll:<repo>:<kind>:<id>:<updated>`, recorded in `gh_deliveries`, so the
//! same view is never applied twice and a change the webhook already applied produces no event.

use std::sync::Weak;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_modules::ApiError;

use crate::sync::{Source, checks_state};
use crate::{GitHub, Inner};
use steve_store::github::RepoRow;
use steve_store::github::{Deliveries, DeliveryRow, PollTarget, RECEIVED, Repositories};

pub const POLL_EVERY_MS: i64 = 2 * 60_000;
pub const ISSUES_EVERY_MS: i64 = 10 * 60_000;
/// No delivery for this long while an item waits on a PR: poll as well.
pub const STALE_MS: i64 = 6 * 3_600_000;
/// Deliveries are kept (for dedupe and the debug list) this long.
pub const DELIVERY_KEEP_MS: i64 = 7 * 24 * 3_600_000;
const TICK: Duration = Duration::from_secs(30);
/// Repos polled at the same time.
const POLL_CONCURRENCY: usize = 4;
/// Bound API work per repo; an unfinished scan continues on the next poll.
pub const PULL_PAGES_MAX: usize = 10;

/// What one poll did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PollReport {
    /// GitHub answered 304 for the PR list.
    pub pulls_unchanged: bool,
    /// The page budget was exhausted; the next URL was saved for the next poll.
    pub pulls_deferred: bool,
    /// Changes handed to the handler (new synthetic deliveries).
    pub applied: usize,
    /// Views already applied before (same synthetic id).
    pub seen: usize,
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

fn poll_is_due<C: steve_store::Read>(
    repositories: &mut Repositories<'_, C>,
    target: &PollTarget,
    now: i64,
) -> bool {
    let last = target.state["last_poll"].as_i64().unwrap_or(0);
    if now - last < POLL_EVERY_MS {
        return false;
    }
    if target.ingest == "poll" {
        return true;
    }
    let stale = target.last_delivery.is_none_or(|t| now - t > STALE_MS);
    stale && repositories.has_open_linked_pr(&target.id).unwrap_or(false)
}

impl GitHub {
    /// Should this repository be polled now?
    pub fn poll_is_due(&self, id: &str, now: i64) -> steve_store::Result<bool> {
        self.store().read(|r| {
            let mut repositories = Repositories::new(r);
            let target = repositories.poll_target(id)?;
            Ok(target.is_some_and(|target| poll_is_due(&mut repositories, &target, now)))
        })
    }

    /// Polls every repo that is due; returns how many were polled.
    pub async fn poll_due(&self) -> usize {
        let now = self.now();
        let due: Vec<String> = match self.store().read(|r| {
            let mut repositories = Repositories::new(r);
            Ok::<_, ApiError>(
                repositories
                    .poll_targets()?
                    .into_iter()
                    .filter(|target| poll_is_due(&mut repositories, target, now))
                    .map(|target| target.id)
                    .collect(),
            )
        }) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "github poll: listing repos failed");
                return 0;
            }
        };
        let n = due.len();
        // A few repos at a time, so one slow repo does not hold up the rest; each repo still
        // polls in one task, in order.
        futures::stream::iter(due)
            .for_each_concurrent(POLL_CONCURRENCY, |id| async move {
                if let Err(e) = self.poll_repo(&id).await {
                    tracing::warn!(repo = %id, error = %e, "github poll failed");
                }
            })
            .await;
        n
    }

    /// Polls one repo in the background (`POST /github/repos/:id/sync`, after linking). Counted by
    /// [`GitHub::settle`].
    pub fn spawn_poll(&self, repo_id: String) {
        self.0.queue_pending_add();
        let gh = self.clone();
        let started = self.0.queue.spawn(async move {
            if let Err(e) = gh.poll_repo(&repo_id).await {
                tracing::warn!(repo = %repo_id, error = %e, "github sync failed");
            }
            gh.settled_one();
        });
        if !started {
            self.settled_one();
        }
    }

    /// One poll of one repo, now (whether due or not).
    pub async fn poll_repo(&self, repo_id: &str) -> Result<PollReport, String> {
        let mut rep = PollReport::default();
        let r = self
            .store()
            .read(|c| steve_store::github::repo_get(c, repo_id).map_err(ApiError::from))
            .map_err(|e| e.to_string())?
            .ok_or("仓库已经取消关联")?;
        if !r.enabled {
            return Ok(rep);
        }
        let conn = self
            .conn(&r.workspace_id, &r.connection_id)
            .map_err(|e| e.to_string())?;
        let now = self.now();
        let repository = json!({"id": r.gh_id, "full_name": r.full_name});

        let (scan, newest, new_etag) = self.poll_pulls(&r, &conn, &repository, &mut rep).await?;

        self.poll_checks(&r, &conn, &repository, &mut rep).await?;
        let (issues_due, since) = self
            .poll_issues(&r, &conn, &repository, &mut rep, now)
            .await?;

        // 4. Remember where we are (re-read: the row may have changed meanwhile).
        self.store()
            .write(|w| {
                let mut repositories = Repositories::new(w);
                if let Some(mut st) = repositories.poll_state(&r.id)? {
                    if !st.is_object() {
                        st = json!({});
                    }
                    st["last_poll"] = json!(now);
                    st["pulls_scan"] = scan.clone();
                    if scan.is_null() {
                        st["pulls_since"] = json!(newest);
                        st["pulls_etag"] = json!(new_etag);
                    }
                    if issues_due {
                        st["last_issues_poll"] = json!(now);
                        st["issues_since"] = json!(since);
                    }
                    repositories.set_poll_state(&r.id, &st)?;
                }
                Ok::<_, ApiError>(())
            })
            .map_err(|e| e.to_string())?;
        Ok(rep)
    }

    /// Poll each unsettled SHA once, preserving check/status digest and receipt identity.
    async fn poll_checks(
        &self,
        r: &RepoRow,
        conn: &crate::GhConn,
        repository: &Value,
        rep: &mut PollReport,
    ) -> Result<(), String> {
        // 2. Checks of open PRs that have not passed.
        let shas: Vec<String> = self
            .store()
            .read(|c| {
                let mut out: Vec<String> = Vec::new();
                for p in steve_store::github::prs(c, &r.workspace_id, Some(&r.id), true, None)? {
                    let rows = steve_store::github::checks_for_sha(c, &r.id, &p.head_sha)?;
                    // Settled failures are looked at again: a rerun may fix them.
                    let done = checks_state(&rows) == ("success", true);
                    if !done && !p.head_sha.is_empty() && !out.contains(&p.head_sha) {
                        out.push(p.head_sha);
                    }
                }
                Ok::<_, ApiError>(out)
            })
            .map_err(|e| e.to_string())?;
        for sha in shas {
            let runs_path = format!(
                "/repos/{}/commits/{sha}/check-runs?per_page=100",
                r.full_name
            );
            let status_path = format!("/repos/{}/commits/{sha}/status", r.full_name);
            let (mut runs, mut status) =
                tokio::try_join!(conn.get(&runs_path), conn.get(&status_path))
                    .map_err(|e| e.to_string())?;
            let take_list = |v: &mut Value, k: &str| match v.get_mut(k).map(Value::take) {
                Some(Value::Array(a)) => a,
                _ => Vec::new(),
            };
            let runs = take_list(&mut runs, "check_runs");
            let statuses = take_list(&mut status, "statuses");
            let mut h = Sha256::new();
            for c in &runs {
                h.update(format!("{}|{}|{}\n", c["id"], c["status"], c["conclusion"]));
            }
            for s in &statuses {
                h.update(format!("{}|{}\n", s["context"], s["state"]));
            }
            let digest = hex::encode(h.finalize());
            let key = format!("poll:{}:checks:{sha}:{}", r.id, &digest[..16]);
            let payload = json!({"sha": sha, "check_runs": runs, "statuses": statuses, "repository": repository});
            self.poll_change(r, &key, "poll_checks", &payload, rep)
                .await?;
        }

        Ok(())
    }

    /// Poll due Issues, filtering PR-shaped entries and returning the watermark to commit.
    async fn poll_issues(
        &self,
        r: &RepoRow,
        conn: &crate::GhConn,
        repository: &Value,
        rep: &mut PollReport,
        now: i64,
    ) -> Result<(bool, i64), String> {
        // 3. Issues, every 10 minutes.
        let last_issues = r.poll_state["last_issues_poll"].as_i64().unwrap_or(0);
        let mut since = r.poll_state["issues_since"].as_i64().unwrap_or(0);
        let issues_due = now - last_issues >= ISSUES_EVERY_MS;
        if issues_due {
            let mut path = format!(
                "/repos/{}/issues?state=all&sort=updated&direction=asc&per_page=100",
                r.full_name
            );
            if since > 0 {
                path.push_str(&format!("&since={}", iso(since)));
            }
            let res = conn
                .get_if_changed(&path, None)
                .await
                .map_err(|e| e.to_string())?;
            let list = match res.body {
                Value::Array(v) => v,
                _ => Vec::new(),
            };
            for i in list {
                if !i["pull_request"].is_null() {
                    continue;
                }
                let (Some(n), Some(u)) = (i["number"].as_i64(), i["updated_at"].as_str()) else {
                    continue;
                };
                if let Some(t) = crate::sync::ts(&i["updated_at"]) {
                    since = since.max(t);
                }
                let key = format!("poll:{}:issue:{n}:{u}", r.id);
                let payload = json!({"action": "poll", "issue": i, "repository": repository});
                self.poll_change(r, &key, "issues", &payload, rep).await?;
            }
        }

        Ok((issues_due, since))
    }

    async fn poll_pulls(
        &self,
        r: &RepoRow,
        conn: &crate::GhConn,
        repository: &Value,
        rep: &mut PollReport,
    ) -> Result<(Value, i64, Option<String>), String> {
        // 1. PRs, newest update first. The pulls endpoint has no `since` parameter:
        // https://docs.github.com/en/rest/pulls/pulls#list-pull-requests
        // Keep the old watermark until the whole scan completes, including all timestamps equal
        // to it (GitHub has second precision). Receipts make inclusive replay harmless.
        let first_path = format!(
            "/repos/{}/pulls?state=all&sort=updated&direction=desc&per_page=50",
            r.full_name
        );
        let mut scan = r.poll_state["pulls_scan"].clone();
        let continuing = scan["next"].as_str().is_some();
        let since = r.poll_state["pulls_since"].as_i64().unwrap_or(0);
        let mut newest = scan["newest"].as_i64().unwrap_or(since);
        let mut path = scan["next"].as_str().unwrap_or(&first_path).to_string();
        let mut new_etag = if continuing {
            scan["etag"].as_str()
        } else {
            r.poll_state["pulls_etag"].as_str()
        }
        .map(String::from);
        let mut list = Vec::new();
        for page in 0..PULL_PAGES_MAX {
            // A 304 on page one says nothing about an unfinished tail.
            // An installation upgraded from first-page-only polling has no watermark yet:
            // its old ETag cannot certify that the older pages were ever consumed.
            let etag = (!continuing && page == 0 && r.poll_state["pulls_since"].is_number())
                .then_some(new_etag.as_deref())
                .flatten();
            let res = conn
                .get_if_changed(&path, etag)
                .await
                .map_err(|e| e.to_string())?;
            if res.not_modified() {
                if continuing || page > 0 {
                    return Err("GitHub returned 304 for an unconditional PR page".into());
                }
                rep.pulls_unchanged = true;
                scan = Value::Null;
                break;
            }
            if !continuing && page == 0 {
                new_etag = res.etag;
            }
            let Value::Array(prs) = res.body else {
                return Err("GitHub returned an invalid PR list".into());
            };
            let mut reached = false;
            for pr in prs {
                let updated = crate::sync::ts(&pr["updated_at"])
                    .ok_or("GitHub returned a PR without an update timestamp")?;
                if updated < since {
                    reached = true;
                    break;
                }
                newest = newest.max(updated);
                list.push(pr);
            }
            match res.next.filter(|_| !reached) {
                None => {
                    scan = Value::Null;
                    break;
                }
                Some(next) => path = next,
            }
            scan = json!({"next": path, "newest": newest, "etag": new_etag});
            if page + 1 == PULL_PAGES_MAX {
                rep.pulls_deferred = true;
                tracing::warn!(repo = %r.id, next = %path, pages = PULL_PAGES_MAX,
                    "github PR page limit reached; continuing next poll");
            }
        }
        // Oldest first within this bounded segment. State is committed only after all deliveries
        // are durable; a failure replays the segment using the same synthetic IDs.
        list.reverse();
        for pr in list {
            let (Some(n), Some(u)) = (pr["number"].as_i64(), pr["updated_at"].as_str()) else {
                return Err("GitHub returned a PR without a number".into());
            };
            let head = pr["head"]["sha"].as_str().unwrap_or("");
            let draft = pr["draft"].as_bool().unwrap_or(false);
            let key = format!("poll:{}:pr:{n}:{u}:{head}:{draft}", r.id);
            let payload = json!({"action": "poll", "pull_request": pr, "repository": repository});
            self.poll_change(r, &key, "pull_request", &payload, rep)
                .await?;
        }

        Ok((scan, newest, new_etag))
    }

    /// Applies one polled view unless its synthetic delivery is known. `Err`: it could not even be
    /// recorded, so the poll must not move past it (its cursors stay).
    async fn poll_change(
        &self,
        r: &RepoRow,
        key: &str,
        event: &str,
        payload: &Value,
        rep: &mut PollReport,
    ) -> Result<(), String> {
        let row = DeliveryRow {
            delivery_id: key.into(),
            workspace_id: Some(r.workspace_id.clone()),
            repo_id: Some(r.id.clone()),
            event: event.into(),
            action: Some("poll".into()),
            status: RECEIVED.into(),
            error: None,
            received: self.now(),
        };
        let source = Source::Poll {
            repo_id: r.id.clone(),
        };
        let src = serde_json::to_string(&source).unwrap_or_default();
        let body = payload.to_string();
        match self.store().write(|tx| {
            Deliveries::new(tx)
                .insert_delivery(&row, &src, &body)
                .map_err(ApiError::from)
        }) {
            Ok(true) => {}
            Ok(false) => {
                rep.seen += 1;
                return Ok(());
            }
            Err(e) => {
                tracing::error!(error = %e, "github poll delivery not recorded");
                return Err(format!("轮询到的变化没有记下：{e}"));
            }
        }
        rep.applied += 1;
        if !self.take_delivery(key) {
            return Ok(());
        }
        let out = self
            .apply_as(&source, event, payload, false, Some(key))
            .await;
        self.delivered(key, event, &out);
        self.release_delivery(key);
        Ok(())
    }

    /// Drops deliveries older than [`DELIVERY_KEEP_MS`].
    pub fn prune_deliveries(&self) -> usize {
        let before = self.now() - DELIVERY_KEEP_MS;
        self.store()
            .write(|tx| {
                Deliveries::new(tx)
                    .prune_deliveries(before)
                    .map_err(ApiError::from)
            })
            .unwrap_or(0)
    }

    /// The background job: queues deliveries left `received` (on start: the first tick is at once),
    /// polls what is due every 30 seconds, prunes deliveries hourly. Holds
    /// only a weak reference; stops when the service is dropped.
    pub async fn run_jobs(self) {
        struct Scope(GitHub);
        impl Drop for Scope {
            fn drop(&mut self) {
                self.0.0.queue.close();
            }
        }
        let _scope = Scope(self.clone());
        let weak: Weak<Inner> = std::sync::Arc::downgrade(&self.0);

        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut n: u64 = 0;
        loop {
            tick.tick().await;
            let Some(inner) = weak.upgrade() else { break };
            let gh = GitHub(inner);
            // first what a restart left accepted but unprocessed, then the poll
            gh.recover_deliveries();
            gh.poll_due().await;
            if n.is_multiple_of(120) {
                gh.prune_deliveries();
            }
            n += 1;
        }
    }
}

impl Inner {
    fn queue_pending_add(&self) {
        self.queue.pending.fetch_add(1, Ordering::SeqCst);
    }
}
