//! Offline token-replay audit for #503 selective workflow reopen rules.
//!
//! Opens a SQLite snapshot strictly read-only (`mode=ro&immutable=1`), reconstructs each
//! historical workflow structurally (hashing all prose/paths so no user content is persisted or
//! printed), and replays every reset event directly through `steve_store::Workflows` on isolated
//! temporary databases to measure:
//! - old rule (`reset_scope` recorded in `workflow_history.detail.invalidated`)
//! - current `#528` rules (`retry_unit` + replacement `deliver`/`propagate_output`, and
//!   `apply_review_findings` + `Workflow::review_scope`)
//! - subsequent turn token usage (`usage_records`, `source = 'agent'`, aggregated by `turn_id`)
//!
//! Usage:
//!   cargo run --manifest-path hub-rs/Cargo.toml -p steve-store --example workflow_token_replay -- \
//!     <snapshot.sqlite3> [--from 2026-11-11] [--to 2026-11-22] [--json]

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_store::testkit::SqliteFixtures;
use steve_store::{
    ItemPatch, LifecycleWrite, NewBot, NewItem, NewProject, NewSession, NoPublisher, Store,
    UnitSpec, WS, Workflow, claim_in, delivery_repo, lifecycle_write_in,
};
use steve_types::orchestration::{Finding, ReviewReceipt};
use tempfile::TempDir;

#[derive(Debug, Clone, Default, Serialize)]
pub struct TokenTotals {
    pub turns: usize,
    pub input: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
    pub total: i64,
}

impl TokenTotals {
    fn add_turn(&mut self, t: &AgentTurnUsage) {
        self.turns += 1;
        self.input += t.input;
        self.cache_read += t.cache_read;
        self.cache_write += t.cache_write;
        self.output += t.output;
        self.total += t.total();
    }

    fn add(&mut self, other: &TokenTotals) {
        self.turns += other.turns;
        self.input += other.input;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.output += other.output;
        self.total += other.total;
    }
}

#[derive(Debug, Clone)]
struct AgentTurnUsage {
    input: i64,
    cache_read: i64,
    cache_write: i64,
    output: i64,
}

impl AgentTurnUsage {
    fn total(&self) -> i64 {
        self.input + self.cache_read + self.cache_write + self.output
    }
}

#[derive(Debug, Clone)]
struct RawHistoryRow {
    revision: i64,
    action: String,
    unit_id: Option<String>,
    detail: Value,
}

