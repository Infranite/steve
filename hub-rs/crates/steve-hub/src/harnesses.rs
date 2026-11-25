//! The harness adapters this Hub knows (its configuration and manifest, harness parity S1) with
//! what the local Node's probe last said of each (S4, S8): the list Steve's model settings, the
//! model catalogue, the bot pages and the runtimes list choose from. Refreshed in the background
//! from the Node (whose probe caches its adapters' answers); read without waiting.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use steve_core::harnesses::{Harness, HarnessModel, HarnessSource};
use steve_node_api::{AdapterStatus, HarnessCapability};

/// The pure resolver reports ignored declarations; the Hub owns its operator diagnostics.
pub(crate) fn adapter_spec(
    id: &str,
    declared: Option<&steve_harness_spec::Declaration>,
) -> steve_harness_spec::AdapterSpec {
    if let Some(declared) = declared {
        for key in declared.ignored_mode_keys() {
            tracing::warn!(
                adapter = id,
                "harness adapter: unknown mode {key} in its declared modes"
            );
        }
    }
    steve_harness_spec::spec(id, declared)
}

#[cfg(test)]
#[test]
fn ignored_declared_modes_still_warn_in_the_hub() {
    use std::io::{self, Write};
    use std::sync::Mutex;

    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let output = Arc::new(Mutex::new(Vec::new()));
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || Capture(writer.clone()))
        .finish();
    let declared = serde_json::from_value(serde_json::json!({
        "properties": {"modes": {"plan": "inspect", "plna": "misspelled"}}
    }))
    .unwrap();
    let spec =
        tracing::subscriber::with_default(subscriber, || adapter_spec("own", Some(&declared)));
    assert_eq!(spec.properties.modes.plan, "inspect");
    let log = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(log.contains("unknown mode plna"), "{log}");
    assert!(log.contains("WARN"), "{log}");
}

/// How often the list asks the Nodes again (each Node answers from its own cache).
const REFRESH: Duration = Duration::from_secs(60);
/// Remote probe timeout per Node so an unreachable device never stalls the poll.
const REMOTE_PROBE: Duration = Duration::from_secs(15);

fn cap_ready(c: &HarnessCapability) -> bool {
    if c.setup
        .as_ref()
        .is_some_and(|s| s.state != steve_node_api::SetupState::Ready)
    {
        return false;
    }
    match &c.adapter {
        Some(a) => a.status == AdapterStatus::Ready && a.session.is_some(),
        None => c.sessions,
    }
}

fn merge_cap_lists(
    mut base: Vec<HarnessCapability>,
    incoming: Vec<HarnessCapability>,
) -> Vec<HarnessCapability> {
    for cap in incoming {
        match base.iter_mut().find(|c| c.name == cap.name) {
            Some(cur) => {
                let cur_r = cap_ready(cur);
                let new_r = cap_ready(&cap);
                if (!cur_r && new_r)
                    || (cur.adapter.is_none() && cap.adapter.is_some() && cur_r == new_r)
                {
                    *cur = cap;
                }
            }
            None => base.push(cap),
        }
    }
    base
}

