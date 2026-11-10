//! The harness adapters this Hub knows (its configuration and manifest, harness parity S1) with
//! what the local Node's probe last said of each (S4, S8): the list Steve's model settings, the
//! model catalogue, the bot pages and the runtimes list choose from. Refreshed in the background
//! from the Node (whose probe caches its adapters' answers); read without waiting.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use steve_core::harnesses::{Harness, HarnessModel, HarnessSource};
use steve_node_api::{AdapterStatus, HarnessCapability};

/// How often the list asks the local Node again (the Node answers from its own cache).
const REFRESH: Duration = Duration::from_secs(60);

pub struct HubHarnesses {
    /// The adapters the Hub can start, by id.
    ids: Vec<String>,
    declared: BTreeMap<String, steve_acp::Declaration>,
    /// What the local Node reported last (none yet, or no Node).
    probed: RwLock<Vec<HarnessCapability>>,
    /// Told after each answer of the Node ([`HubHarnesses::on_update`]).
    updated: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl HubHarnesses {
    pub fn of(cfg: &crate::config::Config) -> Arc<HubHarnesses> {
        Arc::new(HubHarnesses {
            ids: cfg.harness_ids(),
            declared: cfg.declared_adapters.clone(),
            probed: RwLock::new(vec![]),
            updated: std::sync::OnceLock::new(),
        })
    }

    /// Every adapter the Hub knows, with what its probe said.
    pub fn list(&self) -> Vec<Harness> {
        let probed = self.probed.read().unwrap_or_else(|e| e.into_inner());
        self.ids
            .iter()
            .map(|id| {
                merge(
                    id,
                    self.declared.get(id),
                    probed.iter().find(|h| &h.name == id),
                )
            })
            .collect()
    }

    pub fn update(&self, caps: Vec<HarnessCapability>) {
        *self.probed.write().unwrap_or_else(|e| e.into_inner()) = caps;
        if let Some(f) = self.updated.get() {
            f();
        }
    }

    /// `f` runs after every answer of the Node from now on (the first one given stays).
    pub fn on_update(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.updated.set(Box::new(f));
    }

    /// The list as Steve's model resolver reads it.
    pub fn source(self: &Arc<Self>) -> HarnessSource {
        let me = self.clone();
        Arc::new(move || me.list())
    }

    /// Asks `node` now (`fresh`: its adapters again) and keeps the answer.
    pub async fn refresh(&self, node: &dyn steve_node_api::Node, fresh: bool) {
        match node.probe(fresh).await {
            Ok(c) => self.update(c.harnesses),
            Err(e) => tracing::warn!("harnesses: probe of the local Node: {e}"),
        }
    }

    /// Keeps the list fresh from `node` while the Hub runs.
    pub async fn follow(self: Arc<Self>, node: Arc<dyn steve_node_api::Node>) {
        loop {
            self.refresh(node.as_ref(), false).await;
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
fn merge(
    id: &str,
    declared: Option<&steve_acp::Declaration>,
    reported: Option<&HarnessCapability>,
) -> Harness {
    let mut h = Harness::declared(id, declared);
    let Some(adapter) = reported.and_then(|r| r.adapter.as_ref()) else {
        return h;
    };
    // a name the manifest gave wins; else what the adapter calls itself, for one of one's own
    if !h.builtin && declared.and_then(|d| d.name.as_ref()).is_none() {
        h.name = adapter.title.clone();
    }
    h.ready = Some(adapter.status == AdapterStatus::Ready && adapter.session.is_some());
    if let Some(o) = &adapter.session {
        h.modes = Some(o.modes.clone());
        if !o.models.is_empty() {
            h.models = o
                .models
                .iter()
                .map(|m| HarnessModel {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    description: m.description.clone(),
                })
                .collect();
        }
        h.default_model = o.default_model.clone();
        if !o.efforts.is_empty() {
            h.efforts = o.efforts.clone();
        }
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
                update: None,
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
        // a declared name stays; no scratch session: not ready, the declared lists kept
        let named: steve_acp::Declaration =
            serde_json::from_value(serde_json::json!({"name": "Named"})).unwrap();
        let h = merge("mine", Some(&named), Some(&reported("mine", None)));
        assert_eq!((h.name.as_str(), h.ready), ("Named", Some(false)));
    }

    #[test]
    fn every_answer_of_the_node_is_told() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hs = HubHarnesses {
            ids: vec!["claude-code".into()],
            declared: BTreeMap::new(),
            probed: RwLock::new(vec![]),
            updated: std::sync::OnceLock::new(),
        };
        // nobody listening: nothing to tell
        hs.update(vec![]);
        let n = Arc::new(AtomicUsize::new(0));
        let seen = n.clone();
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
        // what a ready adapter offers is where its quick model comes from, not its declared list
        assert_eq!(hs.list()[0].fast_model(), None);
        hs.update(vec![reported("claude-code", Some(offer))]);
        assert_eq!(hs.list()[0].fast_model().as_deref(), Some("haiku"));
        hs.update(vec![]);
        assert_eq!(hs.list()[0].fast_model(), None);
        assert_eq!(n.load(Ordering::SeqCst), 2);
    }
}
