use serde_json::{Value, json};
use steve_harness_spec::*;

// Captured from the old ACP registry at c8af7504 through cargo nextest before extraction.
#[test]
fn the_catalog_preserves_the_pre_extraction_registry() {
    assert_eq!(*DEFAULT_ADAPTER, ("claude-code", "claude-agent-acp"));
    assert_eq!(*DEFAULT_MANAGED_ADAPTERS, vec!["claude-code", "codex"]);
    assert_eq!(builtins().iter().filter(|b| b.primary).count(), 1);
    let mut expected: Value = serde_json::from_str(include_str!("fixtures/registry.json")).unwrap();
    // Environments are maps in both manifest forms. Their serialization order does not change
    // the variables passed to a process; compare each old array in its canonical key order.
    for entry in expected.as_array_mut().unwrap() {
        // #505 adds a fail-closed gateway capability to the extracted registry. Keep the
        // old snapshot intact and check which built-ins explicitly opt in.
        let reads = matches!(entry["id"].as_str(), Some("claude-code" | "codex"));
        entry["properties"]["read_attached"] = json!(reads);
        entry["env"]
            .as_array_mut()
            .unwrap()
            .sort_by_key(|v| v[0].as_str().unwrap().to_owned());
    }
    let actual: Vec<_> = builtins()
        .iter()
        .map(|b| {
            json!({
                "id": b.id, "name": b.name, "install_hint": b.install_hint,
                "npm_package": b.npm_package, "cli_bin": b.cli_bin, "detect_bins": b.detect_bins,
                "args": b.args, "env": b.env, "command_var": b.command_var,
                "properties": b.properties, "components": components(b.id),
            })
        })
        .collect();
    assert_eq!(json!(actual), expected);
}

#[test]
fn patch_keys_and_property_fields_have_one_schema() {
    let properties = serde_json::to_value(AdapterProperties::default()).unwrap();
    let keys: std::collections::BTreeSet<_> = properties
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "local_transcript")
        .collect();
    assert_eq!(keys, PROPERTY_KEYS.into_iter().collect());
    let override_entry: AdapterEntry = serde_json::from_value(json!({"command":"/fixture/adapter", "name":"Own", "properties":{"nested_sandbox":false,"modes":{"plan":"inspect"}}})).unwrap();
    let (_, declared) = split_entries([("own".into(), override_entry)]);
    let p = spec("own", declared.get("own"));
    assert_eq!(p.name, "Own");
    assert!(!p.properties.nested_sandbox);
    assert_eq!(p.properties.modes.plan, "inspect");
    assert_eq!(p.properties.modes.ask, "default");
}

fn known_keys(value: &toml::Value, keys: &[&str], location: &str) {
    for key in value.as_table().unwrap().keys() {
        assert!(
            keys.contains(&key.as_str()),
            "unknown catalog key {location}.{key}"
        );
    }
}

#[test]
fn every_builtin_catalog_key_is_known() {
    let catalog: toml::Value = toml::from_str(include_str!("../src/builtins.toml")).unwrap();
    known_keys(&catalog, &["adapters"], "catalog");
    for adapter in catalog["adapters"].as_array().unwrap() {
        let id = adapter["id"].as_str().unwrap();
        known_keys(
            adapter,
            &[
                "id",
                "cli_bin",
                "detect_bins",
                "npm_package",
                "command_var",
                "command",
                "local_transcript",
                "primary",
                "managed_by_default",
                "components",
                "name",
                "args",
                "env",
                "properties",
                "install_hint",
            ],
            id,
        );
        // Tagged command variants reject fields belonging to no variant, as well as typos.
        adapter["command"]
            .clone()
            .try_into::<DefaultCommand>()
            .unwrap();
        let properties = &adapter["properties"];
        known_keys(properties, &PROPERTY_KEYS, &format!("{id}.properties"));
        for field in ["modes", "mode_meta"] {
            if let Some(value) = properties.get(field) {
                known_keys(
                    value,
                    &["plan", "ask", "accept_edits", "auto", "full_access"],
                    &format!("{id}.properties.{field}"),
                );
            }
        }
        if let Some(state) = properties.get("state") {
            known_keys(
                state,
                &[
                    "env",
                    "default",
                    "import",
                    "context",
                    "login",
                    "home_override",
                    "import_into",
                    "xdg",
                    "extra_dirs",
                    "home_files",
                    "home",
                ],
                &format!("{id}.properties.state"),
            );
            if let Some(login) = state.get("login") {
                known_keys(
                    login,
                    &["files", "renew", "store_env", "settings", "entries"],
                    &format!("{id}.properties.state.login"),
                );
            }
        }
        for component in adapter
            .get("components")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            known_keys(
                component,
                &[
                    "id",
                    "name",
                    "package",
                    "entry",
                    "node_min",
                    "executable_env",
                    "required",
                    "minimum",
                ],
                &format!("{id}.components"),
            );
        }
        // env and protocol metadata payloads are intentionally open maps, not catalog fields.
    }
}

#[test]
fn ignored_mode_keys_are_available_to_caller_diagnostics() {
    let declaration: Declaration = serde_json::from_value(json!({
        "properties": {"modes": {"plan": "inspect", "plna": "mistyped"}}
    }))
    .unwrap();
    assert_eq!(
        declaration.ignored_mode_keys().collect::<Vec<_>>(),
        ["plna"]
    );
    assert_eq!(
        spec("own", Some(&declaration)).properties.modes.plan,
        "inspect"
    );
}
