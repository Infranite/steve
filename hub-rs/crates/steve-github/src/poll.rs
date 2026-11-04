//! The polling fallback (github.md §5.3): repos without webhooks (`ingest=poll`), and App/hook
//! repos whose last delivery is over 6 hours old while they have an open PR linked to an item (the
//! public entry may be down). Every 2 minutes: the 50 most recently updated PRs (with ETag), then the
//! check runs and statuses of open PRs whose checks have not passed; every 10 minutes the issues
//! updated since the last look.
//!
//! What the poll sees goes through the same handler as webhooks ([`GitHub::apply`]) under a
//! synthetic delivery id `poll:<repo>:<kind>:<id>:<updated>`, recorded in `gh_deliveries`, so the
//! same view is never applied twice and a change the webhook already applied produces no event.

use std::sync::Weak;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_modules::ApiError;

use crate::repo::{self, DeliveryRow, RepoRow};
use crate::sync::{Source, checks_state};
use crate::{GitHub, Inner};

pub const POLL_EVERY_MS: i64 = 2 * 60_000;
pub const ISSUES_EVERY_MS: i64 = 10 * 60_000;
/// No delivery for this long while an item waits on a PR: poll as well.
pub const STALE_MS: i64 = 6 * 3_600_000;
/// Deliveries are kept (for dedupe and the debug list) this long.
pub const DELIVERY_KEEP_MS: i64 = 7 * 24 * 3_600_000;
const TICK: Duration = Duration::from_secs(30);

/// What one poll did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PollReport {
    /// GitHub answered 304 for the PR list.
    pub pulls_unchanged: bool,
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

impl GitHub {
    /// Should `r` be polled now?
    pub fn poll_is_due(&self, c: &rusqlite::Connection, r: &RepoRow, now: i64) -> bool {
        if !r.enabled {
            return false;
        }
        let last = r.poll_state["last_poll"].as_i64().unwrap_or(0);
        if now - last < POLL_EVERY_MS {
            return false;
        }
        if r.ingest == "poll" {
            return true;
        }
        let stale = r.last_delivery.is_none_or(|t| now - t > STALE_MS);
        stale && repo::has_open_linked_pr(c, &r.id).unwrap_or(false)
    }