#[derive(Debug, Clone)]
struct RawDeliveryMeta {
    url_hash: String,
    sha256: String,
    mime: String,
    size: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResetReplayEvent {
    pub workflow_id: String,
    pub reset_revision: i64,
    pub has_project: bool,
    pub target_unit: String,
    pub has_failed_review: bool,
    pub paired_sibling_after_failed_review: bool,
    pub old_invalidated_total: usize,
    pub old_invalidated_non_pending: usize,
    pub old_units: Vec<OldInvalidatedUnit>,
    pub retry_reopened_immediate: Vec<String>,
    pub retry_reopened_after_deliver: Vec<(String, Option<String>)>,
    pub retry_reopened_total_non_pending: usize,
    pub retry_stuck_failed_downstream: Vec<String>,
    pub retry_preserved_succeeded_downstream: Vec<String>,
    pub retry_blocked_following_replan: bool,
    pub review_scope_available: bool,
    pub target_in_review_scope: Option<bool>,
    pub review_has_delivery_receipt: bool,
    pub apply_findings_receipt: Option<String>,
    pub apply_findings_reopened: Vec<String>,
    pub apply_findings_rejection_gates: Vec<String>,
    pub counterfactual_non_project_receipt: Option<String>,
    pub counterfactual_non_project_reopened: Vec<String>,
    pub linked_turns: Vec<LinkedTurnRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OldInvalidatedUnit {
    pub unit_id: String,
    pub kind: String,
    pub old_status: String,
    pub old_attempt: i64,
    pub is_target: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkedTurnRecord {
    pub start_revision: i64,
    pub unit_id: String,
    pub kind: String,
    pub old_status: String,
    pub is_target: bool,
    pub same_goal: bool,
    pub tokens: TokenTotals,
    pub reopened_by_retry_rule: bool,
    pub stuck_failed_under_retry_rule: bool,
    pub reopened_by_apply_findings: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplaySummary {
    pub day_from: String,
    pub day_to: String,
    pub agent_total: TokenTotals,
    pub workflows_total: usize,
    pub workflows_with_project: usize,
    pub resets_total: usize,
    pub review_failed_resets: usize,
    pub review_failed_old_invalidated_total: usize,
    pub review_failed_old_invalidated_non_pending: usize,
    pub review_failed_retry_reopened_immediate: usize,
    pub review_failed_retry_reopened_propagated: usize,
    pub review_failed_retry_reopened_total: usize,
    pub review_failed_retry_stuck_failed_downstream: usize,
    pub review_failed_retry_preserved_succeeded_downstream: usize,
    pub review_failed_retry_blocked_replan_count: usize,
    pub review_failed_apply_findings_applied: usize,
    pub review_failed_apply_findings_unverified: usize,
    pub review_failed_apply_findings_reopened_units: usize,
    pub review_failed_apply_findings_gates: BTreeMap<String, usize>,
    pub review_failed_turns_total: TokenTotals,
    pub review_failed_prev_succeeded_turns: TokenTotals,
    pub review_failed_prev_succeeded_same_goal_turns: TokenTotals,
    pub review_failed_by_category: BTreeMap<String, TokenTotals>,
    pub review_failed_retry_reopened_tokens: TokenTotals,
    pub review_failed_retry_stuck_failed_tokens: TokenTotals,
    pub review_failed_pending_first_run_tokens: TokenTotals,
    pub review_failed_genuinely_avoidable_tokens: TokenTotals,
    pub review_failed_counterfactual_collateral_ceiling: TokenTotals,
    pub paired_sibling_resets: usize,
    pub paired_sibling_tokens: TokenTotals,
    pub non_review_collateral_succeeded_resets: usize,
    pub non_review_collateral_succeeded_tokens: TokenTotals,
    pub non_review_stuck_failed_downstream: usize,
    pub non_review_stuck_failed_tokens: TokenTotals,
    pub transitive_review_workflows: Vec<String>,
    pub events: Vec<ResetReplayEvent>,
}

fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    let mut out = String::with_capacity(64);
    for b in h.finalize() {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

fn short_hash(prefix: &str, input: &str) -> String {
    let h = sha256_hex(input);
    format!("{prefix}:{}", &h[..16])
}

fn sanitize_outputs(outputs: &Value) -> Value {
    let items = outputs
        .as_array()
        .into_iter()
        .flatten()
        .map(|out| {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "kind".into(),
                json!(out.get("kind").and_then(Value::as_str).unwrap_or("file")),
            );
            if let Some(title) = out.get("title").and_then(Value::as_str) {
                obj.insert("title".into(), json!(short_hash("title", title)));
            }
            if let Some(loc) = out.get("location").and_then(Value::as_str) {
                obj.insert("location".into(), json!(short_hash("loc", loc)));
            }
            if let Some(role) = out.get("role").and_then(Value::as_str) {
                obj.insert("role".into(), json!(role));
            }
            Value::Object(obj)
        })
        .collect::<Vec<_>>();
    Value::Array(items)
}

fn sanitize_checks(checks: &Value) -> Value {
    let items = checks
        .as_array()
        .into_iter()
        .flatten()
        .map(|chk| {
            let mut obj = serde_json::Map::new();
            let name = chk.get("name").and_then(Value::as_str).unwrap_or("check");
            obj.insert("name".into(), json!(short_hash("chk", name)));
            obj.insert(
                "passed".into(),
                json!(chk.get("passed").and_then(Value::as_bool).unwrap_or(false)),
            );
            if let Some(scope) = chk.get("scope").and_then(Value::as_str) {
                obj.insert("scope".into(), json!(scope));
            }
            if let Some(op) = chk.get("operated").and_then(Value::as_bool) {
                obj.insert("operated".into(), json!(op));
            }
            Value::Object(obj)
        })
        .collect::<Vec<_>>();
    Value::Array(items)
}

fn sanitize_specs(raw_units: &[Value], bot_map: &mut HashMap<String, String>) -> Vec<UnitSpec> {
    raw_units
        .iter()
        .map(|u| {
            let id = u["id"].as_str().unwrap_or("unit").to_string();
            let raw_bot = u["bot_id"].as_str().unwrap_or("bot");
            let next_idx = bot_map.len();
            let bot_id = bot_map
                .entry(raw_bot.to_string())
                .or_insert_with(|| format!("bot_{next_idx}"))
                .clone();
            let goal_raw = u["goal"].as_str().unwrap_or("");
            let depends_on = u["depends_on"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect();
            let resources = u["resources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|r| short_hash("res", r))
                .collect();
            UnitSpec {
                id: id.clone(),
                title: id,
                goal: format!("goal:{}", sha256_hex(goal_raw)),
                bot_id,
                kind: u["kind"].as_str().unwrap_or("work").to_string(),
                depends_on,
                resources,
                required: u["required"].as_bool().unwrap_or(true),
            }
        })
        .collect()
}

fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    let uri = format!("file:{}?mode=ro&immutable=1", path.to_string_lossy());
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
}

fn task_tags(store: &Store) -> Vec<String> {
    vec![
        store
            .tags()
            .kinds(WS)
            .unwrap()
            .into_iter()
            .find(|tag| tag.name == "feat")
            .expect("feat tag")
            .id,
    ]
}

fn init_replay_store(has_project: bool, bot_ids: &[String]) -> (TempDir, Store, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("replay.sqlite3"), NoPublisher).expect("open store");
    store.ensure_workspace().expect("workspace");
    for bot_id in bot_ids {
        store
            .bots()
            .seed(
                bot_id,
                false,
                &NewBot {
                    name: bot_id.clone(),
                    harness: "fixture".into(),
                    ..Default::default()
                },
                WS,
            )
            .expect("seed bot");
    }
    let lead = bot_ids
        .first()
        .cloned()
        .unwrap_or_else(|| "bot_0".to_string());
    let parent = store
        .items()
        .create(
            WS,
            &NewItem {
                tags: task_tags(&store),
                title: "parent".into(),
                goal: "goal:init".into(),
                bot_id: lead,
                ..Default::default()
            },
        )
        .expect("create parent")
        .expect("parent item")
        .id;
    if has_project {
        let project = store
            .projects()
            .create(
                WS,
                &NewProject {
                    name: "repo".into(),
                    kind: "git".into(),
                    path: "repo".into(),
                    worktree: true,
                    ..Default::default()
                },
            )
            .expect("create project");
        store
            .items()
            .patch(
                &parent,
                &ItemPatch {
                    project_id: Some(Some(project.id)),
                    ..Default::default()
                },
            )
            .expect("attach project");
    }
    (dir, store, parent)
}

fn clone_store(store: &Store) -> (TempDir, Store) {
    let dir = tempfile::tempdir().expect("clone tempdir");
    let dst = dir.path().join("clone.sqlite3");
    let dst_str = dst.to_string_lossy().into_owned();
    store
        .fixture_write(|c| {
            c.execute("VACUUM INTO ?", [dst_str.as_str()])?;
            Ok(())
        })
        .expect("vacuum into clone");
    let cloned = Store::open(&dst, NoPublisher).expect("open clone");
    (dir, cloned)
}

fn claim_or_reclaim(store: &Store, parent: &str, unit_id: &str) -> steve_store::WorkUnit {
    let existing_child = store
        .workflows()
        .get(parent)
        .expect("get graph")
        .expect("graph")
        .units
        .into_iter()
        .find(|u| u.spec.id == unit_id && u.status == "pending")
        .and_then(|u| u.item_id);
    match existing_child {
        Some(child) => {
            let item = store
                .items()
                .get(&child)
                .expect("get child")
                .expect("child");
            store
                .write(|w| {
                    lifecycle_write_in(
                        w,
                        &child,
                        &LifecycleWrite {
                            phase: steve_types::ItemPhase::Ready,
                            cycle: item.cycle + 1,
                            delivery_revision: item.delivery_revision,
                            outcome: item.outcome,
                            acceptance: item.acceptance,
                            closing: item.closing,
                            closed_at: item.closed_at,
                            closed_reason: item.closed_reason,
                        },
                    )?;
                    claim_in(w, parent, unit_id)
                })
                .expect("reclaim unit")
        }
        None => store
            .workflows()
            .claim(parent, unit_id)
            .expect("claim unit"),
    }
}

fn start_unit_session(
    store: &Store,
    dir: &TempDir,
    parent: &str,
    unit_id: &str,
    token_seed: usize,
) -> (String, String) {
    let graph = store
        .workflows()
        .get(parent)
        .expect("get graph")
        .expect("graph");
    let unit = graph
        .units
        .iter()
        .find(|u| u.spec.id == unit_id)
        .expect("unit for start");
    let child = unit.item_id.clone().expect("claimed child");
    let session = store
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            item_id: Some(child.clone()),
            bot_id: Some(unit.spec.bot_id.clone()),
            harness: "fixture".into(),
            cwd: dir.path().to_string_lossy().into_owned(),
            title: unit_id.into(),
            token: format!("tok-{unit_id}-{}-{token_seed}", unit.attempt),
            ..Default::default()
        })
        .expect("create session");
    store
        .workflows()
        .bind_session(parent, unit_id, &child, &session.id)
        .expect("bind session");
    (child, session.id)
}

fn seed_deliveries(
    store: &Store,
    child: &str,
    session_id: &str,
    metas: &[RawDeliveryMeta],
    seq_prefix: &str,
) {
    for (idx, m) in metas.iter().enumerate() {
        let id = format!("deliv-{seq_prefix}-{idx}");
        let url = m.url_hash.clone();
        store
            .write(|w| {
                delivery_repo::insert(
                    w,
                    &delivery_repo::Delivery {
                        id,
                        item_id: Some(child.to_string()),
                        title: "artifact".into(),
                        name: "artifact.bin".into(),
                        mime: m.mime.clone(),
                        size: m.size.max(0) as u64,
                        created: 1,
                        entrypoint: None,
                        preview: json!({}),
                        interactive: false,
                        reply: false,
                        workspace_id: WS.into(),
                        owner_id: None,
                        registered_url: url,
                        sha256: m.sha256.clone(),
                        manifest: Default::default(),
                    },
                    Some(session_id),
                )
            })
            .expect("seed delivery");
    }
}

fn sync_parent_cycle_and_goal(
    store: &Store,
    parent: &str,
    target_cycle: i64,
    target_goal_version: i64,
) {
    let item = store.items().get(parent).expect("item").expect("parent");
    if target_cycle > item.cycle {
        store
            .fixture_write(|c| {
                c.execute(
                    "UPDATE items SET cycle=? WHERE id=?",
                    rusqlite::params![target_cycle, parent],
                )?;
                Ok(())
            })
            .expect("bump cycle");
    }
    let current_goal_ver: i64 = store
        .fixture_read::<_, steve_store::StoreError>(|c| {
            Ok(c.query_row(
                "SELECT coalesce(max(version),0) FROM item_goals WHERE item_id=?",
                [parent],
                |r| r.get(0),
            )?)
        })
        .expect("read goal version");
    for v in (current_goal_ver + 1)..=target_goal_version {
        store
            .items()
            .add_goal(parent, &format!("goal:v{v}"))
            .expect("add parent goal");
    }
}

fn find_next_deliver_for_unit<'a>(
    events: &'a [RawHistoryRow],
    after_idx: usize,
    unit_id: &str,
) -> Option<&'a RawHistoryRow> {
    for ev in &events[(after_idx + 1)..] {
        if ev.action == "reset" && ev.unit_id.as_deref() == Some(unit_id) {
            return None;
        }
        if ev.action == "replan" {
            let before_cycle = ev.detail["before"]["cycle"].as_i64();
            let next_cycle = ev.detail["cycle"].as_i64();
            let before_gv = ev.detail["before"]["goal_version"].as_i64();
            let next_gv = ev.detail["goal_version"].as_i64();
            if before_cycle != next_cycle || before_gv != next_gv {
                return None;
            }
        }
        if ev.action == "deliver" && ev.unit_id.as_deref() == Some(unit_id) {
            return Some(ev);
        }
    }
    None
}

