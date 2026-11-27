//! Read-only device observations. Never use the Hub's merged model catalogue to describe a
//! particular machine. Calls use the target Node's existing probe/cache, without changing its
//! provider or login. Cached failures retain the last success for display, never for admission.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use steve_modules::Caller;
use steve_node_api::{
    AdapterStatus, Code, Command, ComponentStatus, GrantId, HarnessCapability, HarnessSetup,
    InstallHarness, MissingCommandKind, Node, NodeId, OperationId, SetupState, Timestamp,
};
use steve_store::{Read, UserRows, node_directory};
use steve_types::devices::{
    HarnessDegradedReason, InstallNodeHarnessResponse, NodeHarnessComponentStatus,
    NodeHarnessComponentView, NodeHarnessDetectedView, NodeHarnessRefreshResponse,
    NodeHarnessReportState, NodeHarnessReportView, NodeHarnessSetupState, NodeHarnessSetupView,
    NodeHarnessView, NodeHarnessesResponse, NodeReason,
};

use super::AppState;
use crate::error::ApiError;

const MAX_AGE: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(45);
/// Acceptance window for an owner-initiated harness adapter installation command.
const INSTALL_ACCEPT_WINDOW_MS: i64 = 30_000;
/// Timeout for submitting `harness.install` to a Node and receiving its initial receipt.
const INSTALL_RPC_TIMEOUT: Duration = Duration::from_secs(15);

pub type Report = NodeHarnessReportView;

#[derive(Clone)]
struct ProbedReport {
    report: Report,
    connection: Option<u64>,
    fresh: bool,
}
pub struct Inventory {
    cache: steve_runtime::cache::Cached<String, ProbedReport>,
    caches: Arc<steve_runtime::cache::Caches>,
}
static INVENTORY_SPACE: steve_runtime::cache::CacheSpace<String, ProbedReport> =
    steve_runtime::cache::CacheSpace {
        name: "node-harnesses",
        ttl: MAX_AGE,
        // Preserve the last report during a slow/offline refresh; never use it for admission.
        stale_for: Duration::from_secs(300),
        share_pct: 10,
        tags: |key| {
            vec![
                steve_runtime::cache::Tag::of("node", key),
                steve_runtime::cache::Tag::of("node-inventory", key),
            ]
        },
        weigh: |value| {
            serde_json::to_vec(&value.report).map_or(1, |b| b.len().min(u32::MAX as usize) as u32)
        },
    };
impl Inventory {
    pub fn new(caches: Arc<steve_runtime::cache::Caches>) -> Self {
        Self {
            cache: caches.space(&INVENTORY_SPACE),
            caches,
        }
    }

    pub fn invalidate(&self, node: &str) {
        let tag = steve_runtime::cache::Tag::of("node-inventory", node);
        self.caches.invalidate(std::slice::from_ref(&tag));
    }
}

struct Target {
    id: String,
    node: Option<Arc<dyn Node>>,
    // A remote link is checked again after awaiting the probe, before publishing its answer.
    remote: Option<Arc<steve_node_wire::RemoteNode>>,
    generation: Option<u64>,
    upgrade_only: bool,
}

impl Target {
    fn current(&self) -> bool {
        self.remote
            .as_ref()
            .is_none_or(|n| n.generation() == self.generation)
    }
}

