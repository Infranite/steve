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
    /// Words that name its quick, cheap models, best first (declared).
    #[serde(skip)]
    pub fast_models: Vec<String>,
}

impl Harness {
    /// The entry for adapter `id` as declared, before any probe.
    pub fn declared(id: &str, declared: Option<&steve_acp::Declaration>) -> Harness {
        let spec = steve_acp::adapters::spec(id, declared);
        let p = &spec.properties;
        let ids: Vec<String> = [
            p.modes.plan.as_str(),
            p.modes.ask.as_str(),
            p.modes.accept_edits.as_str(),
            p.modes.auto.as_str(),
            p.modes.full_access.as_str(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
        let offered = p
            .modes
            .offered_with(Some(&ids), p.blind_set_mode, Some(&p.mode_meta));
        Harness {
            id: id.to_string(),
            name: spec.name.clone(),
            builtin: spec.builtin,
            ready: None,
            modes: (offered.len() < 5).then_some(offered),
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
            fast_models: p.fast_models.clone(),
        }
    }

    /// Its quick, cheap model: of the models it offers, one whose name (the id after any route
    /// prefix, `route/name`) has the earliest of its declared words as a whole word (`mini` is in
    /// `gpt-6-mini`, not in `gemini-3`). Of several: the one on the route of its default model,
    /// then the newest by the numbers in the name, then the plainest name, then the first offered.
    /// None before it has answered a probe (a declared list is not what this installation has) or
    /// when it offers no such model.
    pub fn fast_model(&self) -> Option<String> {
        if self.ready != Some(true) {
            return None;
        }
        let split = |id: &str| match id.rsplit_once('/') {
            Some((route, name)) => (Some(route.to_string()), name.to_ascii_lowercase()),
            None => (None, id.to_ascii_lowercase()),
        };
        let words = |name: &str| -> Vec<String> {
            name.split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|w| !w.is_empty())
                .map(String::from)
                .collect()
        };
        let numbers = |name: &str| -> Vec<u64> {
            name.split(|c: char| !c.is_ascii_digit())
                .filter_map(|n| n.parse().ok())
                .collect()
        };
        let own = self.default_model.as_deref().and_then(|d| split(d).0);
        self.fast_models.iter().find_map(|w| {
            let kw = words(&w.to_ascii_lowercase());
            if kw.is_empty() {
                return None;
            }
            self.models
                .iter()
                .enumerate()
                .filter_map(|(at, m)| {
                    let (route, name) = split(&m.id);
                    let ws = words(&name);
                    ws.windows(kw.len()).any(|win| win == kw).then(|| {
                        let rank = (
                            route == own,
                            numbers(&name),
                            std::cmp::Reverse(ws.len()),
                            std::cmp::Reverse(at),
                        );
                        (rank, &m.id)
                    })
                })
                .max_by(|a, b| a.0.cmp(&b.0))
                .map(|(_, id)| id.clone())
        })
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

    /// Default `perm_mode` key for a new bot on this harness: Ask when offered, else Auto,
    /// AcceptEdits, FullAccess, or Plan.
    pub fn default_perm_mode(&self) -> &'static str {
        const ORDER: [(PermissionMode, &str); 5] = [
            (PermissionMode::Ask, "default"),
            (PermissionMode::Auto, "auto"),
            (PermissionMode::AcceptEdits, "acceptEdits"),
            (PermissionMode::FullAccess, "bypassPermissions"),
            (PermissionMode::Plan, "plan"),
        ];
        ORDER
            .iter()
            .find(|(m, _)| self.offers_mode(*m))
            .map_or("default", |(_, k)| *k)
    }
}

/// Default `(harness_id, perm_mode)` for a new bot: `preferred` (e.g. the main bot's harness) when
/// it is in `list` and not proven unavailable (`ready != Some(false)`), else the first `ready ==
/// Some(true)` harness, else the first `ready != Some(false)`, else the first in `list` or the
/// built-in default adapter.
pub fn default_bot_choice(list: &[Harness], preferred: Option<&str>) -> (String, String) {
    let pick = preferred
        .and_then(|id| list.iter().find(|h| h.id == id && h.ready != Some(false)))
        .or_else(|| list.iter().find(|h| h.ready == Some(true)))
        .or_else(|| list.iter().find(|h| h.ready != Some(false)))
        .or_else(|| list.first());
    match pick {
        Some(h) => (h.id.clone(), h.default_perm_mode().into()),
        None => (
            steve_acp::adapters::DEFAULT_ADAPTER.0.into(),
            "default".into(),
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An adapter that calls `words` quick and has answered a probe with `models`.
    fn offering(words: &[&str], default: Option<&str>, models: &[&str]) -> Harness {
        let mut h = Harness::declared("mine", None);
        h.ready = Some(true);
        h.fast_models = words.iter().map(|w| w.to_string()).collect();
        h.default_model = default.map(String::from);
        h.models = models
            .iter()
            .map(|m| HarnessModel {
                id: m.to_string(),
                name: None,
                description: None,
            })
            .collect();
        h
    }

    #[test]
    fn the_quick_model_is_a_whole_word_the_newest_and_on_the_default_s_route() {
        let pick = |words: &[&str], default: Option<&str>, models: &[&str]| {
            offering(words, default, models).fast_model()
        };
        // a list in alphabetical order: the newest, not the first
        let claude = [
            "default",
            "claude-fable-5-2",
            "claude-haiku-4-5-20251001",
            "claude-haiku-5-5",
            "claude-haiku-5-5-thinking",
            "gw/claude-haiku-4-5-20251001",
            "gw/claude-haiku-5-5",
            "other/claude-haiku-6",
        ];
        assert_eq!(
            pick(&["haiku"], None, &claude).as_deref(),
            Some("claude-haiku-5-5")
        );
        // the default model goes through a route: the quick one takes the same
        assert_eq!(
            pick(&["haiku"], Some("gw/claude-fable-5-2"), &claude).as_deref(),
            Some("gw/claude-haiku-5-5")
        );
        // a route that has none: any, the newest
        assert_eq!(
            pick(&["haiku"], Some("none/x"), &claude).as_deref(),
            Some("other/claude-haiku-6")
        );
        // a word inside another word or in the route is not the word
        let mixed = [
            "gemini-3-flash",
            "local/mac-mini/qwen-3",
            "gpt-5.6-luna-mini",
            "gpt-6-luna",
            "gpt-6-luna-mini",
        ];
        assert_eq!(
            pick(&["mini"], None, &mixed).as_deref(),
            Some("gpt-6-luna-mini")
        );
        assert_eq!(pick(&["mini"], None, &mixed[..2]), None);
        // the earliest declared word that names a model, whatever the case; an alias alone is fine
        assert_eq!(
            pick(&["nano", "mini"], None, &mixed).as_deref(),
            Some("gpt-6-luna-mini")
        );
        assert_eq!(
            pick(&["haiku"], None, &["default", "opus", "Haiku"]).as_deref(),
            Some("Haiku")
        );
        assert_eq!(pick(&[""], None, &mixed), None);
        // what a Codex behind a gateway offered on 2026-11-11: no `mini` at all, the quick tier
        // under another word, with and without a route, and a default on a route that has none
        let codex = [
            "gpt-6.1-sol",
            "gw/gemini-3-flash",
            "codex/gpt-5.6-luna",
            "codex/gpt-6-luna",
            "passthrough/local/mac-mini/qwen-3",
            "gpt-6-luna",
            "gpt-5.6-luna",
        ];
        assert_eq!(pick(&["mini"], Some("gw/gemini-4"), &codex), None);
        assert_eq!(
            pick(&["luna", "mini"], Some("gw/gemini-4"), &codex).as_deref(),
            Some("codex/gpt-6-luna")
        );
        assert_eq!(
            pick(&["luna", "mini"], Some("gpt-6.1-sol"), &codex).as_deref(),
            Some("gpt-6-luna")
        );
        // multi-word hyphenated fast keyword (`flash-lite` beats `flash` when listed earlier)
        assert_eq!(
            pick(
                &["flash-lite", "flash"],
                None,
                &[
                    "gemini-2.5-pro",
                    "gemini-2.5-flash",
                    "gemini-2.5-flash-lite"
                ],
            )
            .as_deref(),
            Some("gemini-2.5-flash-lite")
        );
        // nothing before a probe has answered
        let mut h = offering(&["haiku"], None, &claude);
        h.ready = None;
        assert_eq!(h.fast_model(), None);
    }
}
