//! The models each harness adapter can actually use (`GET /api/system/models?harness=…`), so model
//! fields are picked from a list instead of typed: what the adapter itself offers in a scratch
//! session (its `model` config option, through the local Node's probe, harness parity S8), else
//! the list its manifest declares. Which models currently answer is not probed by calling them:
//! the handler adds what the statistics saw this week (last success, last failure per model).

use serde_json::{Value, json};

/// One model a harness offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: String,
    pub label: String,
    pub family: &'static str,
    /// The route prefix (`kiro`, `passthrough`), shown next to the name.
    pub via: Option<String>,
    /// A short note from the catalogue (an audit warning).
    pub note: Option<String>,
    /// Reasoning efforts the model takes, when its adapter says per model.
    pub efforts: Vec<String>,
}

impl Entry {
    fn new(id: &str, label: Option<&str>) -> Entry {
        let (via, base) = split_via(id);
        Entry {
            id: id.to_string(),
            label: label
                .filter(|l| !l.trim().is_empty())
                .unwrap_or(id)
                .to_string(),
            family: family(base),
            via: via.map(String::from),
            note: None,
            efforts: vec![],
        }
    }

    pub fn json(&self) -> Value {
        json!({
            "id": self.id, "label": self.label, "family": self.family,
            "via": self.via, "note": self.note,
            "efforts": self.efforts,
        })
    }
}

/// Families in the order the picker shows them.
pub const FAMILIES: [&str; 15] = [
    "Claude Opus",
    "Claude Sonnet",
    "Claude Haiku",
    "Claude Fable",
    "Claude",
    "GPT",
    "Gemini",
    "Grok",
    "Mistral",
    "Kimi",
    "DeepSeek",
    "GLM",
    "Qwen",
    "MiniMax",
    OTHER,
];

/// The catch-all family. The cached list keeps the zh-CN words; [`with_usage`] shows them in the request's language.
const OTHER: &str = "其他";
/// Where a list came from: the adapter itself, or the list its manifest declares.
const ADAPTER: &str = "adapter";
const BUILTIN: &str = "内置列表";

/// `kiro/claude-opus-5-5` → (`kiro`, `claude-opus-5-5`); ids without a known route prefix as they are.
fn split_via(id: &str) -> (Option<&str>, &str) {
    match id.split_once('/') {
        Some((p, rest)) if matches!(p, "kiro" | "passthrough") => (Some(p), rest),
        _ => (None, id),
    }
}

/// The family of a model id (route prefix removed).
pub fn family(base: &str) -> &'static str {
    let b = base.to_ascii_lowercase();
    let b = b.rsplit('/').next().unwrap_or(&b);
    [
        ("claude-opus", "Claude Opus"),
        ("claude-sonnet", "Claude Sonnet"),
        ("claude-haiku", "Claude Haiku"),
        ("claude-fable", "Claude Fable"),
        ("claude", "Claude"),
        ("opus", "Claude Opus"),
        ("sonnet", "Claude Sonnet"),
        ("haiku", "Claude Haiku"),
        ("gpt", "GPT"),
        ("o1", "GPT"),
        ("o3", "GPT"),
        ("o4", "GPT"),
        ("gemini", "Gemini"),
        ("grok", "Grok"),
        ("mistral", "Mistral"),
        ("codestral", "Mistral"),
        ("devstral", "Mistral"),
        ("kimi", "Kimi"),
        ("deepseek", "DeepSeek"),
        ("glm", "GLM"),
        ("qwen", "Qwen"),
        ("minimax", "MiniMax"),
    ]
    .iter()
    .find(|(p, _)| b.starts_with(p))
    .map_or(OTHER, |(_, f)| f)
}

/// The note of a model's description: its `[…]` remark (an audit warning), shortened.
fn note_of(desc: &str) -> Option<String> {
    let (_, rest) = desc.split_once('[')?;
    let inner = rest.split(']').next()?.trim();
    let short: String = inner.chars().take(40).collect();
    (!short.is_empty()).then_some(short)
}

/// Where the lists come from: each adapter's own (what its probe reported), else its declared
/// fallback list.
pub struct Catalog {
    /// `STEVE_DEFAULT_MODEL_<HARNESS>`: a new agent session's model where the adapter names none.
    pub agent_defaults: std::collections::BTreeMap<String, String>,
}