fn setup_view(s: HarnessSetup) -> NodeHarnessSetupView {
    NodeHarnessSetupView {
        state: match s.state {
            SetupState::NotInstalled => NodeHarnessSetupState::NotInstalled,
            SetupState::Installing => NodeHarnessSetupState::Installing,
            SetupState::Ready => NodeHarnessSetupState::Ready,
            SetupState::Unavailable => NodeHarnessSetupState::Unavailable,
            SetupState::Unknown => NodeHarnessSetupState::Unknown,
        },
        components: s
            .components
            .into_iter()
            .map(|comp| NodeHarnessComponentView {
                id: comp.id,
                name: comp.name,
                package: comp.package,
                version: comp.version,
                path: comp.path,
                status: match comp.status {
                    ComponentStatus::Installed => NodeHarnessComponentStatus::Installed,
                    ComponentStatus::Missing => NodeHarnessComponentStatus::Missing,
                    ComponentStatus::Unknown => NodeHarnessComponentStatus::Unknown,
                },
                detected: comp.detected.map(|d| NodeHarnessDetectedView {
                    path: d.path,
                    version: d.version,
                }),
            })
            .collect(),
        can_install: s.can_install,
        blocked_reason: s.blocked_reason,
        reason: s.reason,
        help: s.help,
        error: s.error,
    }
}

fn component_outdated(id: &str, setup: &HarnessSetup) -> bool {
    steve_harness_spec::components(id).iter().any(|spec| {
        let Some(min) = spec.minimum else {
            return false;
        };
        let Ok(min_ver) = semver::Version::parse(min) else {
            return false;
        };
        setup.components.iter().any(|comp| {
            comp.id == spec.id
                && comp
                    .version
                    .as_deref()
                    .and_then(|v| semver::Version::parse(v.trim_start_matches('v')).ok())
                    .is_some_and(|v| v < min_ver)
        })
    })
}

fn harness(c: HarnessCapability) -> NodeHarnessView {
    use HarnessDegradedReason::*;
    let a = c.adapter.as_ref();
    let s = c.setup.as_ref();
    let auth = a.is_some_and(|a| a.session_auth_required == Some(true));
    let missing_kind = a.and_then(|a| a.missing_command.as_ref().map(|m| m.kind));
    let adapter_reason = a.and_then(|a| {
        if a.session_auth_required == Some(true) {
            return Some(AuthenticationRequired);
        }
        match a.status {
            AdapterStatus::AdapterMissing => match missing_kind {
                Some(MissingCommandKind::CommandNotInServicePath) => Some(CommandNotInServicePath),
                Some(MissingCommandKind::AdapterPackageMissing) => Some(AdapterPackageMissing),
                Some(MissingCommandKind::BothMissing) | None => Some(AdapterMissing),
            },
            AdapterStatus::InitializeFailed => Some(InitializeFailed),
            AdapterStatus::Ready if a.session.is_none() => Some(SessionUnavailable),
            _ => None,
        }
    });
    let has_missing_diag_presence = matches!(
        missing_kind,
        Some(MissingCommandKind::CommandNotInServicePath)
            | Some(MissingCommandKind::AdapterPackageMissing)
    );
    let has_partial_install = !c.version.is_empty()
        || has_missing_diag_presence
        || s.is_some_and(|s| {
            s.components
                .iter()
                .any(|comp| comp.installed() || comp.detected.is_some())
        });
    let available = if auth {
        Some(false)
    } else if s.is_some_and(|s| s.state == SetupState::Unknown) {
        None
    } else if s.is_some_and(|s| s.state != SetupState::Ready) {
        Some(false)
    } else {
        // A declared capability is not a probe: without an adapter report readiness is unknown.
        a.map(|a| a.status == AdapterStatus::Ready && a.session.is_some())
    };
    let installed = if available == Some(true) {
        true
    } else if let Some(s) = s {
        match s.state {
            SetupState::NotInstalled => has_partial_install || s.error.is_some(),
            SetupState::Installing
            | SetupState::Ready
            | SetupState::Unavailable
            | SetupState::Unknown => true,
        }
    } else if let Some(a) = a {
        match a.status {
            AdapterStatus::AdapterMissing => {
                !a.builtin || !c.version.is_empty() || has_missing_diag_presence
            }
            AdapterStatus::Ready | AdapterStatus::InitializeFailed => true,
        }
    } else {
        c.sessions || !c.version.is_empty()
    };
    let degraded_reason = if available == Some(true) {
        None
    } else if auth {
        Some(AuthenticationRequired)
    } else if !installed {
        None
    } else if let Some(s) = s {
        match s.state {
            SetupState::Installing => Some(Installing),
            SetupState::Unknown => Some(ProbeUnknown),
            SetupState::NotInstalled => adapter_reason.or(Some(AdapterMissing)),
            SetupState::Unavailable => {
                if s.error.is_some() {
                    Some(InstallFailed)
                } else if component_outdated(&c.name, s) {
                    Some(VersionOutdated)
                } else if let Some(r) = adapter_reason {
                    Some(r)
                } else {
                    Some(RuntimeIncompatible)
                }
            }
            SetupState::Ready => adapter_reason.or(Some(ProbeUnknown)),
        }
    } else {
        adapter_reason.or_else(|| available.is_none().then_some(ProbeUnknown))
    };
    let session = a.and_then(|a| a.session.as_ref());
    let default_model = session.and_then(|s| s.default_model.clone());
    let models: Vec<String> = match session {
        Some(s) => s.models.iter().map(|m| m.id.clone()).collect(),
        None => c.models,
    };
    let install_hint = a
        .and_then(|a| a.install_hint.clone())
        .filter(|hint| !hint.trim().is_empty())
        .or_else(|| {
            steve_harness_spec::builtin(&c.name)
                .map(|b| b.install_hint.to_owned())
                .filter(|hint| !hint.trim().is_empty())
        });
    NodeHarnessView {
        name: a
            .map(|a| a.title.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&c.name)
            .to_owned(),
        id: c.name,
        available,
        version: c.version,
        adapter_version: a.and_then(|a| a.version.clone()),
        default_model,
        models,
        degraded_reason,
        install_hint,
        installed,
        // The authentication code is enough. Adapter text can include account data.
        error: if auth {
            None
        } else {
            a.and_then(|a| a.session_error.clone().or_else(|| a.error.clone()))
        },
        missing_command: a.and_then(|a| a.missing_command.clone()),
        setup: c.setup.map(setup_view),
        capabilities: a.and_then(|a| a.capabilities.clone()),
    }
}