fn find_immediate_replan<'a>(
    events: &'a [RawHistoryRow],
    after_idx: usize,
) -> Option<&'a RawHistoryRow> {
    for ev in &events[(after_idx + 1)..] {
        match ev.action.as_str() {
            "replan" => return Some(ev),
            "pause" | "resume" => continue,
            _ => return None,
        }
    }
    None
}

fn collect_transitive_review_workflows(
    by_wf: &BTreeMap<String, Vec<RawHistoryRow>>,
) -> Vec<String> {
    let mut found = BTreeSet::new();
    for (wf, events) in by_wf {
        for ev in events {
            if !matches!(ev.action.as_str(), "plan" | "replan") {
                continue;
            }
            let Some(units) = ev.detail.get("units").and_then(Value::as_array) else {
                continue;
            };
            let by_id: HashMap<&str, &Value> = units
                .iter()
                .filter_map(|u| Some((u["id"].as_str()?, u)))
                .collect();
            for u in units {
                if u["kind"].as_str() != Some("review") {
                    continue;
                }
                let direct: HashSet<&str> = u["depends_on"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let mut ancestors = HashSet::new();
                let mut todo: Vec<&str> = direct.iter().copied().collect();
                while let Some(cur) = todo.pop() {
                    if ancestors.insert(cur)
                        && let Some(node) = by_id.get(cur)
                    {
                        todo.extend(
                            node["depends_on"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str),
                        );
                    }
                }
                if ancestors.len() > direct.len() {
                    found.insert(wf.clone());
                }
            }
        }
    }
    found.into_iter().collect()
}

pub fn replay_snapshot(snapshot: &Path, day_from: &str, day_to: &str) -> ReplaySummary {
    let con = open_readonly(snapshot).expect("open snapshot read-only");

    // 1. Load agent turn usage in [day_from, day_to], matching TOKEN-AUDIT.md口径.
    let mut agent_total = TokenTotals::default();
    let mut turns_by_session: HashMap<String, Vec<AgentTurnUsage>> = HashMap::new();
    {
        let mut stmt = con
            .prepare(
                "SELECT t.session_id,
                        COALESCE(SUM(u.input), 0),
                        COALESCE(SUM(u.cache_read), 0),
                        COALESCE(SUM(u.cache_write), 0),
                        COALESCE(SUM(u.output), 0)
                 FROM usage_records u
                 JOIN agent_turns t ON t.id = u.turn_id
                 WHERE u.source = 'agent' AND u.day BETWEEN ?1 AND ?2
                 GROUP BY t.id
                 ORDER BY t.started, t.id",
            )
            .expect("prepare agent turns");
        let rows = stmt
            .query_map([day_from, day_to], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    AgentTurnUsage {
                        input: r.get(1)?,
                        cache_read: r.get(2)?,
                        cache_write: r.get(3)?,
                        output: r.get(4)?,
                    },
                ))
            })
            .expect("query agent turns");
        for row in rows {
            let (session_id, usage) = row.expect("turn row");
            agent_total.add_turn(&usage);
            turns_by_session.entry(session_id).or_default().push(usage);
        }
    }

    // 2. Load deliveries metadata (hashed URL + digest/mime/size; never prose).
    let mut deliveries_by_item_session: HashMap<(String, String), Vec<RawDeliveryMeta>> =
        HashMap::new();
    {
        let mut stmt = con
            .prepare(
                "SELECT item_id, session_id, url, sha256, mime, size
                 FROM deliveries
                 WHERE item_id IS NOT NULL AND session_id IS NOT NULL",
            )
            .expect("prepare deliveries");
        let rows = stmt
            .query_map([], |r| {
                let item_id: String = r.get(0)?;
                let session_id: String = r.get(1)?;
                let url: String = r.get(2)?;
                Ok((
                    (item_id, session_id),
                    RawDeliveryMeta {
                        url_hash: short_hash("loc", &url),
                        sha256: r.get(3)?,
                        mime: r.get(4)?,
                        size: r.get(5)?,
                    },
                ))
            })
            .expect("query deliveries");
        for row in rows {
            let (key, meta) = row.expect("delivery row");
            deliveries_by_item_session
                .entry(key)
                .or_default()
                .push(meta);
        }
    }

    // 3. Load workflows and parent project presence.
    let mut workflows_meta: Vec<(String, bool)> = Vec::new();
    {
        let mut stmt = con
            .prepare(
                "SELECT w.item_id, i.project_id IS NOT NULL
                 FROM workflows w
                 JOIN items i ON i.id = w.item_id
                 ORDER BY w.created, w.item_id",
            )
            .expect("prepare workflows");
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0))
            })
            .expect("query workflows");
        for row in rows {
            workflows_meta.push(row.expect("workflow meta"));
        }
    }

    // 4. Load all workflow_history rows.
    let mut by_wf: BTreeMap<String, Vec<RawHistoryRow>> = BTreeMap::new();
    {
        let mut stmt = con
            .prepare(
                "SELECT workflow_id, revision, action, unit_id, detail
                 FROM workflow_history
                 ORDER BY workflow_id, revision",
            )
            .expect("prepare history");
        let rows = stmt
            .query_map([], |r| {
                let wf: String = r.get(0)?;
                let detail_str: String = r.get(4)?;
                let detail: Value = serde_json::from_str(&detail_str).expect("history detail json");
                Ok((
                    wf,
                    RawHistoryRow {
                        revision: r.get(1)?,
                        action: r.get(2)?,
                        unit_id: r.get(3)?,
                        detail,
                    },
                ))
            })
            .expect("query history");
        for row in rows {
            let (wf, h) = row.expect("history row");
            by_wf.entry(wf).or_default().push(h);
        }
    }

    let transitive_review_workflows = collect_transitive_review_workflows(&by_wf);
    let mut all_events: Vec<ResetReplayEvent> = Vec::new();

    // 5. Replay every workflow linearly through steve_store::Workflows.
    for (wf_id, has_project) in &workflows_meta {
        let Some(events) = by_wf.get(wf_id) else {
            continue;
        };
        // Pre-scan bots so all bots are seeded cleanly before plan/replan.
        let mut bot_map: HashMap<String, String> = HashMap::new();
        for ev in events {
            if matches!(ev.action.as_str(), "plan" | "replan")
                && let Some(units) = ev.detail.get("units").and_then(Value::as_array)
            {
                let _ = sanitize_specs(units, &mut bot_map);
            }
        }
        let mut bot_ids: Vec<String> = bot_map.values().cloned().collect();
        bot_ids.sort();
        bot_ids.dedup();
        if bot_ids.is_empty() {
            bot_ids.push("bot_0".into());
        }

        let (base_dir, base_store, parent) = init_replay_store(*has_project, &bot_ids);
        let mut latest_goal_hash: HashMap<String, String> = HashMap::new();
        let mut active_session_by_unit: HashMap<String, (String, String)> = HashMap::new();
        let mut last_raw_child_session: HashMap<String, (String, String)> = HashMap::new();
        let mut last_reset_event_for_unit: HashMap<String, (usize, String, String, String, bool)> =
            HashMap::new();
        let mut token_seed = 0usize;
        let mut prev_was_failed_review_reset = false;

        for (idx, ev) in events.iter().enumerate() {
            match ev.action.as_str() {
                "plan" | "replan" => {
                    let units_raw = ev.detail["units"].as_array().expect("plan units");
                    let specs = sanitize_specs(units_raw, &mut bot_map);
                    for u in &specs {
                        latest_goal_hash.insert(u.id.clone(), u.goal.clone());
                    }
                    let max_parallel = ev.detail["max_parallel"]
                        .as_u64()
                        .unwrap_or(specs.len().max(1) as u64)
                        as usize;
                    let max_attempts = ev.detail["max_attempts"].as_i64().unwrap_or(3);
                    let cycle = ev.detail["cycle"].as_i64().unwrap_or(1);
                    let goal_version = ev.detail["goal_version"].as_i64().unwrap_or(1);
                    sync_parent_cycle_and_goal(&base_store, &parent, cycle, goal_version);
                    let expected_rev = base_store
                        .workflows()
                        .get(&parent)
                        .expect("get graph")
                        .map_or(0, |g| g.revision);
                    base_store
                        .workflows()
                        .replace(
                            &parent,
                            expected_rev,
                            "replay",
                            &specs,
                            max_parallel,
                            max_attempts,
                        )
                        .expect("replay replace");
                    prev_was_failed_review_reset = false;
                }
                "pause" => {
                    base_store
                        .workflows()
                        .pause(&parent, true, "replay")
                        .expect("replay pause");
                    prev_was_failed_review_reset = false;
                }
                "resume" => {
                    base_store
                        .workflows()
                        .pause(&parent, false, "replay")
                        .expect("replay resume");
                    prev_was_failed_review_reset = false;
                }
                "single_session" => {
                    let g = base_store
                        .workflows()
                        .get(&parent)
                        .expect("get graph")
                        .expect("graph");
                    base_store
                        .workflows()
                        .accept_single_session(&parent, g.revision, "replay", "owner")
                        .expect("replay single_session");
                    prev_was_failed_review_reset = false;
                }
                "cancel" => {
                    let uid = ev.unit_id.as_deref().expect("cancel unit_id");
                    base_store
                        .workflows()
                        .cancel(&parent, uid, "replay")
                        .expect("replay cancel");
                    prev_was_failed_review_reset = false;
                }
                "claim" => {
                    let uid = ev.unit_id.as_deref().expect("claim unit_id");
                    let _ = claim_or_reclaim(&base_store, &parent, uid);
                    prev_was_failed_review_reset = false;
                }
                "start" => {
                    let uid = ev.unit_id.as_deref().expect("start unit_id");
                    token_seed += 1;
                    let (child, sid) =
                        start_unit_session(&base_store, &base_dir, &parent, uid, token_seed);
                    active_session_by_unit.insert(uid.to_string(), (child, sid));
                    let raw_child = ev.detail["child"].as_str().unwrap_or("").to_string();
                    let raw_sess = ev.detail["session"].as_str().unwrap_or("").to_string();
                    last_raw_child_session.insert(uid.to_string(), (raw_child, raw_sess.clone()));

                    // Attribute any agent turns in the window to the most recent reset covering uid.
                    if let Some((ev_idx, old_kind, old_status, old_goal_hash, is_target)) =
                        last_reset_event_for_unit.get(uid)
                        && let Some(turns) = turns_by_session.get(&raw_sess)
                    {
                        let same_goal = latest_goal_hash.get(uid) == Some(old_goal_hash);
                        let target_event = &mut all_events[*ev_idx];
                        let reopened_by_retry = target_event
                            .retry_reopened_immediate
                            .iter()
                            .any(|id| id == uid)
                            || target_event
                                .retry_reopened_after_deliver
                                .iter()
                                .any(|(id, _)| id == uid);
                        let stuck_failed = target_event
                            .retry_stuck_failed_downstream
                            .iter()
                            .any(|id| id == uid);
                        let reopened_by_apply = target_event
                            .apply_findings_reopened
                            .iter()
                            .any(|id| id == uid);
                        for t in turns {
                            let mut tok = TokenTotals::default();
                            tok.add_turn(t);
                            target_event.linked_turns.push(LinkedTurnRecord {
                                start_revision: ev.revision,
                                unit_id: uid.to_string(),
                                kind: old_kind.clone(),
                                old_status: old_status.clone(),
                                is_target: *is_target,
                                same_goal,
                                tokens: tok,
                                reopened_by_retry_rule: reopened_by_retry,
                                stuck_failed_under_retry_rule: stuck_failed,
                                reopened_by_apply_findings: reopened_by_apply,
                            });
                        }
                    }
                    prev_was_failed_review_reset = false;
                }
                "deliver" => {
                    let uid = ev.unit_id.as_deref().expect("deliver unit_id");
                    let passed = ev.detail["status"].as_str() == Some("succeeded");
                    let (child, sid) = active_session_by_unit
                        .get(uid)
                        .expect("active session for deliver")
                        .clone();
                    let raw_child = ev.detail["child"].as_str().unwrap_or("").to_string();
                    let raw_sess = ev.detail["session"].as_str().unwrap_or("").to_string();
                    if let Some(metas) =
                        deliveries_by_item_session.get(&(raw_child.clone(), raw_sess.clone()))
                    {
                        seed_deliveries(
                            &base_store,
                            &child,
                            &sid,
                            metas,
                            &format!("b-{wf_id}-r{}", ev.revision),
                        );
                    }
                    let outputs = sanitize_outputs(&ev.detail["outputs"]);
                    let checks = sanitize_checks(&ev.detail["checks"]);
                    base_store
                        .workflows()
                        .deliver(&child, &sid, "replay", &outputs, &checks, passed)
                        .expect("replay deliver");
                    prev_was_failed_review_reset = false;
                }
                "reset" => {
                    let target_id = ev.unit_id.as_deref().expect("reset unit_id");
                    let inv = ev.detail["invalidated"]
                        .as_array()
                        .expect("reset invalidated");
                    let has_failed_review = inv.iter().any(|u| {
                        u["kind"].as_str() == Some("review")
                            && u["status"].as_str() == Some("failed")
                    });
                    let paired_sibling = !has_failed_review
                        && prev_was_failed_review_reset
                        && inv.iter().any(|u| {
                            u["id"].as_str() == Some(target_id)
                                && u["status"].as_str() == Some("succeeded")
                        });
                    let old_units: Vec<OldInvalidatedUnit> = inv
                        .iter()
                        .map(|u| {
                            let uid = u["id"].as_str().unwrap_or("").to_string();
                            OldInvalidatedUnit {
                                is_target: uid == target_id,
                                unit_id: uid,
                                kind: u["kind"].as_str().unwrap_or("work").to_string(),
                                old_status: u["status"].as_str().unwrap_or("pending").to_string(),
                                old_attempt: u["attempt"].as_i64().unwrap_or(0),
                            }
                        })
                        .collect();
                    let old_invalidated_non_pending = old_units
                        .iter()
                        .filter(|u| u.old_status != "pending")
                        .count();

                    // Pre-reset graph snapshot from real steve_store.
                    let before_graph = base_store
                        .workflows()
                        .get(&parent)
                        .expect("get pre-reset graph")
                        .expect("pre-reset graph");
                    let before_status: HashMap<String, String> = before_graph
                        .units
                        .iter()
                        .map(|u| (u.spec.id.clone(), u.status.clone()))
                        .collect();

                    // === PROBE A: Real #528 `retry_unit` + replacement `deliver` (`propagate_output`) ===
                    let (probe_a_dir, probe_a) = clone_store(&base_store);
                    let after_retry = probe_a
                        .workflows()
                        .retry_unit(&parent, before_graph.revision, target_id, "replay")
                        .expect("probe_a retry_unit");
                    let retry_reopened_immediate: Vec<String> = after_retry
                        .units
                        .iter()
                        .filter(|u| {
                            u.status == "pending"
                                && before_status.get(&u.spec.id).map(String::as_str)
                                    != Some("pending")
                        })
                        .map(|u| u.spec.id.clone())
                        .collect();

                    // Check if the historical immediate replan conflicts under #528's deferred downstream reopen.
                    let retry_blocked_following_replan =
                        if let Some(replan_ev) = find_immediate_replan(events, idx) {
                            let mut tmp_bots = bot_map.clone();
                            let specs = sanitize_specs(
                                replan_ev.detail["units"].as_array().expect("replan units"),
                                &mut tmp_bots,
                            );
                            let mp = replan_ev.detail["max_parallel"]
                                .as_u64()
                                .unwrap_or(specs.len().max(1) as u64)
                                as usize;
                            let ma = replan_ev.detail["max_attempts"].as_i64().unwrap_or(3);
                            let (_replan_dir, replan_probe) = clone_store(&probe_a);
                            replan_probe
                                .workflows()
                                .replace(&parent, after_retry.revision, "replay", &specs, mp, ma)
                                .is_err()
                        } else {
                            false
                        };

                    // Simulate replacement delivery of target_id using its actual subsequent deliver event.
                    let mut retry_reopened_after_deliver: Vec<(String, Option<String>)> =
                        Vec::new();
                    let mut retry_stuck_failed_downstream: Vec<String> = Vec::new();
                    let mut retry_preserved_succeeded_downstream: Vec<String> = Vec::new();
                    if let Some(next_deliver) = find_next_deliver_for_unit(events, idx, target_id)
                        && next_deliver.detail["status"].as_str() == Some("succeeded")
                    {
                        if after_retry.paused {
                            probe_a
                                .workflows()
                                .pause(&parent, false, "replay")
                                .expect("unpause probe_a");
                        }
                        let _ = claim_or_reclaim(&probe_a, &parent, target_id);
                        token_seed += 1;
                        let (child, sid) = start_unit_session(
                            &probe_a,
                            &probe_a_dir,
                            &parent,
                            target_id,
                            token_seed,
                        );
                        let raw_child = next_deliver.detail["child"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        let raw_sess = next_deliver.detail["session"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        if let Some(metas) = deliveries_by_item_session.get(&(raw_child, raw_sess))
                        {
                            seed_deliveries(
                                &probe_a,
                                &child,
                                &sid,
                                metas,
                                &format!("pa-{wf_id}-r{}", ev.revision),
                            );
                        }
                        let outputs = sanitize_outputs(&next_deliver.detail["outputs"]);
                        let checks = sanitize_checks(&next_deliver.detail["checks"]);
                        let after_deliver = probe_a
                            .workflows()
                            .deliver(&child, &sid, "replay", &outputs, &checks, true)
                            .expect("probe_a deliver");
                        for u in &after_deliver.units {
                            if u.spec.id == target_id {
                                continue;
                            }
                            let was = before_status
                                .get(&u.spec.id)
                                .map(String::as_str)
                                .unwrap_or("pending");
                            if u.status == "pending" && was != "pending" {
                                retry_reopened_after_deliver
                                    .push((u.spec.id.clone(), u.reason.clone()));
                            } else if after_deliver.depends_on(u, target_id) {
                                if was == "failed" && u.status == "failed" {
                                    retry_stuck_failed_downstream.push(u.spec.id.clone());
                                } else if was == "succeeded" && u.status == "succeeded" {
                                    retry_preserved_succeeded_downstream.push(u.spec.id.clone());
                                }
                            }
                        }
                    }

                    // === PROBE B: Real #528 `apply_review_findings` + `Workflow::review_scope` ===
                    let mut review_scope_available = false;
                    let mut target_in_review_scope = None;
                    let mut review_has_delivery_receipt = false;
                    let mut apply_findings_receipt = None;
                    let mut apply_findings_reopened: Vec<String> = Vec::new();
                    let mut apply_findings_rejection_gates: Vec<String> = Vec::new();
                    let mut counterfactual_non_project_receipt = None;
                    let mut counterfactual_non_project_reopened: Vec<String> = Vec::new();

                    if has_failed_review {
                        let failed_review = before_graph
                            .units
                            .iter()
                            .find(|u| u.spec.kind == "review" && u.status == "failed");
                        if let Some(review_unit) = failed_review {
                            let scope_opt = before_graph.review_scope(review_unit);
                            review_scope_available = scope_opt.is_some();
                            if *has_project {
                                apply_findings_rejection_gates.push("project_id_not_null".into());
                            }
                            let review_raw_key = last_raw_child_session
                                .get(&review_unit.spec.id)
                                .cloned()
                                .unwrap_or_default();
                            let review_delivs = deliveries_by_item_session
                                .get(&review_raw_key)
                                .cloned()
                                .unwrap_or_default();
                            review_has_delivery_receipt = !review_delivs.is_empty();
                            if !review_has_delivery_receipt {
                                apply_findings_rejection_gates
                                    .push("missing_review_delivery_receipt".into());
                            }
                            let target_is_review = target_id == review_unit.spec.id;
                            if target_is_review {
                                apply_findings_rejection_gates
                                    .push("target_is_review_no_work_finding".into());
                            }
                            if let Some(scope) = scope_opt {
                                if !target_is_review {
                                    let in_scope = scope.units.iter().any(|u| u.unit == target_id);
                                    target_in_review_scope = Some(in_scope);
                                    if !in_scope {
                                        apply_findings_rejection_gates
                                            .push("target_not_in_review_scope".into());
                                    }
                                }
                                // Construct findings faithfully from real check + real review delivery (if any).
                                let findings = if target_is_review {
                                    Vec::new()
                                } else {
                                    let criterion = before_graph
                                        .units
                                        .iter()
                                        .find(|u| u.spec.id == target_id)
                                        .and_then(|u| u.checks.as_array())
                                        .and_then(|cs| cs.first())
                                        .and_then(|c| c.get("name"))
                                        .and_then(Value::as_str)
                                        .unwrap_or("missing_check")
                                        .to_string();
                                    let evidence_ref = review_delivs
                                        .first()
                                        .map(|d| d.url_hash.clone())
                                        .unwrap_or_else(|| "/d/missing".into());
                                    vec![Finding {
                                        affected_unit: target_id.to_string(),
                                        criterion,
                                        evidence_ref,
                                    }]
                                };

                                // Direct call to main's `apply_review_findings`.
                                let (_probe_b_dir, probe_b) = clone_store(&base_store);
                                let receipt = probe_b
                                    .workflows()
                                    .apply_review_findings(&parent, &scope, &findings, "replay")
                                    .expect("probe_b apply_review_findings");
                                apply_findings_receipt = Some(format!("{receipt:?}"));
                                if receipt == ReviewReceipt::Applied {
                                    let after_apply = probe_b
                                        .workflows()
                                        .get(&parent)
                                        .expect("after apply")
                                        .expect("graph");
                                    apply_findings_reopened = after_apply
                                        .units
                                        .iter()
                                        .filter(|u| {
                                            u.status == "pending"
                                                && before_status.get(&u.spec.id).map(String::as_str)
                                                    != Some("pending")
                                        })
                                        .map(|u| u.spec.id.clone())
                                        .collect();
                                }

                                // Counterfactual Probe B2: clear project_id + seed a synthetic review
                                // delivery receipt to see what `apply_review_findings` *would* reopen
                                // if Git projects & action=fail delivery receipts were supported.
                                if !target_is_review {
                                    let (_cf_dir, cf_store) = clone_store(&base_store);
                                    cf_store
                                        .items()
                                        .patch(
                                            &parent,
                                            &ItemPatch {
                                                project_id: Some(None),
                                                ..Default::default()
                                            },
                                        )
                                        .expect("clear project_id");
                                    let synth_url = format!("/d/synth-review-{}", ev.revision);
                                    if let (Some(r_child), Some(r_sess)) = (
                                        review_unit.item_id.as_deref(),
                                        review_unit.session_id.as_deref(),
                                    ) {
                                        seed_deliveries(
                                            &cf_store,
                                            r_child,
                                            r_sess,
                                            &[RawDeliveryMeta {
                                                url_hash: synth_url.clone(),
                                                sha256: sha256_hex(&synth_url),
                                                mime: "text/plain".into(),
                                                size: 16,
                                            }],
                                            &format!("cf-{wf_id}-r{}", ev.revision),
                                        );
                                    }
                                    let mut cf_findings = findings.clone();
                                    cf_findings[0].evidence_ref = synth_url;
                                    match cf_store.workflows().apply_review_findings(
                                        &parent,
                                        &scope,
                                        &cf_findings,
                                        "replay",
                                    ) {
                                        Ok(cf_receipt) => {
                                            counterfactual_non_project_receipt =
                                                Some(format!("{cf_receipt:?}"));
                                            if cf_receipt == ReviewReceipt::Applied {
                                                let after_cf = cf_store
                                                    .workflows()
                                                    .get(&parent)
                                                    .expect("after cf")
                                                    .expect("graph");
                                                counterfactual_non_project_reopened = after_cf
                                                    .units
                                                    .iter()
                                                    .filter(|u| {
                                                        u.status == "pending"
                                                            && before_status
                                                                .get(&u.spec.id)
                                                                .map(String::as_str)
                                                                != Some("pending")
                                                    })
                                                    .map(|u| u.spec.id.clone())
                                                    .collect();
                                            }
                                        }
                                        Err(e) => {
                                            counterfactual_non_project_receipt =
                                                Some(format!("Err({e})"));
                                        }
                                    }
                                }
                            } else {
                                apply_findings_rejection_gates.push("review_scope_none".into());
                                apply_findings_receipt = Some("ScopeNone".into());
                            }
                        }
                    }

                    let retry_reopened_total_non_pending =
                        retry_reopened_immediate.len() + retry_reopened_after_deliver.len();
                    let event_idx = all_events.len();
                    all_events.push(ResetReplayEvent {
                        workflow_id: wf_id.clone(),
                        reset_revision: ev.revision,
                        has_project: *has_project,
                        target_unit: target_id.to_string(),
                        has_failed_review,
                        paired_sibling_after_failed_review: paired_sibling,
                        old_invalidated_total: old_units.len(),
                        old_invalidated_non_pending,
                        old_units,
                        retry_reopened_immediate,
                        retry_reopened_after_deliver,
                        retry_reopened_total_non_pending,
                        retry_stuck_failed_downstream,
                        retry_preserved_succeeded_downstream,
                        retry_blocked_following_replan,
                        review_scope_available,
                        target_in_review_scope,
                        review_has_delivery_receipt,
                        apply_findings_receipt,
                        apply_findings_reopened,
                        apply_findings_rejection_gates,
                        counterfactual_non_project_receipt,
                        counterfactual_non_project_reopened,
                        linked_turns: Vec::new(),
                    });

                    // Record last reset for each unit in historical invalidated[] for turn linkage.
                    for u in inv {
                        let uid = u["id"].as_str().unwrap_or("").to_string();
                        let kind = u["kind"].as_str().unwrap_or("work").to_string();
                        let st = u["status"].as_str().unwrap_or("pending").to_string();
                        let old_goal_hash =
                            format!("goal:{}", sha256_hex(u["goal"].as_str().unwrap_or("")));
                        let is_tgt = uid == target_id;
                        last_reset_event_for_unit
                            .insert(uid, (event_idx, kind, st, old_goal_hash, is_tgt));
                    }

                    // Advance the baseline Store to match the exact historical post-reset state
                    // so subsequent events in this workflow start from true historical state.
                    advance_baseline_reset(&base_store, &parent, inv);
                    prev_was_failed_review_reset = has_failed_review;
                }
                _ => {
                    prev_was_failed_review_reset = false;
                }
            }
        }
    }

    summarize(
        day_from,
        day_to,
        agent_total,
        &workflows_meta,
        transitive_review_workflows,
        all_events,
    )
}

/// Reproduces the historical `reset` outcome on the baseline Store using `retry_unit` so later
/// events in the same workflow replay from the exact pre-event state observed in production.
fn advance_baseline_reset(store: &Store, parent: &str, invalidated: &[Value]) {
    let graph: Workflow = store
        .workflows()
        .get(parent)
        .expect("get graph")
        .expect("graph");
    // Reopen leaf-first (reverse graph order) so no active dependent check could trip.
    let inv_ids: HashSet<&str> = invalidated
        .iter()
        .filter_map(|u| u["id"].as_str())
        .collect();
    for u in graph.units.iter().rev() {
        if inv_ids.contains(u.spec.id.as_str()) && u.status != "pending" {
            let rev = store
                .workflows()
                .get(parent)
                .expect("get graph")
                .expect("graph")
                .revision;
            store
                .workflows()
                .retry_unit(parent, rev, &u.spec.id, "baseline-advance")
                .expect("baseline advance retry_unit");
        }
    }
}

fn summarize(
    day_from: &str,
    day_to: &str,
    agent_total: TokenTotals,
    workflows_meta: &[(String, bool)],
    transitive_review_workflows: Vec<String>,
    events: Vec<ResetReplayEvent>,
) -> ReplaySummary {
    let mut s = ReplaySummary {
        day_from: day_from.to_string(),
        day_to: day_to.to_string(),
        agent_total,
        workflows_total: workflows_meta.len(),
        workflows_with_project: workflows_meta.iter().filter(|(_, p)| *p).count(),
        resets_total: events.len(),
        review_failed_resets: 0,
        review_failed_old_invalidated_total: 0,
        review_failed_old_invalidated_non_pending: 0,
        review_failed_retry_reopened_immediate: 0,
        review_failed_retry_reopened_propagated: 0,
        review_failed_retry_reopened_total: 0,
        review_failed_retry_stuck_failed_downstream: 0,
        review_failed_retry_preserved_succeeded_downstream: 0,
        review_failed_retry_blocked_replan_count: 0,
        review_failed_apply_findings_applied: 0,
        review_failed_apply_findings_unverified: 0,
        review_failed_apply_findings_reopened_units: 0,
        review_failed_apply_findings_gates: BTreeMap::new(),
        review_failed_turns_total: TokenTotals::default(),
        review_failed_prev_succeeded_turns: TokenTotals::default(),
        review_failed_prev_succeeded_same_goal_turns: TokenTotals::default(),
        review_failed_by_category: BTreeMap::new(),
        review_failed_retry_reopened_tokens: TokenTotals::default(),
        review_failed_retry_stuck_failed_tokens: TokenTotals::default(),
        review_failed_pending_first_run_tokens: TokenTotals::default(),
        review_failed_genuinely_avoidable_tokens: TokenTotals::default(),
        review_failed_counterfactual_collateral_ceiling: TokenTotals::default(),
        paired_sibling_resets: 0,
        paired_sibling_tokens: TokenTotals::default(),
        non_review_collateral_succeeded_resets: 0,
        non_review_collateral_succeeded_tokens: TokenTotals::default(),
        non_review_stuck_failed_downstream: 0,
        non_review_stuck_failed_tokens: TokenTotals::default(),
        transitive_review_workflows,
        events: Vec::new(),
    };

    for ev in &events {
        if ev.has_failed_review {
            s.review_failed_resets += 1;
            s.review_failed_old_invalidated_total += ev.old_invalidated_total;
            s.review_failed_old_invalidated_non_pending += ev.old_invalidated_non_pending;
            s.review_failed_retry_reopened_immediate += ev.retry_reopened_immediate.len();
            s.review_failed_retry_reopened_propagated += ev.retry_reopened_after_deliver.len();
            s.review_failed_retry_reopened_total += ev.retry_reopened_total_non_pending;
            s.review_failed_retry_stuck_failed_downstream += ev.retry_stuck_failed_downstream.len();
            s.review_failed_retry_preserved_succeeded_downstream +=
                ev.retry_preserved_succeeded_downstream.len();
            if ev.retry_blocked_following_replan {
                s.review_failed_retry_blocked_replan_count += 1;
            }
            if ev.apply_findings_receipt.as_deref() == Some("Applied") {
                s.review_failed_apply_findings_applied += 1;
                s.review_failed_apply_findings_reopened_units += ev.apply_findings_reopened.len();
            } else {
                s.review_failed_apply_findings_unverified += 1;
            }
            for gate in &ev.apply_findings_rejection_gates {
                *s.review_failed_apply_findings_gates
                    .entry(gate.clone())
                    .or_default() += 1;
            }
            for t in &ev.linked_turns {
                s.review_failed_turns_total.add(&t.tokens);
                if t.old_status == "succeeded" {
                    s.review_failed_prev_succeeded_turns.add(&t.tokens);
                    if t.same_goal {
                        s.review_failed_prev_succeeded_same_goal_turns
                            .add(&t.tokens);
                    }
                }
                let cat = match (
                    t.is_target,
                    t.kind.as_str(),
                    t.old_status.as_str(),
                    t.same_goal,
                ) {
                    (true, "work", "succeeded", false) => "target_work_succeeded_goal_changed",
                    (true, "work", "succeeded", true) => "target_work_succeeded_goal_unchanged",
                    (true, "review", "failed", _) => "target_review_failed",
                    (false, "work", "succeeded", _) => "downstream_work_succeeded_collateral",
                    (false, "review", "failed", _) => "downstream_review_failed",
                    (false, "integrate", "pending", _) => "downstream_integrate_pending_first_run",
                    _ => "other",
                };
                s.review_failed_by_category
                    .entry(cat.to_string())
                    .or_default()
                    .add(&t.tokens);
                if t.old_status == "pending" {
                    s.review_failed_pending_first_run_tokens.add(&t.tokens);
                } else if t.reopened_by_retry_rule {
                    s.review_failed_retry_reopened_tokens.add(&t.tokens);
                } else if t.stuck_failed_under_retry_rule {
                    s.review_failed_retry_stuck_failed_tokens.add(&t.tokens);
                } else if t.old_status == "succeeded" && !t.is_target {
                    s.review_failed_genuinely_avoidable_tokens.add(&t.tokens);
                }
                if t.old_status == "succeeded" && !t.is_target {
                    s.review_failed_counterfactual_collateral_ceiling
                        .add(&t.tokens);
                }
            }
        } else {
            if ev.paired_sibling_after_failed_review {
                s.paired_sibling_resets += 1;
                for t in &ev.linked_turns {
                    if t.is_target {
                        s.paired_sibling_tokens.add(&t.tokens);
                    }
                }
            }
            let has_collat_succ = ev
                .old_units
                .iter()
                .any(|u| !u.is_target && u.old_status == "succeeded");
            if has_collat_succ {
                s.non_review_collateral_succeeded_resets += 1;
                for t in &ev.linked_turns {
                    if !t.is_target && t.old_status == "succeeded" {
                        s.non_review_collateral_succeeded_tokens.add(&t.tokens);
                    }
                }
            }
            s.non_review_stuck_failed_downstream += ev.retry_stuck_failed_downstream.len();
            for t in &ev.linked_turns {
                if t.stuck_failed_under_retry_rule {
                    s.non_review_stuck_failed_tokens.add(&t.tokens);
                }
            }
        }
    }

    s.events = events;
    s
}

fn fmt_m(n: i64) -> String {
    format!("{:.2}M", (n as f64) / 1_000_000.0)
}

fn print_markdown(s: &ReplaySummary) {
    println!("# #503 Selective Reopen Offline Replay Summary");
    println!();
    println!(
        "- Window: `{} .. {}` (`usage_records.source='agent'`, grouped by `turn_id`)",
        s.day_from, s.day_to
    );
    println!(
        "- Agent window totals: {} turns, cache_read={}, total={}",
        s.agent_total.turns,
        fmt_m(s.agent_total.cache_read),
        fmt_m(s.agent_total.total)
    );
    println!(
        "- Workflows: {} total ({} with `project_id IS NOT NULL`)",
        s.workflows_total, s.workflows_with_project
    );
    println!(
        "- Reset events: {} total ({} containing failed review; {} paired sibling resets immediately after failed review; {} non-review resets with collateral succeeded downstream)",
        s.resets_total,
        s.review_failed_resets,
        s.paired_sibling_resets,
        s.non_review_collateral_succeeded_resets
    );
    println!();
    println!(
        "## Review-Failed Reset Cohort ({} resets)",
        s.review_failed_resets
    );
    println!();
    println!("| Metric | Units / Count | Turns | Cache Read | Total Tokens |");
    println!("|---|---:|---:|---:|---:|");
    println!(
        "| Old `reset` invalidated (all / non-pending) | {} / {} | {} | {} | {} |",
        s.review_failed_old_invalidated_total,
        s.review_failed_old_invalidated_non_pending,
        s.review_failed_turns_total.turns,
        fmt_m(s.review_failed_turns_total.cache_read),
        fmt_m(s.review_failed_turns_total.total)
    );
    println!(
        "|   of which previously succeeded | — | {} | {} | {} |",
        s.review_failed_prev_succeeded_turns.turns,
        fmt_m(s.review_failed_prev_succeeded_turns.cache_read),
        fmt_m(s.review_failed_prev_succeeded_turns.total)
    );
    println!(
        "|   of which goal unchanged | — | {} | {} | {} |",
        s.review_failed_prev_succeeded_same_goal_turns.turns,
        fmt_m(s.review_failed_prev_succeeded_same_goal_turns.cache_read),
        fmt_m(s.review_failed_prev_succeeded_same_goal_turns.total)
    );
    println!(
        "| #528 `retry_unit` + `deliver` reopened (immediate + propagated) | {} ({} + {}) | {} | {} | {} |",
        s.review_failed_retry_reopened_total,
        s.review_failed_retry_reopened_immediate,
        s.review_failed_retry_reopened_propagated,
        s.review_failed_retry_reopened_tokens.turns,
        fmt_m(s.review_failed_retry_reopened_tokens.cache_read),
        fmt_m(s.review_failed_retry_reopened_tokens.total)
    );
    println!(
        "| #528 `retry_unit` + `deliver` stuck `failed` downstream (BUG: not savings) | {} | {} | {} | {} |",
        s.review_failed_retry_stuck_failed_downstream,
        s.review_failed_retry_stuck_failed_tokens.turns,
        fmt_m(s.review_failed_retry_stuck_failed_tokens.cache_read),
        fmt_m(s.review_failed_retry_stuck_failed_tokens.total)
    );
    println!(
        "| Pending `integrate` (attempt=0, first-time run, not rework) | {} | {} | {} | {} |",
        s.review_failed_old_invalidated_total - s.review_failed_old_invalidated_non_pending,
        s.review_failed_pending_first_run_tokens.turns,
        fmt_m(s.review_failed_pending_first_run_tokens.cache_read),
        fmt_m(s.review_failed_pending_first_run_tokens.total)
    );
    println!(
        "| #528 genuinely avoided (preserved `succeeded` downstream) | {} | {} | {} | {} |",
        s.review_failed_retry_preserved_succeeded_downstream,
        s.review_failed_genuinely_avoidable_tokens.turns,
        fmt_m(s.review_failed_genuinely_avoidable_tokens.cache_read),
        fmt_m(s.review_failed_genuinely_avoidable_tokens.total)
    );
    println!(
        "| Counterfactual ceiling (all collateral `succeeded` downstream) | — | {} | {} | {} |",
        s.review_failed_counterfactual_collateral_ceiling.turns,
        fmt_m(s.review_failed_counterfactual_collateral_ceiling.cache_read),
        fmt_m(s.review_failed_counterfactual_collateral_ceiling.total)
    );
    println!(
        "| #528 `apply_review_findings` applied / unverified | {} / {} (units={}) | — | — | — |",
        s.review_failed_apply_findings_applied,
        s.review_failed_apply_findings_unverified,
        s.review_failed_apply_findings_reopened_units
    );
    println!();
    println!("## Breakdown by Category");
    println!();
    println!("| Category | Turns | Cache Read | Total Tokens |");
    println!("|---|---:|---:|---:|");
    for (cat, tok) in &s.review_failed_by_category {
        println!(
            "| `{cat}` | {} | {} | {} |",
            tok.turns,
            fmt_m(tok.cache_read),
            fmt_m(tok.total)
        );
    }
    println!();
    println!("## `apply_review_findings` Rejection Gates (non-exclusive)");
    for (gate, n) in &s.review_failed_apply_findings_gates {
        println!("- `{gate}`: {n} / {}", s.review_failed_resets);
    }
    println!(
        "- Following `replan` blocked under deferred downstream reopen: {} / {}",
        s.review_failed_retry_blocked_replan_count, s.review_failed_resets
    );
    println!(
        "- Workflows with transitive review coverage (`work -> work -> review`): {:?}",
        s.transitive_review_workflows
    );
    println!(
        "- Paired sibling resets right after failed review: {} resets, {} turns, cache_read={}, total={}",
        s.paired_sibling_resets,
        s.paired_sibling_tokens.turns,
        fmt_m(s.paired_sibling_tokens.cache_read),
        fmt_m(s.paired_sibling_tokens.total)
    );
    println!(
        "- Non-review resets with collateral succeeded downstream: {} resets, {} collateral turns, cache_read={}, total={}",
        s.non_review_collateral_succeeded_resets,
        s.non_review_collateral_succeeded_tokens.turns,
        fmt_m(s.non_review_collateral_succeeded_tokens.cache_read),
        fmt_m(s.non_review_collateral_succeeded_tokens.total)
    );
    println!(
        "- Non-review resets with stuck `failed` downstream under #528: {} units, {} turns, cache_read={}, total={}",
        s.non_review_stuck_failed_downstream,
        s.non_review_stuck_failed_tokens.turns,
        fmt_m(s.non_review_stuck_failed_tokens.cache_read),
        fmt_m(s.non_review_stuck_failed_tokens.total)
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut snapshot: Option<PathBuf> = None;
    let mut day_from = "2026-11-11".to_string();
    let mut day_to = "2026-11-22".to_string();
    let mut as_json = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--from" => day_from = args.next().expect("--from requires YYYY-MM-DD"),
            "--to" => day_to = args.next().expect("--to requires YYYY-MM-DD"),
            "--json" => as_json = true,
            "--help" | "-h" => {
                eprintln!(
                    "Usage: workflow_token_replay <snapshot.sqlite3> [--from YYYY-MM-DD] [--to YYYY-MM-DD] [--json]"
                );
                return;
            }
            other if snapshot.is_none() => snapshot = Some(PathBuf::from(other)),
            other => panic!("unexpected argument: {other}"),
        }
    }
    let snapshot = snapshot.expect("path to read-only SQLite snapshot is required");
    let summary = replay_snapshot(&snapshot, &day_from, &day_to);
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).expect("serialize summary")
        );
    } else {
        print_markdown(&summary);
    }
}
