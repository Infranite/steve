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