pub struct HubHarnesses {
    ids: Vec<String>,
    declared: BTreeMap<String, steve_harness_spec::Declaration>,
    pub(crate) reports: Arc<crate::system::RuntimeReports>,
}
impl HubHarnesses {
    pub(crate) fn of(
        cfg: &crate::config::Config,
        reports: Arc<crate::system::RuntimeReports>,
    ) -> Arc<Self> {
        Arc::new(Self {
            ids: cfg.harness_ids(),
            declared: cfg.declared_adapters.clone(),
            reports,
        })
    }
    fn report(&self, node: &str) -> Option<Arc<crate::system::RuntimeReport>> {
        self.reports
            .peek(node)
            .filter(|(r, fresh)| *fresh && r.recent())
            .map(|(r, _)| r)
    }
    pub fn list(&self) -> Vec<Harness> {
        let caps = self
            .reports
            .nodes()
            .into_iter()
            .filter_map(|node| self.report(&node))
            .fold(Vec::new(), |caps, report| {
                merge_cap_lists(caps, report.caps.clone())
            });
        let mut out: Vec<_> = self
            .ids
            .iter()
            .map(|id| {
                merge(
                    id,
                    self.declared.get(id),
                    caps.iter().find(|c| &c.name == id),
                )
            })
            .collect();
        for cap in &caps {
            if !self.ids.contains(&cap.name) {
                out.push(merge(&cap.name, self.declared.get(&cap.name), Some(cap)));
            }
        }
        out
    }
    pub fn readiness(&self, linked: &[String]) -> Vec<(String, String, Option<bool>)> {
        let pending = self
            .reports
            .model_node()
            .is_some_and(|node| self.report(node).is_none())
            || linked.iter().any(|node| self.report(node).is_none());
        self.list()
            .into_iter()
            .map(|h| {
                let ready = match h.ready {
                    Some(false) if pending => None,
                    ready => ready,
                };
                (h.id, h.name, ready)
            })
            .collect()
    }
    #[cfg(test)]
    pub(crate) async fn update(&self, caps: Vec<HarnessCapability>) {
        self.reports.bind_model_node("local");
        self.reports.seed("local", caps, None).await;
    }
    pub fn on_update(&self, f: impl Fn() + Send + Sync + 'static) {
        self.reports.on_update(f);
    }
    pub fn source(self: &Arc<Self>) -> HarnessSource {
        let me = self.clone();
        Arc::new(move || me.list())
    }
    pub fn for_node(&self, node: &str) -> Vec<Harness> {
        let report = self.report(node);
        self.list()
            .into_iter()
            .map(|h| {
                let cap = report
                    .as_ref()
                    .and_then(|r| r.caps.iter().find(|c| c.name == h.id));
                let mut h = merge(&h.id, self.declared.get(&h.id), cap);
                if cap.is_none_or(|c| c.adapter.is_none()) {
                    h.ready = None;
                }
                h
            })
            .collect()
    }
    pub(crate) fn target_report(
        &self,
        node: &str,
        generation: Option<u64>,
        harness: &str,
    ) -> Option<(i64, Option<HarnessCapability>)> {
        let report = self
            .report(node)
            .filter(|r| r.link_generation == generation)?;
        Some((
            report.checked_at,
            report.caps.iter().find(|c| c.name == harness).cloned(),
        ))
    }
    pub fn bot_source(
        self: &Arc<Self>,
        store: steve_store::Store,
    ) -> steve_core::models::BotHarnessSource {
        let me = self.clone();
        Arc::new(move |bot| {
            let nodes: std::collections::BTreeSet<String> = match bot {
                Some(id) => store
                    .read(|r| steve_store::bot_admin::harness_nodes(r, id))
                    .unwrap_or_default(),
                None => steve_store::bot_dispatch::DEFAULT_NODES
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            };
            if nodes.is_empty() {
                // Placement is not fixed yet; another Node's offer must not certify this one.
                return me.for_node("");
            }
            let lists: Vec<_> = nodes.into_iter().map(|node| me.for_node(&node)).collect();
            let mut out = me.for_node("");
            for h in &mut out {
                let reported: Vec<_> = lists
                    .iter()
                    .flat_map(|list| list.iter())
                    .filter(|n| n.id == h.id && n.ready == Some(true))
                    .collect();
                if let Some(first) = reported.first() {
                    *h = (*first).clone();
                    h.selection_unknown = reported.len() < lists.len();
                    h.models.retain(|m| {
                        reported
                            .iter()
                            .all(|n| n.models.iter().any(|x| x.id == m.id))
                    });
                    h.efforts
                        .retain(|e| reported.iter().all(|n| n.efforts.contains(e)));
                }
            }
            out
        })
    }