    /// Polls every repo that is due; returns how many were polled.
    pub async fn poll_due(&self) -> usize {
        let now = self.now();
        let due: Vec<String> = match self.store().reader(|c| {
            Ok::<_, ApiError>(
                repo::enabled_repos(c)?
                    .into_iter()
                    .filter(|r| self.poll_is_due(c, r, now))
                    .map(|r| r.id)
                    .collect(),
            )
        }) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "github poll: listing repos failed");
                return 0;
            }
        };
        for id in &due {
            if let Err(e) = self.poll_repo(id).await {
                tracing::warn!(repo = %id, error = %e, "github poll failed");
            }
        }
        due.len()
    }

    /// Polls one repo in the background (`POST /github/repos/:id/sync`, after linking). Counted by
    /// [`GitHub::settle`].
    pub fn spawn_poll(&self, repo_id: String) {
        self.0.queue_pending_add();
        let gh = self.clone();
        tokio::spawn(async move {
            if let Err(e) = gh.poll_repo(&repo_id).await {
                tracing::warn!(repo = %repo_id, error = %e, "github sync failed");
            }
            gh.settled_one();
        });
    }

    /// One poll of one repo, now (whether due or not).
    pub async fn poll_repo(&self, repo_id: &str) -> Result<PollReport, String> {
        let mut rep = PollReport::default();
        let r = self
            .store()
            .reader(|c| repo::repo_get(c, repo_id).map_err(ApiError::from))
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

        // 1. PRs, newest update first.
        let path = format!(
            "/repos/{}/pulls?state=all&sort=updated&direction=desc&per_page=50",
            r.full_name
        );
        let etag = r.poll_state["pulls_etag"].as_str().map(String::from);
        let res = conn
            .get_if_changed(&path, etag.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        let mut new_etag = etag.clone();
        if res.not_modified() {
            rep.pulls_unchanged = true;
        } else {
            new_etag = res.etag.clone();
            // Oldest first, so transitions apply in order.
            let mut list = res.body.as_array().cloned().unwrap_or_default();
            list.reverse();
            for pr in list {
                let (Some(n), Some(u)) = (pr["number"].as_i64(), pr["updated_at"].as_str()) else {
                    continue;
                };
                let key = format!("poll:{}:pr:{n}:{u}", r.id);
                let payload =
                    json!({"action": "poll", "pull_request": pr, "repository": repository});
                self.poll_change(&r, &key, "pull_request", &payload, &mut rep)
                    .await?;
            }
        }

        // 2. Checks of open PRs that have not passed.
        let shas: Vec<String> = self
            .store()
            .reader(|c| {
                let mut out: Vec<String> = Vec::new();
                for p in repo::prs(c, &r.workspace_id, Some(&r.id), true, None)? {
                    let rows = repo::checks_for_sha(c, &r.id, &p.head_sha)?;
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
            let runs = conn
                .get(&format!(
                    "/repos/{}/commits/{sha}/check-runs?per_page=100",
                    r.full_name
                ))
                .await
                .map_err(|e| e.to_string())?;
            let status = conn
                .get(&format!("/repos/{}/commits/{sha}/status", r.full_name))
                .await
                .map_err(|e| e.to_string())?;
            let runs = runs["check_runs"].as_array().cloned().unwrap_or_default();
            let statuses = status["statuses"].as_array().cloned().unwrap_or_default();
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
            self.poll_change(&r, &key, "poll_checks", &payload, &mut rep)
                .await?;
        }

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
            for i in res.body.as_array().cloned().unwrap_or_default() {
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
                self.poll_change(&r, &key, "issues", &payload, &mut rep)
                    .await?;
            }
        }

        // 4. Remember where we are (re-read: the row may have changed meanwhile).
        self.store()
            .transaction(|tx| {
                if let Some(cur) = repo::repo_get(tx, &r.id)? {
                    let mut st = cur.poll_state.clone();
                    if !st.is_object() {
                        st = json!({});
                    }
                    st["last_poll"] = json!(now);
                    if let Some(e) = &new_etag {
                        st["pulls_etag"] = json!(e);
                    }
                    if issues_due {
                        st["last_issues_poll"] = json!(now);
                        st["issues_since"] = json!(since);
                    }
                    repo::set_poll_state(tx, &r.id, &st)?;
                }
                Ok::<_, ApiError>(())
            })
            .map_err(|e| e.to_string())?;
        Ok(rep)
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
            status: repo::RECEIVED.into(),
            error: None,
            received: self.now(),
        };
        let source = Source::Poll {
            repo_id: r.id.clone(),
        };
        let src = serde_json::to_string(&source).unwrap_or_default();
        let body = payload.to_string();
        match self
            .store()
            .transaction(|tx| repo::insert_delivery(tx, &row, &src, &body).map_err(ApiError::from))
        {
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
        let out = self.apply(&source, event, payload).await;
        self.delivered(key, event, &out);
        self.release_delivery(key);
        Ok(())
    }

    /// Drops deliveries older than [`DELIVERY_KEEP_MS`].
    pub fn prune_deliveries(&self) -> usize {
        let before = self.now() - DELIVERY_KEEP_MS;
        self.store()
            .transaction(|tx| repo::prune_deliveries(tx, before).map_err(ApiError::from))
            .unwrap_or(0)
    }

    /// The background job: queues deliveries left `received` (on start: the first tick is at once),
    /// polls what is due every 30 seconds, prunes deliveries hourly. Holds
    /// only a weak reference; stops when the service is dropped.
    pub fn spawn_jobs(&self) -> tokio::task::JoinHandle<()> {
        let weak: Weak<Inner> = std::sync::Arc::downgrade(&self.0);
        tokio::spawn(async move {
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
        })
    }
}

impl Inner {
    fn queue_pending_add(&self) {
        self.queue.pending.fetch_add(1, Ordering::SeqCst);
    }
}
