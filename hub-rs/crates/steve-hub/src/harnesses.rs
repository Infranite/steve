//! The harness adapters this Hub knows (its configuration and manifest, harness parity S1) with
//! what the local Node's probe last said of each (S4, S8): the list Steve's model settings, the
//! model catalogue, the bot pages and the runtimes list choose from. Refreshed in the background
//! from the Node (whose probe caches its adapters' answers); read without waiting.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use steve_core::harnesses::{Harness, HarnessModel, HarnessSource};
use steve_node_api::{AdapterStatus, HarnessCapability};

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
    declared: BTreeMap<String, steve_acp::Declaration>,
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
        default_node: Option<String>,
    ) -> steve_core::models::BotHarnessSource {
        let me = self.clone();
        Arc::new(move |bot| {
            let (mut nodes, legacy): (BTreeSet<String>, bool) = store.reader(|c| {
                let mut q = c.prepare(
                    "SELECT r.node_id FROM runtime_sessions r JOIN sessions s ON s.id=r.session_id
                       WHERE s.bot_id=?1 AND s.status<>'ended'
                     UNION
                     SELECT n.value FROM bot_node_bindings b JOIN bot_workspaces w
                       ON w.workspace_id=b.workspace_id AND w.bot_id=b.bot_id, json_each(b.nodes) n
                       WHERE b.bot_id=?1"
                )?;
                let rows = q.query_map([bot], |r| r.get(0))?.collect::<Result<BTreeSet<String>, _>>()?;
                let legacy: bool = c.query_row(
                    "SELECT NOT EXISTS(SELECT 1 FROM bot_node_bindings WHERE bot_id=?1)
                         OR EXISTS(SELECT 1 FROM bot_workspaces w WHERE w.bot_id=?1 AND NOT EXISTS
                           (SELECT 1 FROM bot_node_bindings b WHERE b.bot_id=w.bot_id AND b.workspace_id=w.workspace_id))",
                    [bot], |r| r.get(0),
                )?;
                Ok::<_, steve_store::StoreError>((rows, legacy))
            }).unwrap_or_default();
            if legacy {
                nodes.extend(default_node.iter().cloned());
            }
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
    pub async fn follow_all(
        self: Arc<Self>,
        local: Option<Arc<dyn steve_node_api::Node>>,
        svc: Option<Arc<crate::nodes::NodeService>>,
    ) {
        loop {
            self.refresh_all(local.as_deref(), svc.as_deref(), false)
                .await;
            tokio::time::sleep(REFRESH).await;
        }
    }
}

/// An adapter of one's own as the runtimes page declares it.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct NewHarness {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Declared properties ([`steve_acp::adapters::PropertiesPatch`]), checked key by key.
    #[serde(default)]
    pub properties: Option<serde_json::Value>,
}

/// Adds (or replaces) `h` in the Hub's harness manifest at `path`, keeping every other entry; in
/// force once the Hub restarts. The file may hold variables an adapter needs: it is the owner's
/// only.
pub fn declare(path: &std::path::Path, h: NewHarness) -> Result<(), String> {
    let id_ok = !h.id.is_empty()
        && h.id.len() <= 64
        && h.id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && h.id.starts_with(|c: char| c.is_ascii_alphanumeric());
    if !id_ok {
        return Err(steve_i18n::t!("hub.harness.bad_id", id = h.id));
    }
    let command = h.command.trim();
    if command.is_empty() || command.contains('\n') {
        return Err(steve_i18n::t!("hub.harness.bad_command"));
    }
    if let Some(p) = &h.properties {
        let patch: steve_acp::adapters::PropertiesPatch = serde_json::from_value(p.clone())
            .map_err(|e| steve_i18n::t!("hub.harness.bad_properties", error = e.to_string()))?;
        let _ = patch;
        // a key the adapter layer does not know is a mistake, not something to drop silently
        let given = p
            .as_object()
            .ok_or_else(|| steve_i18n::t!("hub.harness.bad_properties", error = "not an object"))?;
        if let Some(k) = given
            .keys()
            .find(|k| !steve_acp::adapters::PROPERTY_KEYS.contains(&k.as_str()))
        {
            return Err(steve_i18n::t!("hub.harness.unknown_property", key = k));
        }
    }
    let mut entry = serde_json::json!({ "command": command });
    if let Some(n) = h.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        entry["name"] = n.into();
    }
    if !h.args.is_empty() {
        entry["args"] = serde_json::json!(h.args);
    }
    if !h.env.is_empty() {
        entry["env"] = serde_json::json!(h.env);
    }
    if let Some(p) = h.properties {
        entry["properties"] = p;
    }
    edit_manifest(path, |adapters| {
        adapters.insert(h.id.clone(), entry);
    })
}