    // Until Cached supports an ordered refresh of a still-fresh entry, revoke
    // before probing. An unconditional same-generation put could restore an older
    // ready report after a newer unavailable answer; get_or_load alone would skip
    // the 60s probe while its 300s entry is fresh.
    pub async fn refresh(&self, node: &dyn steve_node_api::Node, fresh: bool) {
        self.reports.bind_model_node(node.id().as_str());
        let _ = self
            .reports
            .load(node.id().as_str(), Some(node), None, fresh, true)
            .await;
    }
    pub async fn refresh_all(
        &self,
        local: Option<&dyn steve_node_api::Node>,
        svc: Option<&crate::nodes::NodeService>,
        fresh: bool,
    ) {
        let mut linked = std::collections::BTreeSet::new();
        if let Some(local) = local {
            linked.insert(local.id().to_string());
        }
        if let Some(svc) = svc {
            linked.extend(svc.registry.linked());
        }
        self.reports.retain_nodes(&linked);
        if let Some(node) = local {
            self.refresh(node, fresh).await;
        } else if let Some(node) = self.reports.model_node() {
            self.reports.invalidate(node);
        }
        if let Some(svc) = svc {
            let asks = svc
                .registry
                .linked()
                .into_iter()
                .filter_map(|id| {
                    steve_node_api::NodeId::new(id.clone())
                        .ok()
                        .map(|node| (id, svc.fleet.remote(&node)))
                })
                .map(|(id, remote)| async move {
                    let generation = remote.generation();
                    if generation.is_none() {
                        self.reports.invalidate(&id);
                        return;
                    }
                    let result = tokio::time::timeout(
                        REMOTE_PROBE,
                        self.reports
                            .load(&id, Some(remote.as_ref()), generation, fresh, true),
                    )
                    .await;
                    if generation != remote.generation() || result.is_err() {
                        self.reports.invalidate(&id);
                    }
                });
            futures::future::join_all(asks).await;
        }
    }
    pub async fn follow(self: Arc<Self>, node: Arc<dyn steve_node_api::Node>) {
        self.follow_all(Some(node), None).await;
    }
    pub fn follow_task(
        self: Arc<Self>,
        local: Option<Arc<dyn steve_node_api::Node>>,
        svc: Option<Arc<crate::nodes::NodeService>>,
    ) -> steve_runtime::Task {
        steve_runtime::every_after(
            "harnesses.follow",
            Arc::new(steve_runtime::SystemClock),
            REFRESH,
            steve_runtime::FirstRun::Immediate,
            move || {
                let me = self.clone();
                let local = local.clone();
                let svc = svc.clone();
                async move {
                    me.refresh_all(local.as_deref(), svc.as_deref(), false)
                        .await;
                }
            },
        )
    }
    pub async fn follow_all(
        self: Arc<Self>,
        local: Option<Arc<dyn steve_node_api::Node>>,
        svc: Option<Arc<crate::nodes::NodeService>>,
    ) {
        self.follow_task(local, svc).await;
    }
}

/// An adapter of one's own as the runtimes page declares it.
/// Custom declarations never inject environment variables; provider sign-in belongs to the harness
/// on its Node and user variables belong to `node_env`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewHarness {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Declared properties ([`steve_harness_spec::PropertiesPatch`]), checked key by key.
    #[serde(default)]
    pub properties: Option<serde_json::Value>,
}

/// Strongly typed failure for custom harness manifest mutations (`POST/DELETE /api/system/harnesses`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HarnessError {
    #[error("custom harness manifest does not accept environment variables")]
    EnvNotAllowed,
    #[error(
        "built-in harness `{0}` cannot be overridden or removed through the custom harness API"
    )]
    BuiltinProtected(String),
    #[error("invalid harness id: {0}")]
    InvalidId(String),
    #[error("invalid harness command")]
    InvalidCommand,
    #[error("invalid harness properties: {0}")]
    InvalidProperties(String),
    #[error("unknown harness property: {0}")]
    UnknownProperty(String),
    #[error("invalid harness declaration payload: {0}")]
    InvalidPayload(String),
    #[error("could not read harness manifest: {0}")]
    ManifestRead(String),
    #[error("could not write harness manifest: {0}")]
    ManifestWrite(String),
}

impl HarnessError {
    /// Bounded reason label for `steve_security_refusals_total`.
    pub fn refusal_reason(&self) -> Option<&'static str> {
        Some(match self {
            Self::EnvNotAllowed => "env_not_allowed",
            Self::BuiltinProtected(_) => "builtin_override",
            Self::InvalidId(_) => "invalid_id",
            Self::InvalidCommand => "invalid_command",
            Self::InvalidProperties(_) => "invalid_properties",
            Self::UnknownProperty(_) => "unknown_property",
            Self::InvalidPayload(_) => "invalid_payload",
            Self::ManifestRead(_) | Self::ManifestWrite(_) => return None,
        })
    }

    pub fn into_api_error(self) -> crate::error::ApiError {
        use steve_types::ConfigReason;
        match self {
            Self::EnvNotAllowed => {
                crate::error::ApiError::Contract(ConfigReason::harness_env_not_allowed(
                    "custom harness declarations do not accept environment variables",
                ))
            }
            Self::BuiltinProtected(id) => {
                crate::error::ApiError::Contract(ConfigReason::harness_builtin_protected(
                    id.clone(),
                    format!("built-in harness \"{id}\" cannot be overridden or removed"),
                ))
            }
            Self::InvalidId(id) => {
                crate::error::ApiError::bad_request(steve_i18n::t!("hub.harness.bad_id", id = id))
            }
            Self::InvalidCommand => {
                crate::error::ApiError::bad_request(steve_i18n::t!("hub.harness.bad_command"))
            }
            Self::InvalidProperties(error) => crate::error::ApiError::bad_request(steve_i18n::t!(
                "hub.harness.bad_properties",
                error = error
            )),
            Self::UnknownProperty(key) => crate::error::ApiError::bad_request(steve_i18n::t!(
                "hub.harness.unknown_property",
                key = key
            )),
            Self::InvalidPayload(msg) => crate::error::ApiError::bad_request(msg),
            Self::ManifestRead(error) => crate::error::ApiError::bad_request(steve_i18n::t!(
                "hub.harness.manifest_unreadable",
                error = error
            )),
            Self::ManifestWrite(error) => crate::error::ApiError::bad_request(steve_i18n::t!(
                "hub.harness.manifest_unwritable",
                error = error
            )),
        }
    }
}