impl Inventory {
    fn connection(t: &Target) -> Option<u64> {
        t.remote.as_ref().map_or(t.generation, |n| n.generation())
    }
    async fn view(&self, t: &Target) -> Report {
        let cached = self.cache.peek(&t.id).await;
        let (mut report, current) = cached.map_or_else(
            || (Report::default(), false),
            |(v, current)| {
                (
                    v.report.clone(),
                    current && v.connection == Self::connection(t),
                )
            },
        );
        let linked = t.node.is_some() && t.current();
        if t.upgrade_only && linked {
            report.state = NodeHarnessReportState::UpgradeRequired;
            report.error = None;
            report.refreshing = false;
            report.stale = report.checked_at.is_some();
            return report;
        }
        report.stale = report.checked_at.is_some()
            && (!linked || !current || report.state != NodeHarnessReportState::Reported);
        if !linked {
            report.state = NodeHarnessReportState::Offline;
        } else if !current {
            report.state = NodeHarnessReportState::Unreported;
        }
        report
    }
    async fn refresh(&self, t: &Target, fresh: bool) -> Report {
        if t.upgrade_only {
            return self.view(t).await;
        }
        let Some(node) = &t.node else {
            return self.view(t).await;
        };
        let old = self.cache.peek(&t.id).await.map(|(v, _)| v);
        let tag = steve_runtime::cache::Tag::of("node-inventory", &t.id);
        if old
            .as_ref()
            .is_some_and(|v| fresh || v.connection != t.generation)
        {
            self.caches.invalidate(std::slice::from_ref(&tag));
        }
        // A manual refresh that joined automatic discovery gets one genuinely fresh probe.
        // This is request semantics, not a second cache or an unbounded publication loop.
        for attempt in 0..2 {
            let result = self
                .cache
                .get_or_load(t.id.clone(), || async {
                    let mut report = old.as_ref().map(|v| v.report.clone()).unwrap_or_default();
                    let result = tokio::time::timeout(PROBE_TIMEOUT, node.probe(fresh)).await;
                    if !t.current() {
                        report.state = NodeHarnessReportState::Offline;
                        report.stale = report.checked_at.is_some();
                        return Ok::<_, ()>(ProbedReport {
                            report,
                            connection: t.generation,
                            fresh,
                        });
                    }
                    report.attempted_at = Some(steve_store::now());
                    report.refreshing = false;
                    let (state, error) = match result {
                        Ok(Ok(c)) if c.node.as_str() == t.id => {
                            report.checked_at = report.attempted_at;
                            report.exec_allowed = Some(c.capacity.execs > 0);
                            report.harnesses = c.harnesses.into_iter().map(harness).collect();
                            (NodeHarnessReportState::Reported, None)
                        }
                        Ok(Ok(_)) => (
                            NodeHarnessReportState::Failed,
                            Some("probe returned a different Node identity".into()),
                        ),
                        Ok(Err(e)) => (
                            if e.code == Code::Unsupported {
                                NodeHarnessReportState::Unsupported
                            } else {
                                NodeHarnessReportState::Failed
                            },
                            Some(e.safe_detail),
                        ),
                        Err(_) => (
                            NodeHarnessReportState::Failed,
                            Some("Node harness probe timed out".into()),
                        ),
                    };
                    report.state = state;
                    report.error = error;
                    report.stale =
                        state != NodeHarnessReportState::Reported && report.checked_at.is_some();
                    Ok(ProbedReport {
                        report,
                        connection: t.generation,
                        fresh,
                    })
                })
                .await
                .expect("inventory loaders return report values");
            if !t.current()
                || (result.connection == t.generation && (!fresh || result.fresh))
                || attempt == 1
            {
                return result.report.clone();
            }
            self.caches.invalidate(std::slice::from_ref(&tag));
        }
        unreachable!("bounded refresh always returns")
    }
}