/// Removes adapter `id` from the Hub's harness manifest; whether it was there.
pub fn undeclare(path: &std::path::Path, id: &str) -> Result<bool, String> {
    let mut removed = false;
    edit_manifest(path, |adapters| removed = adapters.remove(id).is_some())?;
    Ok(removed)
}

fn edit_manifest(
    path: &std::path::Path,
    f: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut doc: serde_json::Value = match std::fs::read_to_string(path) {
        Ok(t) => serde_json::from_str(&t).map_err(|e| {
            steve_i18n::t!("hub.harness.manifest_unreadable", error = e.to_string())
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => {
            return Err(steve_i18n::t!(
                "hub.harness.manifest_unreadable",
                error = e.to_string()
            ));
        }
    };
    if !doc.is_object() {
        doc = serde_json::json!({});
    }
    if !doc["adapters"].is_object() {
        doc["adapters"] = serde_json::json!({});
    }
    f(doc["adapters"].as_object_mut().expect("an object"));
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
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
    write().map_err(|e| steve_i18n::t!("hub.harness.manifest_unwritable", error = e.to_string()))
}

/// Adapter `id` as declared, with what its probe reported where it did.
pub(crate) fn merge(
    id: &str,
    declared: Option<&steve_acp::Declaration>,
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
    if let Some(o) = &adapter.session {
        h.modes = Some(o.modes.clone());
        h.models = o
            .models
            .iter()
            .map(|m| HarnessModel {
                id: m.id.clone(),
                name: m.name.clone(),
                description: m.description.clone(),
            })
            .collect();
        h.default_model = o.default_model.clone();
        // An answered, empty list is authoritative too: fallback efforts must not make an
        // adapter that offers no reasoning control appear to accept one.
        h.efforts = o.efforts.clone();
    }
    h
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
                update: None,
                resume_command: None,
                fork_command: None,
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
            env: BTreeMap::from([("K".into(), "v".into())]),
            properties: Some(props),
        };
        declare(&path, new(serde_json::json!({"modes": {"ask": "default"}}))).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // the other entries and keys stay
        assert_eq!(doc["adapters"]["codex"], "/opt/codex-acp");
        assert_eq!(doc["node"], "/opt/node");
        let entry: steve_acp::AdapterEntry =
            serde_json::from_value(doc["adapters"]["my-agent"].clone()).unwrap();
        let (commands, declared) = steve_acp::adapters::split_entries([("my-agent".into(), entry)]);
        assert_eq!(
            commands["my-agent"],
            std::path::PathBuf::from("/opt/my-agent")
        );
        assert_eq!(declared["my-agent"].name.as_deref(), Some("My Agent"));
        assert_eq!(
            steve_acp::adapters::spec("my-agent", declared.get("my-agent"))
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
        // a property the adapter layer does not know, a bad id: refused, nothing written
        assert!(declare(&path, new(serde_json::json!({"nope": 1}))).is_err());
        let mut bad = new(serde_json::json!({}));
        bad.id = "Bad Id".into();
        assert!(declare(&path, bad).is_err());
        assert!(undeclare(&path, "my-agent").unwrap());
        assert!(!undeclare(&path, "my-agent").unwrap());
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
            }],
            default_model: Some("m-1".into()),
            efforts: vec!["low".into()],
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
        let named: steve_acp::Declaration =
            serde_json::from_value(serde_json::json!({"name": "Named"})).unwrap();
        let h = merge("mine", Some(&named), Some(&reported("mine", None)));
        assert_eq!((h.name.as_str(), h.ready), ("Named", Some(false)));
        // a ready adapter reporting empty efforts clears fallback_efforts instead of keeping stale ones
        let no_efforts = AdapterOffer {
            modes: vec![PermissionMode::Ask],
            models: vec![],
            default_model: None,
            efforts: vec![],
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
            steve_node_api::ApiError::new(steve_node_api::ErrorCode::Offline, "probe unavailable"),
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
            }],
            default_model: Some(model.into()),
            efforts: vec![],
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
        let source = hs.bot_source(store, Some("remote".into()));
        let local = serde_json::json!({"harness":"claude-code","model":"local-model"});
        let remote = serde_json::json!({"harness":"claude-code","model":"remote-model"});
        assert!(
            steve_core::models::validate_bot_patch(None, local.as_object().unwrap(), &hs.list())
                .is_ok()
        );
        assert!(
            steve_core::models::validate_bot_patch(None, local.as_object().unwrap(), &source(None))
                .is_err()
        );
        assert!(
            steve_core::models::validate_bot_patch(
                None,
                remote.as_object().unwrap(),
                &source(None)
            )
            .is_ok()
        );
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
                })
                .collect(),
            default_model: Some("default".into()),
            efforts: vec![],
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