impl Catalog {
    pub fn of(cfg: &crate::config::Config) -> Catalog {
        Catalog {
            agent_defaults: cfg
                .default_models
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    /// `{harness, name, source, default, efforts, families, models}` of `h`.
    pub fn list(&self, h: &steve_core::harnesses::Harness) -> Value {
        let probed = h.ready == Some(true);
        let models: Vec<Entry> = h
            .models
            .iter()
            .filter(|m| steve_core::models::valid_model(&m.id))
            .map(|m| {
                let mut e = Entry::new(&m.id, m.name.as_deref());
                e.note = m.description.as_deref().and_then(note_of);
                e
            })
            .collect();
        let families: Vec<&str> = FAMILIES
            .iter()
            .copied()
            .filter(|f| models.iter().any(|m| m.family == *f))
            .collect();
        json!({
            "harness": h.id,
            "name": h.name,
            "source": if probed && !models.is_empty() { ADAPTER } else { BUILTIN },
            "default": h.default_model.clone().or_else(|| self.agent_defaults.get(&h.id).cloned()),
            "efforts": h.efforts,
            "families": families,
            "models": models.iter().map(Entry::json).collect::<Vec<_>>(),
        })
    }
}

/// Adds what the statistics saw (`used`: model → `{calls, ok_at, failed_at}`) to a list: each listed
/// model gets its `used`, and a model used this week that the list lacks is appended with a note. The
/// list's own words (source, the catch-all family) come out in the request's language.
pub fn with_usage(list: Value, used: &Value) -> Value {
    shown(add_usage(list, used))
}

/// The source and the catch-all family in the request's language.
fn shown(mut list: Value) -> Value {
    let other = || steve_i18n::t!("hub.models.family_other");
    match list["source"].as_str() {
        Some(BUILTIN) => list["source"] = json!(steve_i18n::t!("hub.models.source_builtin")),
        Some(ADAPTER) => list["source"] = json!(steve_i18n::t!("hub.models.source_adapter")),
        _ => {}
    }
    for f in list["families"].as_array_mut().into_iter().flatten() {
        if f == OTHER {
            *f = json!(other());
        }
    }
    for m in list["models"].as_array_mut().into_iter().flatten() {
        if m["family"] == OTHER {
            m["family"] = json!(other());
        }
    }
    list
}

fn add_usage(mut list: Value, used: &Value) -> Value {
    let Some(used) = used.as_object() else {
        return list;
    };
    let Some(models) = list.get_mut("models").and_then(Value::as_array_mut) else {
        return list;
    };
    for m in models.iter_mut() {
        if let Some(u) = m["id"].as_str().and_then(|id| used.get(id)) {
            m["used"] = u.clone();
        }
    }
    for (id, u) in used {
        if !models.iter().any(|m| m["id"] == *id) && steve_core::models::valid_model(id) {
            let mut e = Entry::new(id, None);
            e.note = Some(steve_i18n::t!("hub.models.recent_unlisted"));
            let mut v = e.json();
            v["used"] = u.clone();
            models.push(v);
        }
    }
    let mut fams: Vec<String> = list["families"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    for m in list["models"].as_array().into_iter().flatten() {
        if let Some(f) = m["family"].as_str()
            && !fams.iter().any(|x| x == f)
        {
            fams.push(f.to_string());
        }
    }
    list["families"] = json!(fams);
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_core::harnesses::{Harness, HarnessModel};

    fn ids(v: &Value) -> Vec<String> {
        v["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    }

    fn catalog() -> Catalog {
        Catalog {
            agent_defaults: [("claude-code".to_string(), "claude-opus-5-5".to_string())]
                .into_iter()
                .collect(),
        }
    }

    /// Before a probe: the declared list, the agent default; after: the adapter's own models,
    /// grouped, a description's audit remark as the note, its default and efforts.
    #[test]
    fn an_adapters_own_list_replaces_the_declared_one() {
        let c = catalog();
        let declared = Harness::declared("claude-code", None);
        let v = c.list(&declared);
        assert_eq!(v["source"], BUILTIN);
        assert_eq!(ids(&v).len(), 3);
        assert_eq!(v["default"], "claude-opus-5-5");
        let mut probed = Harness::declared("mine", None);
        probed.ready = Some(true);
        probed.models = vec![
            HarnessModel {
                id: "gpt-6-sol".into(),
                name: Some("GPT 6 Sol".into()),
                description: Some("Served here. [warning: slow]".into()),
            },
            HarnessModel {
                id: "kiro/claude-sonnet-5-5".into(),
                name: None,
                description: None,
            },
            HarnessModel {
                id: "not a model".into(),
                name: None,
                description: None,
            },
        ];
        probed.default_model = Some("gpt-6-sol".into());
        probed.efforts = vec!["low".into(), "high".into()];
        let v = c.list(&probed);
        assert_eq!(v["source"], ADAPTER);
        assert_eq!(ids(&v), ["gpt-6-sol", "kiro/claude-sonnet-5-5"]);
        assert_eq!(v["models"][0]["label"], "GPT 6 Sol");
        assert_eq!(v["models"][0]["note"], "warning: slow");
        assert_eq!(v["models"][1]["via"], "kiro");
        assert_eq!(v["families"], json!(["Claude Sonnet", "GPT"]));
        assert_eq!(v["default"], "gpt-6-sol");
        assert_eq!(v["efforts"], json!(["low", "high"]));
    }

    #[test]
    fn families() {
        assert_eq!(family("claude-opus-4-8"), "Claude Opus");
        assert_eq!(family("o4-mini"), "GPT");
        assert_eq!(family("xai/grok-4"), "Grok");
        assert_eq!(family("devstral-medium"), "Mistral");
        assert_eq!(family("deepseek/deepseek-flash"), "DeepSeek");
        assert_eq!(family("local/mac-mini/unsloth/Qwen3.6-35B"), "Qwen");
        assert_eq!(family("auto-mode-classifier"), "其他");
        assert_eq!(
            split_via("passthrough/glm-5"),
            (Some("passthrough"), "glm-5")
        );
        assert_eq!(split_via("deepseek/x"), (None, "deepseek/x"));
    }
}