async fn targets(st: &AppState, caller: Caller) -> Result<Vec<Target>, ApiError> {
    // A colocated service Node is adopted by the fleet too; the in-process Node is not.
    // Fence its reports by connection generation just as for other machines.
    let local_remote = st.node.as_ref().and_then(|runtime| {
        let svc = st.nodes.as_ref()?;
        svc.fleet
            .runtime_for("local")
            .filter(|r| Arc::ptr_eq(r, runtime))
            .map(|_| svc.fleet.remote(&NodeId::new("local").expect("static id")))
    });
    let local_generation = local_remote.as_ref().and_then(|n| n.generation());
    let local_upgrade_only = local_remote.as_ref().is_some_and(|n| n.is_upgrade());
    let mut out = vec![Target {
        id: "local".into(),
        node: st
            .node
            .as_ref()
            .filter(|_| local_remote.is_none() || local_generation.is_some())
            .map(|n| n.node()),
        remote: local_remote,
        generation: local_generation,
        upgrade_only: local_upgrade_only,
    }];
    if let Some(svc) = &st.nodes {
        let store = svc.store.clone();
        let rows = tokio::task::spawn_blocking(move || {
            store.read(|c| steve_store::nodes::list_for_owner(c, &caller.me))
        })
        .await
        .map_err(|_| ApiError::Json(500, "internal".into()))?
        .map_err(|_| ApiError::Json(500, "internal".into()))?;
        for n in rows
            .into_iter()
            .filter(|n| !n.local && (n.state == "active" || n.state == "paused"))
        {
            let id = NodeId::new(n.node_id.clone())
                .map_err(|_| ApiError::Json(500, "invalid Node identity".into()))?;
            let remote = svc.fleet.remote(&id);
            let generation = remote.generation();
            let upgrade_only = remote.is_upgrade();
            out.push(Target {
                id: n.node_id,
                node: generation.map(|_| remote.clone() as Arc<dyn Node>),
                remote: Some(remote),
                generation,
                upgrade_only,
            });
        }
    }
    Ok(out)
}

