//! The models each harness can actually use (`GET /api/system/models?harness=…`), so model fields are
//! picked from a list instead of typed.
//!
//! - Claude Code: the user's own model picker, `modelPicker.options` of `~/.claude/settings.json`
//!   (`$CLAUDE_CONFIG_DIR` when set), which mirrors the gateway's catalogue. Only that key is
//!   deserialized: `apiKeyHelper`, `env` and anything else in the file are skipped by the parser and
//!   never stored, logged or returned. Nothing is executed.
//! - Codex: `codex debug models` (the catalogue Codex itself shows, `visibility: list` only) and the
//!   default `model` of `$CODEX_HOME/config.toml`.
//!
//! Either falls back to the built-in list. Lists are cached for ten minutes per harness. Which models
//! currently answer is not probed by calling them: the handler adds what the statistics saw this week
//! (last success, last failure per model).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use steve_core::models::{CLAUDE_CODE, CLAUDE_MODELS, CODEX, CODEX_MODELS};

/// How long a list is reused.
const TTL: Duration = Duration::from_secs(600);

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
    /// Reasoning efforts the model takes (Codex's catalogue says).
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
pub const FAMILIES: [&str; 13] = [
    "Claude Opus",
    "Claude Sonnet",
    "Claude Haiku",
    "Claude Fable",
    "Claude",
    "GPT",
    "Gemini",
    "Kimi",
    "DeepSeek",
    "GLM",
    "Qwen",
    "MiniMax",
    OTHER,
];

/// The catch-all family. The cached list keeps the zh-CN words; [`with_usage`] shows them in the request's language.
const OTHER: &str = "其他";
/// Where a list came from, when it isn't the CLI's own (`codex debug models`).
const BUILTIN: &str = "内置列表";
const CLAUDE_PICKER: &str = "Claude Code 的模型选择器（~/.claude/settings.json）";

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
        ("o3", "GPT"),
        ("gemini", "Gemini"),
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

/// The one key of Claude Code's settings this module reads. Every other key (`apiKeyHelper`, `env`, …)
/// is skipped by the deserializer.
#[derive(Deserialize)]
struct ClaudeSettings {
    #[serde(rename = "modelPicker", default)]
    model_picker: Option<Picker>,
}

#[derive(Deserialize)]
struct Picker {
    #[serde(default)]
    options: Vec<Value>,
}

/// The models of the user's Claude Code model picker, or `None` when the file or the key is missing.
pub fn claude_picker(settings: &Path) -> Option<Vec<Entry>> {
    let text = std::fs::read(settings).ok()?;
    let s: ClaudeSettings = serde_json::from_slice(&text).ok()?;
    let mut out: Vec<Entry> = vec![];
    for o in s.model_picker?.options {
        let Some(id) = o.get("model").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if !steve_core::models::valid_model(id) || out.iter().any(|e| e.id == id) {
            continue;
        }
        out.push(Entry::new(id, o.get("label").and_then(Value::as_str)));
    }
    (!out.is_empty()).then_some(out)
}

/// The note of a Codex catalogue description: its `[…]` audit remark, shortened.
fn codex_note(desc: &str) -> Option<String> {
    let (_, rest) = desc.split_once('[')?;
    let inner = rest.split(']').next()?.trim();
    let short: String = inner.chars().take(40).collect();
    (!short.is_empty()).then_some(short)
}

/// Codex's catalogue (`codex debug models`): listed models, with their efforts and audit notes.
pub fn codex_catalogue(json_text: &str) -> Option<Vec<Entry>> {
    let v: Value = serde_json::from_str(json_text).ok()?;
    let list = v.get("models").and_then(Value::as_array).or(v.as_array())?;
    let mut out: Vec<(i64, Entry)> = vec![];
    for m in list {
        if m.get("visibility").and_then(Value::as_str) != Some("list") {
            continue;
        }
        let Some(id) = m.get("slug").and_then(Value::as_str) else {
            continue;
        };
        if !steve_core::models::valid_model(id) {
            continue;
        }
        let mut e = Entry::new(id, m.get("display_name").and_then(Value::as_str));
        e.note = m
            .get("description")
            .and_then(Value::as_str)
            .and_then(codex_note);
        e.efforts = m
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("effort").and_then(Value::as_str).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        out.push((m.get("priority").and_then(Value::as_i64).unwrap_or(99), e));
    }
    // Codex's own order: priority, then id
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.id.cmp(&b.1.id)));
    let out: Vec<Entry> = out.into_iter().map(|(_, e)| e).collect();
    (!out.is_empty()).then_some(out)
}