fn record_refusal(err: &HarnessError) {
    if let Some(reason) = err.refusal_reason() {
        metrics::counter!(
            "steve_security_refusals_total",
            "boundary" => "harness_manifest",
            "reason" => reason
        )
        .increment(1);
    }
}

/// Parses a `POST /api/system/harnesses` payload, explicitly refusing any `env` injection attempt
/// with a typed error and metric before deserializing [`NewHarness`].
pub fn parse_new_harness(raw: serde_json::Value) -> Result<NewHarness, HarnessError> {
    if raw.as_object().is_some_and(|obj| obj.contains_key("env")) {
        let err = HarnessError::EnvNotAllowed;
        record_refusal(&err);
        return Err(err);
    }
    serde_json::from_value(raw).map_err(|e| {
        let err = HarnessError::InvalidPayload(e.to_string());
        record_refusal(&err);
        err
    })
}

/// Adds (or replaces) a custom adapter `h` in the Hub's harness manifest at `path`, keeping every
/// other entry; in force once the Hub restarts. Built-in harness ids cannot be overridden here.
pub fn declare(path: &std::path::Path, h: NewHarness) -> Result<(), HarnessError> {
    let id_ok = !h.id.is_empty()
        && h.id.len() <= 64
        && h.id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && h.id.starts_with(|c: char| c.is_ascii_alphanumeric());
    if !id_ok {
        let err = HarnessError::InvalidId(h.id);
        record_refusal(&err);
        return Err(err);
    }
    if steve_harness_spec::builtin(&h.id).is_some() {
        let err = HarnessError::BuiltinProtected(h.id);
        record_refusal(&err);
        return Err(err);
    }
    let command = h.command.trim();
    if command.is_empty() || command.contains('\n') {
        let err = HarnessError::InvalidCommand;
        record_refusal(&err);
        return Err(err);
    }
    if let Some(p) = &h.properties {
        let patch: steve_harness_spec::PropertiesPatch = serde_json::from_value(p.clone())
            .map_err(|e| {
                let err = HarnessError::InvalidProperties(e.to_string());
                record_refusal(&err);
                err
            })?;
        let _ = patch;
        // a key the adapter layer does not know is a mistake, not something to drop silently
        let given = p.as_object().ok_or_else(|| {
            let err = HarnessError::InvalidProperties("not an object".into());
            record_refusal(&err);
            err
        })?;
        if let Some(k) = given
            .keys()
            .find(|k| !steve_harness_spec::PROPERTY_KEYS.contains(&k.as_str()))
        {
            let err = HarnessError::UnknownProperty(k.clone());
            record_refusal(&err);
            return Err(err);
        }
    }
    let mut entry = serde_json::json!({ "command": command });
    if let Some(n) = h.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        entry["name"] = n.into();
    }
    if !h.args.is_empty() {
        entry["args"] = serde_json::json!(h.args);
    }
    if let Some(p) = h.properties {
        entry["properties"] = p;
    }
    edit_manifest(path, |adapters| {
        adapters.insert(h.id.clone(), entry);
    })
}

/// Removes custom adapter `id` from the Hub's harness manifest; refuses built-in ids.
pub fn undeclare(path: &std::path::Path, id: &str) -> Result<bool, HarnessError> {
    if steve_harness_spec::builtin(id).is_some() {
        let err = HarnessError::BuiltinProtected(id.to_owned());
        record_refusal(&err);
        return Err(err);
    }
    let mut removed = false;
    edit_manifest(path, |adapters| removed = adapters.remove(id).is_some())?;
    Ok(removed)
}