/// Fast cached list; slow probes run independently per machine. Polling cannot queue duplicate
/// work behind a slow Node. Reconnection changes its generation and bypasses the one-minute TTL.
pub async fn list(
    State(st): State<AppState>,
    caller: Caller,
) -> Result<Json<NodeHarnessesResponse>, ApiError> {
    let targets = targets(&st, caller).await?;
    let mut nodes = BTreeMap::new();
    for t in targets {
        // Without retained entries the list must return the direct observation; a
        // background probe alone would leave every later diagnostic list cold too.
        if st.cfg.cache.mode == steve_runtime::cache::CacheMode::Off {
            nodes.insert(t.id.clone(), st.node_harnesses.refresh(&t, false).await);
            continue;
        }
        let due = !t.upgrade_only
            && t.node.is_some()
            && st
                .node_harnesses
                .cache
                .peek(&t.id)
                .await
                .is_none_or(|(value, current)| !current || value.connection != t.generation);
        let mut report = st.node_harnesses.view(&t).await;
        report.refreshing = due;
        nodes.insert(t.id.clone(), report);
        if due {
            let st = st.clone();
            tokio::spawn(async move {
                st.node_harnesses.refresh(&t, false).await;
            });
        }
    }
    Ok(Json(NodeHarnessesResponse { nodes }))
}

#[derive(Default, Deserialize)]
pub struct Refresh {
    fresh: Option<String>,
}

/// Explicit single-Node refresh; concurrent requests share the result of the in-flight probe.
pub async fn get(
    State(st): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
    Query(q): Query<Refresh>,
) -> Result<Json<NodeHarnessRefreshResponse>, ApiError> {
    let t = targets(&st, caller)
        .await?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| ApiError::Json(404, "Node not found".into()))?;
    let report = st
        .node_harnesses
        .refresh(&t, q.fresh.as_deref() == Some("1"))
        .await;
    Ok(Json(NodeHarnessRefreshResponse {
        node_id: id,
        report,
    }))
}

fn install_authorized(
    db: &mut impl Read,
    me: &str,
    node: &str,
) -> Result<bool, steve_store::StoreError> {
    if UserRows(db).first_owner()?.as_deref() != Some(me) {
        return Ok(false);
    }
    if node == "local" {
        return Ok(true);
    }
    if node_directory::owner_if_connectable(db, node)?.as_deref() != Some(me) {
        return Ok(false);
    }
    Ok(node_directory::link_state(db, node)?.is_some_and(|(_, state)| state == "active"))
}