/// Where the lists come from.
pub struct Catalog {
    pub claude_settings: PathBuf,
    pub codex: PathBuf,
    pub codex_home: PathBuf,
    /// `STEVE_DEFAULT_MODEL_CLAUDE`
    pub claude_default: Option<String>,
    cache: Mutex<HashMap<String, (Instant, Value)>>,
    /// The local Node (`STEVE_NODE_MODE=local`): Codex's list is what its probe reports, the Hub
    /// runs no `codex`.
    node: std::sync::OnceLock<std::sync::Arc<dyn steve_node_api::Node>>,
}

impl Catalog {
    pub fn new(
        claude_settings: PathBuf,
        codex: PathBuf,
        codex_home: PathBuf,
        claude_default: Option<String>,
    ) -> Catalog {
        Catalog {
            claude_settings,
            codex,
            codex_home,
            claude_default,
            cache: Mutex::default(),
            node: std::sync::OnceLock::new(),
        }
    }

    /// List through the local Node from now on.
    pub fn set_node(&self, node: std::sync::Arc<dyn steve_node_api::Node>) {
        let _ = self.node.set(node);
    }

    /// From the Hub's configuration: `$CLAUDE_CONFIG_DIR/settings.json` or `~/.claude/settings.json`.
    pub fn of(cfg: &crate::config::Config) -> Catalog {
        let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::config::home_dir().join(".claude"));
        Catalog::new(
            claude_dir.join("settings.json"),
            cfg.codex_executable.clone().into(),
            cfg.codex_home.clone(),
            cfg.default_models.get(CLAUDE_CODE).cloned(),
        )
    }

    /// `{harness, name, source, default, families, models}`; `None` for a harness with no list.
    pub async fn get(&self, harness: &str, fresh: bool) -> Option<Value> {
        if harness != CLAUDE_CODE && harness != CODEX {
            return None;
        }
        let mut c = self.cache.lock().await;
        if let Some((at, v)) = c.get(harness)
            && !fresh
            && at.elapsed() < TTL
        {
            return Some(v.clone());
        }
        let v = self.load(harness, fresh).await;
        c.insert(harness.to_string(), (Instant::now(), v.clone()));
        Some(v)
    }

    async fn load(&self, harness: &str, fresh: bool) -> Value {
        let (models, source, default) = if harness == CODEX {
            let listed = match self.node.get() {
                Some(n) => node_codex(n.as_ref(), fresh).await,
                None => self.codex_list().await,
            };
            let (models, source) = match listed {
                Some(m) => (m, "codex debug models"),
                None => (
                    CODEX_MODELS.iter().map(|m| Entry::new(m, None)).collect(),
                    BUILTIN,
                ),
            };
            (
                models,
                source,
                steve_core::models::codex_config_model(&self.codex_home),
            )
        } else {
            let p = self.claude_settings.clone();
            let picked = tokio::task::spawn_blocking(move || claude_picker(&p))
                .await
                .ok()
                .flatten();
            let (models, source) = match picked {
                Some(m) => (m, CLAUDE_PICKER),
                None => (
                    CLAUDE_MODELS.iter().map(|m| Entry::new(m, None)).collect(),
                    BUILTIN,
                ),
            };
            (models, source, self.claude_default.clone())
        };
        // Codex's catalogue is ordered by its own priority: its families follow that order (GPT first);
        // the Claude Code picker is alphabetical, so its families follow the fixed order
        let families: Vec<&str> = if harness == CODEX {
            models.iter().fold(vec![], |mut v, m| {
                if !v.contains(&m.family) {
                    v.push(m.family);
                }
                v
            })
        } else {
            FAMILIES
                .iter()
                .copied()
                .filter(|f| models.iter().any(|m| m.family == *f))
                .collect()
        };
        json!({
            "harness": harness,
            "name": steve_core::models::harness_name(harness),
            "source": source,
            "default": default,
            "families": families,
            "models": models.iter().map(Entry::json).collect::<Vec<_>>(),
        })
    }

    /// `codex debug models` with `CODEX_HOME`, 15 s at most.
    async fn codex_list(&self) -> Option<Vec<Entry>> {
        let out = tokio::process::Command::new(&self.codex)
            .env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
            .env_remove("STEVE_SUPERVISOR_PID")
            .env_remove("STEVE_UPGRADE_ID")
            .args(["debug", "models"])
            .env("CODEX_HOME", &self.codex_home)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(Duration::from_secs(15), out)
            .await
            .ok()?
            .ok()?;
        if !out.status.success() {
            return None;
        }
        codex_catalogue(&String::from_utf8_lossy(&out.stdout))
    }
}