fn edit_manifest(
    path: &std::path::Path,
    f: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> Result<(), HarnessError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut doc: serde_json::Value = match std::fs::read_to_string(path) {
        Ok(t) => serde_json::from_str(&t).map_err(|e| HarnessError::ManifestRead(e.to_string()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => {
            return Err(HarnessError::ManifestRead(e.to_string()));
        }
    };
    if !doc.is_object() {
        doc = serde_json::json!({});
    }
    if !doc["adapters"].is_object() {
        doc["adapters"] = serde_json::json!({});
    }
    f(doc["adapters"].as_object_mut().expect("an object"));
    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| HarnessError::ManifestWrite(e.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        std::io::Write::write_all(&mut f, text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| HarnessError::ManifestWrite(e.to_string()))
}

/// Adapter `id` as declared, with what its probe reported where it did.
pub(crate) fn merge(
    id: &str,
    declared: Option<&steve_harness_spec::Declaration>,
    reported: Option<&HarnessCapability>,
) -> Harness {
    let mut h = Harness::declared(id, declared);
    let Some(r) = reported else {
        return h;
    };
    let Some(adapter) = r.adapter.as_ref() else {
        h.ready = Some(r.sessions);
        return h;
    };
    // a name the manifest gave wins; else what the adapter calls itself, for one of one's own
    if !h.builtin && declared.and_then(|d| d.name.as_ref()).is_none() {
        h.name = adapter.title.clone();
    }
    h.ready = Some(
        adapter.status == AdapterStatus::Ready
            && adapter.session.is_some()
            && reported
                .and_then(|r| r.setup.as_ref())
                .is_none_or(|s| s.state == steve_node_api::SetupState::Ready),
    );
    // Setup may have revoked an otherwise successful ACP snapshot (for example during
    // installation). Do not retain its models/modes in the independent model catalogue.
    if h.ready != Some(true) {
        h.models.clear();
        h.default_model = None;
        h.modes = None;
        h.efforts.clear();
        return h;
    }
    if let Some(caps) = &adapter.capabilities {
        h.effort_mode = caps.effort_mode;
    }
    if let Some(o) = &adapter.session {
        h.modes = Some(o.modes.clone());
        h.models = o
            .models
            .iter()
            .map(|m| HarnessModel {
                id: m.id.clone(),
                name: m.name.clone(),
                description: m.description.clone(),
                efforts: m.efforts.clone(),
            })
            .collect();
        h.default_model = o.default_model.clone();
        // An answered, empty list is authoritative too: fallback efforts must not make an
        // adapter that offers no reasoning control appear to accept one.
        h.efforts = o.efforts.clone();
        h.variant_models = o.variant_models.clone();
    }
    h
}

/// Hub harnesses whose local Node reported `harnesses` ready for any session: every
/// permission mode, `models` besides its default one and no reasoning control. For the
/// dispatch of tests.
#[cfg(test)]
pub(crate) async fn ready_locally(harnesses: &[&str], models: &[&str]) -> Arc<HubHarnesses> {
    use steve_node_api::{AdapterInfo, AdapterModel, AdapterOffer, PermissionMode};
    let home = tempfile::tempdir().unwrap();
    let cfg = crate::config::Config::from_vars(|k| {
        (k == "STEVE_HOME").then(|| home.path().display().to_string())
    })
    .unwrap();
    let hs = HubHarnesses::of(
        &cfg,
        crate::system::RuntimeReports::new(Arc::new(steve_runtime::cache::Caches::new(cfg.cache))),
    );
    let modes = vec![
        PermissionMode::Plan,
        PermissionMode::Ask,
        PermissionMode::AcceptEdits,
        PermissionMode::Auto,
        PermissionMode::FullAccess,
    ];
    let caps = harnesses
        .iter()
        .map(|id| HarnessCapability {
            name: id.to_string(),
            version: String::new(),
            sessions: true,
            jobs: true,
            models: vec![],
            resume: true,
            cancel: true,
            permissions: true,
            images: true,
            read_attached: false,
            enforcement: vec![],
            setup: None,
            adapter: Some(AdapterInfo {
                title: id.to_string(),
                builtin: false,
                status: AdapterStatus::Ready,
                error: None,
                version: None,
                load_session: true,
                list_sessions: false,
                mcp_http: true,
                embedded_context: false,
                auth: vec![],
                install_hint: None,
                system_prompt: false,
                session: Some(AdapterOffer {
                    modes: modes.clone(),
                    models: models
                        .iter()
                        .map(|m| AdapterModel {
                            id: m.to_string(),
                            name: None,
                            description: None,
                            efforts: None,
                        })
                        .collect(),
                    default_model: Some("default".into()),
                    efforts: vec![],
                    variant_models: vec![],
                }),
                session_error: None,
                session_auth_required: None,
                execution_modes: Some(modes.clone()),
                resume_command: None,
                fork_command: None,
                capabilities: None,
            }),
        })
        .collect();
    hs.update(caps).await;
    hs
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_node_api::{AdapterInfo, AdapterModel, AdapterOffer, PermissionMode};

    fn reported(id: &str, offer: Option<AdapterOffer>) -> HarnessCapability {
        HarnessCapability {
            name: id.into(),
            version: String::new(),
            sessions: true,
            jobs: true,
            models: vec![],
            resume: true,
            cancel: true,
            permissions: true,
            images: true,
            read_attached: false,
            enforcement: vec![],
            setup: None,
            adapter: Some(AdapterInfo {
                title: "Mine (adapter)".into(),
                builtin: false,
                status: AdapterStatus::Ready,
                error: None,
                version: None,
                load_session: true,
                list_sessions: false,
                mcp_http: true,
                embedded_context: false,
                auth: vec![],
                install_hint: None,
                system_prompt: false,
                session: offer,
                session_error: None,
                session_auth_required: None,
                execution_modes: None,
                resume_command: None,
                fork_command: None,
                capabilities: None,
            }),
        }
    }

    #[test]
    fn an_adapter_of_ones_own_is_declared_in_the_manifest_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness.json");
        std::fs::write(
            &path,
            r#"{"adapters": {"codex": "/opt/codex-acp"}, "node": "/opt/node"}"#,
        )
        .unwrap();
        let new = |props: serde_json::Value| NewHarness {
            id: "my-agent".into(),
            name: Some("My Agent".into()),
            command: "/opt/my-agent".into(),
            args: vec!["--acp".into()],
            properties: Some(props),
        };
        declare(&path, new(serde_json::json!({"modes": {"ask": "default"}}))).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // the other entries and keys stay
        assert_eq!(doc["adapters"]["codex"], "/opt/codex-acp");
        assert_eq!(doc["node"], "/opt/node");
        assert!(doc["adapters"]["my-agent"].get("env").is_none());
        let entry: steve_harness_spec::AdapterEntry =
            serde_json::from_value(doc["adapters"]["my-agent"].clone()).unwrap();
        let (commands, declared) = steve_harness_spec::split_entries([("my-agent".into(), entry)]);
        assert_eq!(
            commands["my-agent"],
            std::path::PathBuf::from("/opt/my-agent")
        );
        assert_eq!(declared["my-agent"].name.as_deref(), Some("My Agent"));
        assert_eq!(
            steve_harness_spec::spec("my-agent", declared.get("my-agent"))
                .properties
                .modes
                .ask,
            "default"
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // a property the adapter layer does not know, a bad id, env injection, or built-in override: refused
        assert!(matches!(
            declare(&path, new(serde_json::json!({"nope": 1}))),
            Err(HarnessError::UnknownProperty(_))
        ));
        let mut bad = new(serde_json::json!({}));
        bad.id = "Bad Id".into();
        assert!(matches!(
            declare(&path, bad),
            Err(HarnessError::InvalidId(_))
        ));
        let mut builtin = new(serde_json::json!({}));
        builtin.id = "claude-code".into();
        assert_eq!(
            declare(&path, builtin),
            Err(HarnessError::BuiltinProtected("claude-code".into()))
        );
        assert_eq!(
            undeclare(&path, "codex"),
            Err(HarnessError::BuiltinProtected("codex".into()))
        );
        assert_eq!(
            parse_new_harness(serde_json::json!({
                "id": "my-agent",
                "command": "/opt/my-agent",
                "env": {"ANTHROPIC_API_KEY": "sk-test"}
            }))
            .unwrap_err(),
            HarnessError::EnvNotAllowed
        );
        assert!(undeclare(&path, "my-agent").unwrap());
        assert!(!undeclare(&path, "my-agent").unwrap());
    }

    #[test]
    fn harness_manifest_refusals_increment_metrics_by_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness.json");
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            let _ = parse_new_harness(serde_json::json!({
                "id": "my-agent",
                "command": "/opt/my-agent",
                "env": {"ANTHROPIC_API_KEY": "sk-test"}
            }));
            let _ = declare(
                &path,
                NewHarness {
                    id: "claude-code".into(),
                    name: None,
                    command: "/opt/hijack".into(),
                    args: vec![],
                    properties: None,
                },
            );
            let _ = undeclare(&path, "codex");
        });
        let text = handle.render();
        assert!(
            text.lines()
                .any(|line| line.starts_with("steve_security_refusals_total{")
                    && line.contains("boundary=\"harness_manifest\"")
                    && line.contains("reason=\"env_not_allowed\"")
                    && line.ends_with(" 1")),
            "{text}"
        );
        assert!(
            text.lines()
                .any(|line| line.starts_with("steve_security_refusals_total{")
                    && line.contains("boundary=\"harness_manifest\"")
                    && line.contains("reason=\"builtin_override\"")
                    && line.ends_with(" 2")),
            "{text}"
        );
    }

    #[test]
    fn what_the_probe_said_replaces_the_declared_fallbacks() {
        // before a probe: the declared data
        let h = merge("claude-code", None, None);
        assert_eq!(h.ready, None);
        assert!(h.models.len() >= 3 && h.efforts.contains(&"max".to_string()));
        // after: the adapter's own lists, its modes, its name for one of one's own
        let offer = AdapterOffer {
            modes: vec![PermissionMode::Plan, PermissionMode::Ask],
            models: vec![AdapterModel {
                id: "m-1".into(),
                name: Some("One".into()),
                description: None,
                efforts: None,
            }],
            default_model: Some("m-1".into()),
            efforts: vec!["low".into()],
            variant_models: vec![],
        };
        let h = merge("mine", None, Some(&reported("mine", Some(offer))));
        assert_eq!(h.name, "Mine (adapter)");
        assert_eq!(h.ready, Some(true));
        assert_eq!(h.model_ids(), ["m-1"]);
        assert_eq!(h.default_model.as_deref(), Some("m-1"));
        assert_eq!(h.efforts, ["low"]);
        assert_eq!(h.modes.as_deref().map(|m| m.len()), Some(2));
        assert!(!h.offers_mode(PermissionMode::FullAccess));
        let h = merge(
            "claude-code",
            None,
            Some(&reported("claude-code", Some(AdapterOffer::default()))),
        );
        assert_eq!(h.ready, Some(true));
        assert!(
            h.models.is_empty() && h.efforts.is_empty(),
            "an answered empty list is not a fallback"
        );
        // a declared name stays; no scratch session: not ready, the declared lists kept
        let named: steve_harness_spec::Declaration =
            serde_json::from_value(serde_json::json!({"name": "Named"})).unwrap();
        let h = merge("mine", Some(&named), Some(&reported("mine", None)));
        assert_eq!((h.name.as_str(), h.ready), ("Named", Some(false)));
        // a ready adapter reporting empty efforts clears fallback_efforts instead of keeping stale ones
        let no_efforts = AdapterOffer {
            modes: vec![PermissionMode::Ask],
            models: vec![],
            default_model: None,
            efforts: vec![],
            variant_models: vec![],
        };
        let h = merge(
            "claude-code",
            None,
            Some(&reported("claude-code", Some(no_efforts))),
        );
        assert_eq!(h.ready, Some(true));
        assert!(h.efforts.is_empty(), "{:?}", h.efforts);
    }

    fn harnesses() -> Arc<HubHarnesses> {
        let home = tempfile::tempdir().unwrap();
        let cfg = crate::config::Config::from_vars(|k| {
            (k == "STEVE_HOME").then(|| home.path().display().to_string())
        })
        .unwrap();
        let reports = crate::system::RuntimeReports::new(Arc::new(
            steve_runtime::cache::Caches::new(cfg.cache),
        ));
        HubHarnesses::of(&cfg, reports)
    }
    #[tokio::test]
    async fn failed_node_refresh_revokes_local_readiness_without_erasing_remote_evidence() {
        let hs = harnesses();
        hs.update(vec![reported("claude-code", Some(AdapterOffer::default()))])
            .await;
        hs.reports
            .seed(
                "remote",
                vec![reported("remote-agent", Some(AdapterOffer::default()))],
                Some(1),
            )
            .await;
        let node = steve_node_api::testkit::FakeNode::new("local");
        node.refuse_next(
            "probe",
            steve_node_api::Error::new(steve_node_api::Code::Unavailable, "probe unavailable"),
        );
        hs.refresh(node.as_ref(), false).await;
        assert!(
            hs.list()
                .iter()
                .find(|h| h.id == "claude-code")
                .unwrap()
                .ready
                .is_none()
        );
        hs.update(vec![]).await;
        assert_eq!(
            hs.list()
                .iter()
                .find(|h| h.id == "remote-agent")
                .unwrap()
                .ready,
            Some(true)
        );
    }
    #[tokio::test]
    async fn a_linked_node_not_heard_from_leaves_a_harness_unknown() {
        let hs = harnesses();
        let ready_of = |linked: &[String]| {
            hs.readiness(linked)
                .into_iter()
                .find(|(id, _, _)| id == "claude-code")
                .and_then(|(_, _, ready)| ready)
        };
        hs.update(vec![reported("claude-code", None)]).await;
        assert_eq!(ready_of(&[]), Some(false));
        let linked = vec!["n_second".into()];
        assert_eq!(ready_of(&linked), None);
        hs.reports
            .seed("n_second", vec![reported("claude-code", None)], Some(1))
            .await;
        assert_eq!(ready_of(&linked), Some(false));
        hs.reports
            .seed(
                "n_second",
                vec![reported("claude-code", Some(AdapterOffer::default()))],
                Some(1),
            )
            .await;
        assert_eq!(ready_of(&linked), Some(true));
        hs.reports
            .seed("n_second", vec![reported("claude-code", None)], Some(1))
            .await;
        hs.reports.invalidate("local");
        assert_eq!(ready_of(&linked), None);
    }
    #[tokio::test(start_paused = true)]
    async fn bot_selection_uses_target_node_and_stale_or_missing_offers_stay_unknown() {
        let hs = harnesses();
        let offer = |model: &str| AdapterOffer {
            modes: vec![PermissionMode::Ask],
            models: vec![AdapterModel {
                id: model.into(),
                name: None,
                description: None,
                efforts: None,
            }],
            default_model: Some(model.into()),
            efforts: vec![],
            variant_models: vec![],
        };
        hs.update(vec![reported("claude-code", Some(offer("local-model")))])
            .await;
        hs.reports
            .seed(
                "remote",
                vec![reported("claude-code", Some(offer("remote-model")))],
                Some(1),
            )
            .await;
        let dir = tempfile::tempdir().unwrap();
        let store =
            steve_store::Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher)
                .unwrap();
        store.ensure_workspace().unwrap();
        store
            .bots()
            .seed(
                "b",
                false,
                &steve_store::NewBot {
                    name: "B".into(),
                    harness: "claude-code".into(),
                    ..Default::default()
                },
                steve_store::WS,
            )
            .unwrap();
        store
            .write(|w| {
                steve_store::bot_admin::set_bindings(w, steve_store::WS, "b", &["remote".into()])
            })
            .unwrap();
        let source = hs.bot_source(store);
        let local = serde_json::json!({"harness":"claude-code","model":"local-model"});
        let remote = serde_json::json!({"harness":"claude-code","model":"remote-model"});
        let valid = |patch: &serde_json::Value, bot: Option<&str>| {
            steve_core::models::validate_bot_patch(None, patch.as_object().unwrap(), &source(bot))
                .is_ok()
        };
        // a Bot nobody configured runs on the Hub's own Node
        assert!(valid(&local, None) && !valid(&remote, None));
        // a configured one on its execution Nodes
        assert!(valid(&remote, Some("b")) && !valid(&local, Some("b")));
        tokio::time::advance(REFRESH * 3).await;
        assert!(source(None).iter().all(|h| h.ready.is_none()));
    }
    #[tokio::test]
    async fn every_answer_of_the_node_is_told() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hs = harnesses();
        hs.update(vec![]).await;
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        hs.on_update(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        let offer = AdapterOffer {
            modes: vec![PermissionMode::Ask],
            models: ["default", "haiku"]
                .into_iter()
                .map(|id| AdapterModel {
                    id: id.into(),
                    name: None,
                    description: None,
                    efforts: None,
                })
                .collect(),
            default_model: Some("default".into()),
            efforts: vec![],
            variant_models: vec![],
        };
        hs.update(vec![reported("claude-code", Some(offer))]).await;
        assert_eq!(
            hs.list()
                .iter()
                .find(|h| h.id == "claude-code")
                .unwrap()
                .fast_model()
                .as_deref(),
            Some("haiku")
        );
        hs.update(vec![]).await;
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
}