/// Owner-only installation of a built-in harness's managed ACP adapter components on a specific
/// Node (local or remote). Respects the target Node's local execution policy (`policy.json`
/// `"exec"`) and compiled built-in catalog; accepts no arbitrary package or path parameters.
pub async fn install(
    State(st): State<AppState>,
    caller: Caller,
    Path((id, harness)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<InstallNodeHarnessResponse>, ApiError> {
    if !body.is_empty() {
        let parsed: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|_| ApiError::bad_request("invalid JSON body"))?;
        if parsed != serde_json::json!({}) {
            return Err(ApiError::bad_request(
                "installation takes no package, command or destination parameters",
            ));
        }
    }
    if steve_harness_spec::components(&harness).is_empty() {
        return Err(ApiError::bad_request(
            "only registered built-in harness components can be installed",
        ));
    }
    let store = st
        .modules
        .as_ref()
        .map(|m| m.store.clone())
        .or_else(|| st.nodes.as_ref().map(|n| n.store.clone()))
        .ok_or_else(|| {
            ApiError::Contract(steve_types::Error::new(
                steve_types::Code::Unavailable,
                "no store",
            ))
        })?;
    let (me, node_id) = (caller.me.clone(), id.clone());
    let allowed =
        tokio::task::spawn_blocking(move || store.read(|db| install_authorized(db, &me, &node_id)))
            .await
            .map_err(|_| ApiError::Json(500, "internal".into()))?
            .map_err(|_| ApiError::Json(500, "internal".into()))?;
    if !allowed {
        ::metrics::counter!("steve_hub_harness_installs_total", "outcome" => "rejected")
            .increment(1);
        return Err(ApiError::Contract(steve_types::Error::new(
            steve_types::Code::Forbidden,
            "Only the instance owner may install harness components on active Nodes",
        )));
    }
    let t = targets(&st, caller)
        .await?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| ApiError::Json(404, "Node not found".into()))?;
    if t.upgrade_only {
        ::metrics::counter!("steve_hub_harness_installs_total", "outcome" => "rejected")
            .increment(1);
        return Err(ApiError::Contract(NodeReason::upgrade_required(
            "Node requires upgrade before installing harness components",
        )));
    }
    let Some(node) = t.node else {
        ::metrics::counter!("steve_hub_harness_installs_total", "outcome" => "rejected")
            .increment(1);
        return Err(ApiError::Contract(steve_types::Error::new(
            steve_types::Code::Unavailable,
            "Node is offline; reconnect it before installing harness components",
        )));
    };
    let now = crate::nodes::now();
    let op_id = OperationId::new(format!("install_{}", uuid::Uuid::new_v4().simple()))
        .expect("generated operation id");
    let command = Command {
        id: op_id.clone(),
        issued_at: Timestamp(now),
        accept_before: Timestamp(now + INSTALL_ACCEPT_WINDOW_MS),
        grant: GrantId::new("instance_owner").expect("static grant"),
        body: InstallHarness { harness },
    };
    st.node_harnesses.invalidate(&id);
    if id == "local" {
        st.runtimes.invalidate();
    }
    let receipt = tokio::time::timeout(INSTALL_RPC_TIMEOUT, node.install_harness(command))
        .await
        .map_err(|_| {
            ::metrics::counter!("steve_hub_harness_installs_total", "outcome" => "rejected")
                .increment(1);
            ApiError::Contract(steve_types::Error::new(
                steve_types::Code::Unavailable,
                "Node harness installation request timed out",
            ))
        })?
        .map_err(|e| {
            ::metrics::counter!("steve_hub_harness_installs_total", "outcome" => "rejected")
                .increment(1);
            let code = match e.code {
                Code::Forbidden => steve_types::Code::Forbidden,
                Code::Busy | Code::Conflict => steve_types::Code::Conflict,
                Code::Unavailable | Code::OutcomeUnknown => steve_types::Code::Unavailable,
                _ => steve_types::Code::Invalid,
            };
            let mut contract = steve_types::Error::new(code, e.safe_detail);
            contract.reason = e.reason;
            contract.params = e.params;
            ApiError::Contract(contract)
        })?;
    st.node_harnesses.invalidate(&id);
    if id == "local" {
        st.runtimes.invalidate();
    }
    ::metrics::counter!("steve_hub_harness_installs_total", "outcome" => "accepted").increment(1);
    Ok(Json(InstallNodeHarnessResponse {
        accepted: true,
        operation: receipt.operation.as_str().to_string(),
    }))
}

#[cfg(test)]
mod tests;

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route("/node-harnesses", get(node_harnesses::list))
        .route("/node-harnesses/{id}", get(node_harnesses::get))
        .route(
            "/node-harnesses/{id}/{harness}/install",
            post(node_harnesses::install),
        )
}

pub(crate) const CACHE_SPEC: (&str, u8) = (INVENTORY_SPACE.name, INVENTORY_SPACE.share_pct);

#[cfg(test)]
pub(crate) fn register_cache_spaces(caches: &steve_runtime::cache::Caches) {
    let _ = caches.space(&INVENTORY_SPACE);
}