/// Codex's models as the local Node's probe reports them. An empty list only when the Node says
/// it has no Codex (no version); `None`, so the built-in list applies as without the Node, when the
/// probe fails, the Node runs no Codex harness, or Codex answers `--version` but lists no models
/// (`debug models` failed, timed out or is unknown to that CLI).
async fn node_codex(node: &dyn steve_node_api::Node, fresh: bool) -> Option<Vec<Entry>> {
    let caps = match node.probe(fresh).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("probe of the local Node: {e}");
            return None;
        }
    };
    let h = caps.harnesses.iter().find(|h| h.name == CODEX)?;
    if h.version.is_empty() {
        return Some(vec![]);
    }
    let models: Vec<Entry> = h
        .models
        .iter()
        .filter(|m| steve_core::models::valid_model(m))
        .map(|m| Entry::new(m, None))
        .collect();
    (!models.is_empty()).then_some(models)
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
        Some(CLAUDE_PICKER) => {
            list["source"] = json!(steve_i18n::t!("hub.models.source_claude_picker"))
        }
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

    /// An executable shell script at `p`.
    fn script(p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }

    /// A catalog whose own `codex` would leave a mark if the Hub ran it.
    fn marked(dir: &Path) -> Catalog {
        let codex = dir.join("hub-codex");
        script(&codex, &format!("touch '{}'", dir.join("ran").display()));
        Catalog::new(dir.join("settings.json"), codex, dir.join(".codex"), None)
    }

    /// A LocalNode with a Codex adapter whose probe asks `codex` (absent unless the test writes it).
    fn node_with_codex(dir: &Path, codex: PathBuf) -> std::sync::Arc<steve_node::LocalNode> {
        let node = steve_node::LocalNode::open(steve_node::NodeConfig {
            node: steve_node_api::NodeId::new("local").unwrap(),
            home: dir.join("node"),
            acp: steve_acp::AcpConfig {
                adapters: HashMap::from([(CODEX.to_string(), dir.join("adapter"))]),
                node: "node".into(),
                claude_executable: dir.join("no-claude"),
                steve_home: dir.join("steve"),
                codex_home: dir.join(".codex"),
                user_home: dir.to_path_buf(),
                version: "test".into(),
                idle_release: Duration::from_secs(600),
                retry_delays: vec![],
            },
            roots: vec![],
            capacity: steve_node_api::Capacity {
                sessions: 0,
                jobs: 0,
                execs: 0,
            },
            storage: Default::default(),
            jobs: Default::default(),
        })
        .unwrap();
        node.set_discovery(steve_node::history::DiscoveryConfig {
            homes: steve_node::history::Homes {
                user_home: dir.to_path_buf(),
                steve_home: dir.join("steve"),
                claude_dir: dir.join(".claude"),
                codex_home: dir.join(".codex"),
            },
            binaries: steve_node::history::Binaries {
                claude: dir.join("no-claude"),
                codex,
            },
        });
        std::sync::Arc::new(node)
    }

    fn ids(v: &Value) -> Vec<String> {
        v["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    }

    fn builtin() -> Vec<String> {
        CODEX_MODELS.iter().map(|m| m.to_string()).collect()
    }

    /// With the local Node, the Hub runs no `codex`; a Node that runs no Codex harness, or whose
    /// probe fails, leaves the built-in list as without the Node.
    #[tokio::test]
    async fn a_node_without_a_codex_harness_keeps_the_builtin_list() {
        let dir = tempfile::tempdir().unwrap();
        let c = marked(dir.path());
        c.set_node(steve_node_api::testkit::FakeNode::new("local"));
        let v = c.get(CODEX, true).await.unwrap();
        assert_eq!(ids(&v), builtin(), "{v}");
        assert_eq!(v["source"], BUILTIN);
        assert!(!dir.path().join("ran").exists(), "the Hub ran codex");
    }

    /// Codex is installed (it has a version) but `debug models` fails: the built-in list, not
    /// none, as the same machine lists without the Node.
    #[tokio::test]
    async fn codex_with_a_version_but_no_models_keeps_the_builtin_list() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("bin/codex");
        script(
            &codex,
            "if [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.40.0'; else exit 2; fi",
        );
        let node = node_with_codex(dir.path(), codex);
        let c = marked(dir.path());
        c.set_node(node.clone());
        let v = c.get(CODEX, true).await.unwrap();
        assert_eq!(ids(&v), builtin(), "{v}");
        assert_eq!(v["source"], BUILTIN);
        assert!(!dir.path().join("ran").exists(), "the Hub ran codex");
        node.shutdown().await;
    }

    /// A Node whose machine has no Codex (no version) lists no Codex models.
    #[tokio::test]
    async fn a_node_without_codex_lists_no_codex_models() {
        let dir = tempfile::tempdir().unwrap();
        let node = node_with_codex(dir.path(), dir.path().join("bin/codex"));
        let c = marked(dir.path());
        c.set_node(node.clone());
        let v = c.get(CODEX, true).await.unwrap();
        assert_eq!(v["models"], json!([]), "{v}");
        assert!(!dir.path().join("ran").exists(), "the Hub ran codex");
        node.shutdown().await;
    }

    /// A Node whose Codex lists models: those, in Codex's order.
    #[tokio::test]
    async fn a_node_with_codex_lists_its_models() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("bin/codex");
        script(
            &codex,
            r#"if [ "$1" = "--version" ]; then echo 'codex-cli 0.80.0'; else echo '{"models":[{"slug":"gpt-5.5","visibility":"list","priority":2},{"slug":"gpt-5.5-codex","visibility":"list","priority":1}]}'; fi"#,
        );
        let node = node_with_codex(dir.path(), codex);
        let c = marked(dir.path());
        c.set_node(node.clone());
        let v = c.get(CODEX, true).await.unwrap();
        assert_eq!(ids(&v), ["gpt-5.5-codex", "gpt-5.5"], "{v}");
        assert_eq!(v["source"], "codex debug models");
        node.shutdown().await;
    }

    /// A settings file with a key helper that would leave a mark if anything ran it, and a key in
    /// `env`: neither may reach the list.
    fn settings(dir: &Path) -> PathBuf {
        let helper = dir.join("helper.sh");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\ntouch '{}'\necho sk-ant-SECRET\n",
                dir.join("ran").display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let p = dir.join("settings.json");
        std::fs::write(
            &p,
            json!({
                "apiKeyHelper": helper.display().to_string(),
                "env": {"ANTHROPIC_API_KEY": "sk-ant-ENVKEY", "ANTHROPIC_BASE_URL": "http://gw"},
                "model": "claude-opus-5-5",
                "modelPicker": {"options": [
                    {"model": "claude-opus-5-5", "label": "claude-opus-5-5", "description": "Example Gateway · claude-opus-5-5"},
                    {"model": "claude-haiku-5-5", "label": "claude-haiku-5-5"},
                    {"model": "kiro/claude-sonnet-5-5", "label": "kiro/claude-sonnet-5-5"},
                    {"model": "gpt-6-luna"},
                    {"model": "gemini-3-flash"},
                    {"model": "bad model"},
                    {"label": "no id"},
                    {"model": "claude-opus-5-5"},
                ]},
            })
            .to_string(),
        )
        .unwrap();
        p
    }

    #[tokio::test]
    async fn claude_lists_the_picker_and_never_the_keys() {
        let d = tempfile::tempdir().unwrap();
        let c = Catalog::new(
            settings(d.path()),
            d.path().join("no-codex"),
            d.path().into(),
            Some("claude-opus-5-5".into()),
        );
        let v = c.get(CLAUDE_CODE, false).await.unwrap();
        let ids: Vec<&str> = v["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "claude-opus-5-5",
                "claude-haiku-5-5",
                "kiro/claude-sonnet-5-5",
                "gpt-6-luna",
                "gemini-3-flash"
            ]
        );
        assert_eq!(v["models"][2]["family"], "Claude Sonnet");
        assert_eq!(v["models"][2]["via"], "kiro");
        assert_eq!(
            v["families"],
            json!([
                "Claude Opus",
                "Claude Sonnet",
                "Claude Haiku",
                "GPT",
                "Gemini"
            ])
        );
        assert_eq!(v["default"], "claude-opus-5-5");
        assert!(v["source"].as_str().unwrap().contains("settings.json"));
        let text = v.to_string();
        for secret in ["sk-ant", "helper.sh", "apiKeyHelper", "ENVKEY", "http://gw"] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
        assert!(!d.path().join("ran").exists(), "the key helper was run");
        // no picker: the built-in list
        std::fs::write(d.path().join("settings.json"), r#"{"apiKeyHelper": "x"}"#).unwrap();
        let v = c.get(CLAUDE_CODE, true).await.unwrap();
        assert_eq!(v["source"], "内置列表");
        assert_eq!(v["models"].as_array().unwrap().len(), CLAUDE_MODELS.len());
        assert!(c.get("gemini", false).await.is_none());
    }

    #[tokio::test]
    async fn codex_reads_its_catalogue_and_falls_back() {
        let d = tempfile::tempdir().unwrap();
        let cat = json!({"models": [
            {"slug": "gpt-6.1-sol", "display_name": "GPT-6.1-Sol", "visibility": "list", "priority": 1, "description": "Latest. [warning 2026-10-26: 上次审计有间歇失败]", "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}]},
            {"slug": "gpt-6-luna", "display_name": "GPT-6-Luna", "visibility": "list", "priority": 5},
            {"slug": "chat_x", "visibility": "hide", "priority": 5},
            {"slug": "claude-sonnet-5-5", "visibility": "list", "priority": 5},
        ]});
        std::fs::write(d.path().join("cat.json"), cat.to_string()).unwrap();
        std::fs::write(d.path().join("config.toml"), "model = \"gpt-6-astra\"\n").unwrap();
        let exe = d.path().join("codex");
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\n[ \"$1 $2\" = 'debug models' ] && [ \"$CODEX_HOME\" = '{0}' ] && cat '{0}/cat.json'\n",
                d.path().display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let c = Catalog::new(d.path().join("none.json"), exe, d.path().into(), None);
        let v = c.get(CODEX, false).await.unwrap();
        assert_eq!(v["source"], "codex debug models");
        assert_eq!(v["default"], "gpt-6-astra");
        let ids: Vec<&str> = v["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["gpt-6.1-sol", "claude-sonnet-5-5", "gpt-6-luna"]);
        assert_eq!(v["models"][0]["label"], "GPT-6.1-Sol");
        assert_eq!(v["models"][0]["efforts"], json!(["low", "high"]));
        assert_eq!(
            v["models"][0]["note"],
            "warning 2026-10-26: 上次审计有间歇失败"
        );
        // with what the statistics saw
        let v = with_usage(
            v,
            &json!({"gpt-6-luna": {"calls": 3, "ok_at": 5, "failed_at": null}, "gpt-old": {"calls": 1, "ok_at": null, "failed_at": 9}}),
        );
        assert_eq!(v["models"][2]["used"]["calls"], 3);
        assert_eq!(v["models"][3]["id"], "gpt-old");
        assert_eq!(v["models"][3]["note"], "最近用过，不在列表里");
        // codex missing: the built-in list
        let c = Catalog::new(
            d.path().join("none.json"),
            d.path().join("nope"),
            d.path().into(),
            None,
        );
        let v = c.get(CODEX, false).await.unwrap();
        assert_eq!(v["source"], "内置列表");
        assert_eq!(v["models"][0]["id"], CODEX_MODELS[0]);
    }

    #[test]
    fn families() {
        assert_eq!(family("claude-opus-4-8"), "Claude Opus");
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
