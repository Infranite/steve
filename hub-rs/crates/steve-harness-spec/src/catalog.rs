#[derive(Debug, Clone, Serialize)]
pub struct Component {
    pub id: &'static str,
    pub name: &'static str,
    pub package: &'static str,
    /// Entry point relative to `node_modules` (`.bin` is npm's stable CLI link).
    pub entry: &'static str,
    pub node_min: (u64, u64, u64),
    pub executable_env: Option<&'static str>,
    pub required: &'static [&'static str],
    pub minimum: Option<&'static str>,
}

use super::*;
use std::sync::LazyLock;

/// How the runtime resolves a trusted built-in command. No path is read here.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DefaultCommand {
    Claude,
    Codex,
    NpmEntry {
        package: String,
        entry: String,
        windows_entry: Option<String>,
    },
    NpmBin {
        package: String,
        bin: String,
    },
    Path {
        bin: String,
    },
}

#[derive(Deserialize)]
struct ComponentDeclaration {
    id: String,
    name: String,
    package: String,
    entry: String,
    node_min: (u64, u64, u64),
    executable_env: Option<String>,
    #[serde(default)]
    required: Vec<String>,
    minimum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandRule {
    pub mark: BrandMark,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub substrings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpClientLanguage {
    Json,
    Bash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpClientSpec {
    pub id: String,
    pub language: McpClientLanguage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default)]
    pub wrap: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_template: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiAdapterMeta {
    pub name: String,
    pub brand_mark: BrandMark,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub brand_substrings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub brand_words: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_strip_prefixes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiMetadata {
    pub adapters: BTreeMap<String, UiAdapterMeta>,
    pub model_brands: Vec<BrandRule>,
    pub mcp_clients: Vec<McpClientSpec>,
}

#[derive(Deserialize)]
struct UiDeclaration {
    #[serde(default)]
    model_brands: Vec<BrandRule>,
    #[serde(default)]
    mcp_clients: Vec<McpClientSpec>,
}

#[derive(Deserialize)]
struct BuiltinDeclaration {
    id: String,
    cli_bin: String,
    #[serde(default)]
    detect_bins: Vec<String>,
    npm_package: Option<String>,
    command_var: String,
    command: DefaultCommand,
    #[serde(default)]
    local_transcript: bool,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    managed_by_default: bool,
    #[serde(default)]
    brand_substrings: Vec<String>,
    #[serde(default)]
    brand_words: Vec<String>,
    #[serde(default)]
    components: Vec<ComponentDeclaration>,
    /// Trusted launch flags owned by the compile-time built-in catalog (never accepted from
    /// operator or API manifests).
    #[serde(default)]
    env: BTreeMap<String, String>,
    // The very same serde structure used by AdapterEntry::Declared in harness.json.
    #[serde(flatten)]
    declaration: Declaration,
}
#[derive(Deserialize)]
struct Catalog {
    adapters: Vec<BuiltinDeclaration>,
    ui: UiDeclaration,
}

/// The immutable catalog view. Borrowed fields preserve the existing registry API, including
/// Component's static package references. Only the compile-time catalog is interned once;
/// operator manifests are ordinary owned Declarations and never enter this storage.
pub struct Builtin {
    pub id: &'static str,
    pub name: &'static str,
    pub install_hint: &'static str,
    pub npm_package: Option<&'static str>,
    pub cli_bin: &'static str,
    pub detect_bins: Vec<&'static str>,
    pub args: Vec<&'static str>,
    pub env: Vec<(&'static str, &'static str)>,
    pub command_var: &'static str,
    pub command: DefaultCommand,
    pub properties: AdapterProperties,
    pub primary: bool,
    pub managed_by_default: bool,
    pub brand_substrings: Vec<&'static str>,
    pub brand_words: Vec<&'static str>,
    pub components: Vec<Component>,
}
fn intern(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}
fn intern_list(v: Vec<String>) -> Vec<&'static str> {
    v.into_iter().map(intern).collect()
}
impl From<ComponentDeclaration> for Component {
    fn from(c: ComponentDeclaration) -> Self {
        Self {
            id: intern(c.id),
            name: intern(c.name),
            package: intern(c.package),
            entry: intern(c.entry),
            node_min: c.node_min,
            executable_env: c.executable_env.map(intern),
            minimum: c.minimum.map(intern),
            required: Box::leak(intern_list(c.required).into_boxed_slice()),
        }
    }
}
impl From<BuiltinDeclaration> for Builtin {
    fn from(b: BuiltinDeclaration) -> Self {
        let mut properties = AdapterProperties::default();
        b.declaration.properties.apply(&mut properties);
        properties.local_transcript = b.local_transcript;
        Self {
            id: intern(b.id),
            name: intern(b.declaration.name.expect("built-in name")),
            install_hint: intern(b.declaration.install_hint.expect("built-in install hint")),
            npm_package: b.npm_package.map(intern),
            cli_bin: intern(b.cli_bin),
            detect_bins: intern_list(b.detect_bins),
            args: intern_list(b.declaration.args.unwrap_or_default()),
            env: b
                .env
                .into_iter()
                .map(|(k, v)| (intern(k), intern(v)))
                .collect(),
            command_var: intern(b.command_var),
            command: b.command,
            properties,
            primary: b.primary,
            managed_by_default: b.managed_by_default,
            brand_substrings: intern_list(b.brand_substrings),
            brand_words: intern_list(b.brand_words),
            components: b.components.into_iter().map(Component::from).collect(),
        }
    }
}
static PARSED: LazyLock<(Vec<Builtin>, UiMetadata)> = LazyLock::new(|| {
    let catalog: Catalog =
        toml::from_str(include_str!("builtins.toml")).expect("valid built-in declarations");
    let builtins: Vec<Builtin> = catalog.adapters.into_iter().map(Builtin::from).collect();
    let mut adapters = BTreeMap::new();
    for b in &builtins {
        adapters.insert(
            b.id.to_owned(),
            UiAdapterMeta {
                name: b.name.to_owned(),
                brand_mark: b.properties.brand_mark,
                brand_substrings: b.brand_substrings.iter().map(|s| (*s).to_owned()).collect(),
                brand_words: b.brand_words.iter().map(|s| (*s).to_owned()).collect(),
                model_strip_prefixes: b.properties.model_strip_prefixes.clone(),
            },
        );
    }
    let ui = UiMetadata {
        adapters,
        model_brands: catalog.ui.model_brands,
        mcp_clients: catalog.ui.mcp_clients,
    };
    (builtins, ui)
});
pub fn builtins() -> &'static [Builtin] {
    &PARSED.0
}
pub fn ui_metadata() -> &'static UiMetadata {
    &PARSED.1
}
pub fn ui_metadata_json() -> String {
    let mut out = serde_json::to_string_pretty(ui_metadata()).expect("ui metadata json");
    out.push('\n');
    out
}
pub fn builtin(id: &str) -> Option<&'static Builtin> {
    builtins().iter().find(|b| b.id == id)
}
pub fn builtin_by_history_prefix(prefix: &str) -> Option<&'static Builtin> {
    builtins()
        .iter()
        .find(|b| b.properties.history_prefix.as_deref() == Some(prefix))
}
pub fn builtin_ids() -> impl Iterator<Item = &'static str> {
    builtins().iter().map(|b| b.id)
}
pub fn builtin_bins() -> impl Iterator<Item = (&'static str, &'static str, &'static [&'static str])>
{
    builtins()
        .iter()
        .map(|b| (b.id, b.cli_bin, b.detect_bins.as_slice()))
}
/// Id and command environment key without resolving or looking up any command.
pub fn builtin_command_vars() -> impl Iterator<Item = (&'static str, &'static str)> {
    builtins().iter().map(|b| (b.id, b.command_var))
}
pub fn builtin_packages() -> impl Iterator<Item = (&'static str, &'static str)> {
    builtins()
        .iter()
        .filter_map(|b| b.npm_package.map(|p| (b.id, p)))
}
pub fn is_npm_managed(id: &str) -> bool {
    builtin(id).is_some_and(|b| b.npm_package.is_some())
}
pub fn components(id: &str) -> &'static [Component] {
    builtin(id).map(|b| b.components.as_slice()).unwrap_or(&[])
}
/// Default selection is data in the catalog; preserve tuple-field access for current callers.
pub static DEFAULT_ADAPTER: LazyLock<(&'static str, &'static str)> = LazyLock::new(|| {
    let b = builtins()
        .iter()
        .find(|b| b.primary)
        .expect("one primary built-in");
    (b.id, b.cli_bin)
});
pub static DEFAULT_MANAGED_ADAPTERS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    builtins()
        .iter()
        .filter(|b| b.managed_by_default)
        .map(|b| b.id)
        .collect()
});
pub const DEFAULT_MODEL_VAR: &str = "STEVE_DEFAULT_MODEL_CLAUDE";
pub fn implicit_selection(id: &str) -> bool {
    DEFAULT_MANAGED_ADAPTERS.contains(&id)
}
/// Built-in defaults overlaid by the same declaration structure accepted from harness.json.
pub fn spec(id: &str, declared: Option<&Declaration>) -> AdapterSpec {
    let builtin = builtin(id);
    let mut properties = builtin.map(|b| b.properties.clone()).unwrap_or_default();
    let mut name = builtin
        .map(|b| b.name.to_owned())
        .unwrap_or_else(|| id.to_owned());
    let mut install_hint = builtin.map(|b| b.install_hint.to_owned());
    if let Some(d) = declared {
        d.properties.apply(&mut properties);
        if let Some(n) = d.name.as_ref().filter(|n| !n.trim().is_empty()) {
            name = n.clone();
        }
        if d.install_hint.is_some() {
            install_hint = d.install_hint.clone();
        }
    }
    AdapterSpec {
        id: id.to_owned(),
        name,
        builtin: builtin.is_some(),
        properties,
        install_hint,
    }
}
