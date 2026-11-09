//! The harness adapters Steve can run on, as the Hub knows them (harness parity S8): the adapter
//! registry's entries with what their Node's probe reported (the permission modes, models and
//! efforts each offers). Steve's judgments, research, the model settings and the bot pages choose
//! from this list, by adapter id; nothing here knows a harness by name.

use std::sync::Arc;

use serde::Serialize;
use steve_node_api::PermissionMode;

/// A model an adapter offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HarnessModel {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One harness adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Harness {
    /// The adapter's id: the key everywhere (bots, settings, statistics).
    pub id: String,
    pub name: String,
    pub builtin: bool,
    /// Its adapter answered the last probe; `None`: not asked (no Node, or not yet).
    pub ready: Option<bool>,
    /// Steve's permission modes it offers; `None`: not known yet (all are shown).
    pub modes: Option<Vec<PermissionMode>>,
    /// The models it offers (what the probe reported, else its declared fallback list).
    pub models: Vec<HarnessModel>,
    /// What it runs when no model is set.
    pub default_model: Option<String>,
    /// The reasoning efforts it takes.
    pub efforts: Vec<String>,
    /// Steve's judgments fall back down Steve's model lists after the bot's model (declared).
    pub model_fallback: bool,
}

impl Harness {
    /// The entry for adapter `id` as declared, before any probe.
    pub fn declared(id: &str, declared: Option<&steve_acp::Declaration>) -> Harness {
        let spec = steve_acp::adapters::spec(id, declared);
        let p = &spec.properties;
        Harness {
            id: id.to_string(),
            name: spec.name.clone(),
            builtin: spec.builtin,
            ready: None,
            modes: None,
            models: p
                .fallback_models
                .iter()
                .map(|m| HarnessModel {
                    id: m.clone(),
                    name: None,
                    description: None,
                })
                .collect(),
            default_model: None,
            efforts: p.fallback_efforts.clone(),
            model_fallback: p.model_fallback,
        }
    }

    /// The model ids it offers.
    pub fn model_ids(&self) -> Vec<String> {
        self.models.iter().map(|m| m.id.clone()).collect()
    }

    /// Whether it takes `effort` (an adapter that reports no efforts takes none).
    pub fn takes_effort(&self, effort: &str) -> bool {
        self.efforts.iter().any(|e| e == effort)
    }

    /// Whether it can be set to permission mode `mode` (unknown yet: yes).
    pub fn offers_mode(&self, mode: PermissionMode) -> bool {
        self.modes.as_ref().is_none_or(|m| m.contains(&mode))
    }
}

/// Where the list comes from: the Hub's registry and its Node's probe; cheap to call.
pub type HarnessSource = Arc<dyn Fn() -> Vec<Harness> + Send + Sync>;

/// The built-in adapters as declared (tests, a Hub that has not probed yet).
pub fn builtin() -> Vec<Harness> {
    steve_acp::adapters::builtin_ids()
        .map(|id| Harness::declared(id, None))
        .collect()
}

/// A source that is always the built-in adapters as declared.
pub fn builtin_source() -> HarnessSource {
    Arc::new(builtin)
}

/// The display name of adapter `id` (its declared or built-in name; an unknown id as it is).
pub fn name_of(list: &[Harness], id: &str) -> String {
    list.iter().find(|h| h.id == id).map_or_else(
        || steve_acp::adapters::spec(id, None).name,
        |h| h.name.clone(),
    )
}
