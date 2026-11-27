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
        let id = entry["id"].as_str().unwrap_or_default().to_owned();
        let reads = matches!(id.as_str(), "claude-code" | "codex");
        entry["properties"]["read_attached"] = json!(reads);
        entry["properties"]["effort_mode"] = json!("acp_option");
        if id == "pi" {
            entry["properties"]["fallback_efforts"] =
                json!(["low", "medium", "high", "xhigh", "max"]);
        } else if id == "antigravity" {
            entry["properties"]["fallback_efforts"] = json!(["low", "medium", "high", "xhigh"]);
        }
        // Card 1 adds declarative native `append_system_context` for empirically verified harnesses.
        entry["properties"]["append_system_context"] = match id.as_str() {
            "claude-code" | "codex" => json!({"kind": "acp_system_prompt"}),
            "antigravity" => json!({"kind": "meta_append", "namespace": "antigravity"}),
            _ => Value::Null,
        };
        if id == "claude-code" {
            entry["properties"]["state"]["login"]["entries"] = json!([
                "apiKeyHelper",
                "awsAuthRefresh",
                "awsCredentialExport",
                "forceLoginMethod",
                "forceLoginOrgUUID",
                "env.ANTHROPIC_*",
                "env.CLAUDE_CODE_USE_*",
                "env.CLAUDE_CODE_AUTO_MODE_*",
                "env.CLAUDE_CODE_BG_CLASSIFIER_MODEL",
                "env.CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
                "env.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
                "env.CLAUDE_CODE_DISABLE_FEATURE_FLAGS",
                "env.DISABLE_GROWTHBOOK",
                "env.DISABLE_TELEMETRY",
                "env.DO_NOT_TRACK",
                "env.CLAUDE_CODE_GB_DISK_CACHE_WHEN_TELEMETRY_OFF",
                "env.HTTP_PROXY",
                "env.HTTPS_PROXY",
                "env.ALL_PROXY",
                "env.NO_PROXY",
                "env.http_proxy",
                "env.https_proxy",
                "env.all_proxy",
                "env.no_proxy",
                "env.CLAUDE_CODE_HTTP_PROXY",
                "env.CLAUDE_CODE_HTTPS_PROXY",
                "env.CLAUDE_CODE_PROXY_*",
                "env.SSL_CERT_FILE",
                "env.SSL_CERT_DIR",
                "env.NODE_EXTRA_CA_CERTS",
                "env.CLAUDE_CODE_CERT_STORE",
                "env.CLAUDE_CODE_CLIENT_CERT",
                "env.CLAUDE_CODE_CLIENT_KEY",
                "env.CLAUDE_CODE_CLIENT_KEY_PASSPHRASE",
                "env.AWS_*",
                "env.CLOUD_ML_REGION",
                "env.VERTEX_REGION_*"
            ]);
        }
        // #583 replaces hardcoded SessionMeta enum with declarative protected_deny, brand_mark,
        // model_strip_prefixes and history_prefix metadata.
        entry["properties"]
            .as_object_mut()
            .unwrap()
            .remove("session_meta");
        entry["properties"]["protected_deny"] = if id == "claude-code" {
            json!({
                "wrapper": ["claudeCode", "options", "settings", "permissions", "deny"],
                "tools": ["Read", "Edit"],
                "abs_prefix": "/",
                "dir_suffix": "/**",
            })
        } else {
            Value::Null
        };
        entry["properties"]["brand_mark"] = json!(match id.as_str() {
            "claude-code" => "anthropic",
            "codex" => "openai",
            "gemini" | "antigravity" => "google",
            "github-copilot-cli" => "github",
            "opencode" => "sst",
            "qwen-code" => "alibaba",
            "kimi-code" => "moonshot",
            "codebuddy-code" => "tencent",
            "cursor" => "anysphere",
            "mistral-vibe" => "mistral",
            _ => "other",
        });
        entry["properties"]["model_strip_prefixes"] = if id == "claude-code" {
            json!(["claude-"])
        } else {
            json!([])
        };
        entry["properties"]["history_prefix"] = match id.as_str() {
            "claude-code" => json!("cc"),
            "codex" => json!("cx"),
            _ => Value::Null,
        };
        entry["properties"]["state_settings"] = match id.as_str() {
            "antigravity" => json!("antigravity_settings"),
            _ => json!("none"),
        };
        // #620 runs Codex inside Steve's OS sandbox and uses Steve's read-only sandbox for research.
        if id == "codex" {
            entry["properties"]["self_read_only"] = json!(false);
            entry["properties"]["nested_sandbox"] = json!(true);
        }
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
    known_keys(&catalog, &["adapters", "ui"], "catalog");
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
                "brand_substrings",
                "brand_words",
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
        for field in ["modes", "mode_meta", "mode_args"] {
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
                    "arg",
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
    let ui = &catalog["ui"];
    known_keys(ui, &["model_brands", "mcp_clients"], "catalog.ui");
    for rule in ui["model_brands"].as_array().unwrap() {
        known_keys(
            rule,
            &["mark", "substrings", "words"],
            "catalog.ui.model_brands",
        );
    }
    for client in ui["mcp_clients"].as_array().unwrap() {
        known_keys(
            client,
            &[
                "id",
                "language",
                "file",
                "wrap",
                "template",
                "command_template",
            ],
            "catalog.ui.mcp_clients",
        );
    }
}

#[test]
fn web_harness_meta_json_matches_builtin_catalog() {
    let generated = ui_metadata_json();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../web/src/gen/harness-meta.json");
    if std::env::var_os("STEVE_UPDATE_HARNESS_META").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &generated).unwrap();
    }
    let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing {} ({e}); run STEVE_UPDATE_HARNESS_META=1 cargo test -p steve-harness-spec",
            path.display()
        )
    });
    assert_eq!(
        actual, generated,
        "web/src/gen/harness-meta.json is out of sync with builtins.toml; run STEVE_UPDATE_HARNESS_META=1 cargo test -p steve-harness-spec"
    );
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

#[test]
fn only_declared_native_approval_modes_are_offered() {
    let native = [
        ("claude-code", "auto"),
        ("codex", "agent"),
        ("goose", "smart_approve"),
        ("mistral-vibe", "smart-approve"),
        ("antigravity", "auto"),
    ];
    for builtin in builtins() {
        let want = native.iter().find(|(id, _)| *id == builtin.id);
        let props = &builtin.properties;
        assert_eq!(
            props.native_approval().is_some(),
            want.is_some(),
            "{}",
            builtin.id
        );
        if let Some((_, id)) = want {
            use steve_node_api::PermissionMode;
            let offered = [id.to_string(), props.modes.full_access.clone()];
            assert_eq!(
                props
                    .modes
                    .choice(PermissionMode::Auto)
                    .resolve(Some(&offered)),
                Ok(Some((*id).into())),
                "{}",
                builtin.id
            );
            assert!(
                props
                    .modes
                    .choice(PermissionMode::Auto)
                    .resolve(Some(&offered[1..]))
                    .is_err()
            );
            assert_eq!(props.modes.id(PermissionMode::Auto), *id);
        }
    }
    for mode in ["", "default", "bypassPermissions", "custom-reviewed"] {
        let declaration: Declaration = serde_json::from_value(json!({
            "properties": {"native_approval": "reject", "modes":{"auto":mode}}
        }))
        .unwrap();
        let props = spec("custom-harness", Some(&declaration)).properties;
        assert_eq!(
            props.native_approval(),
            (mode == "custom-reviewed").then_some(NativeApprovalFallback::Reject)
        );
    }

    // Full access covers modes.full_access, mode_args.full_access, mode_meta.full_access, and native-only;
    // aliases of ask (like opencode's "build") and empty declarations are unsupported.
    assert_eq!(
        builtin("claude-code")
            .unwrap()
            .properties
            .full_access_mode()
            .as_deref(),
        Some("bypassPermissions")
    );
    assert_eq!(
        builtin("codex")
            .unwrap()
            .properties
            .full_access_mode()
            .as_deref(),
        Some("agent-full-access")
    );
    assert_eq!(
        builtin("antigravity")
            .unwrap()
            .properties
            .full_access_mode()
            .as_deref(),
        Some("--dangerously-skip-permissions")
    );
    assert_eq!(
        builtin("opencode").unwrap().properties.full_access_mode(),
        None
    );
    assert_eq!(builtin("kilo").unwrap().properties.full_access_mode(), None);
}

#[test]
fn verified_append_system_context_declarations_match_contract() {
    let claude = builtin("claude-code").unwrap();
    assert_eq!(
        claude.properties.append_system_context,
        Some(AppendSystemContext::AcpSystemPrompt)
    );
    let codex = builtin("codex").unwrap();
    assert_eq!(
        codex.properties.append_system_context,
        Some(AppendSystemContext::AcpSystemPrompt)
    );
    let agy = builtin("antigravity").unwrap();
    assert_eq!(
        agy.properties.append_system_context,
        Some(AppendSystemContext::MetaAppend {
            namespace: "antigravity".into(),
        })
    );
    // Unverified or unsupported harnesses must NOT declare native append_system_context.
    assert_eq!(
        builtin("kimi-code")
            .unwrap()
            .properties
            .append_system_context,
        None
    );
    assert_eq!(
        builtin("pi").unwrap().properties.append_system_context,
        None
    );

    // Custom manifest declarations can specify cli_flag or meta_append.
    let decl: Declaration = serde_json::from_value(json!({
        "properties": {
            "append_system_context": {
                "kind": "cli_flag",
                "flag": "--append-system-prompt"
            }
        }
    }))
    .unwrap();
    assert_eq!(
        spec("custom-cli", Some(&decl))
            .properties
            .append_system_context,
        Some(AppendSystemContext::CliFlag {
            flag: "--append-system-prompt".into(),
        })
    );
}

#[test]
fn antigravity_declares_nested_sandbox_native_auto_full_access_and_state_settings() {
    let agy = builtin("antigravity").unwrap();
    assert!(agy.properties.nested_sandbox);
    assert_eq!(agy.properties.modes.auto, "auto");
    assert_eq!(agy.properties.mode_args.get("auto"), None);
    assert_eq!(
        agy.properties.mode_args.get("full_access"),
        Some(&vec!["--dangerously-skip-permissions".to_string()])
    );
    assert_eq!(
        agy.properties.native_approval(),
        Some(NativeApprovalFallback::AskOwner)
    );
    assert_eq!(
        agy.properties.state_settings,
        StateSettings::AntigravitySettings
    );
    let override_entry: AdapterEntry = serde_json::from_value(json!({
        "command": "/fixture/adapter",
        "name": "Own",
        "properties": { "state_settings": "antigravity_settings" }
    }))
    .unwrap();
    let (_, declared) = split_entries([("own".into(), override_entry)]);
    assert_eq!(
        spec("own", declared.get("own")).properties.state_settings,
        StateSettings::AntigravitySettings
    );
}

#[test]
fn codex_declares_nested_sandbox_without_self_read_only() {
    let codex = builtin("codex").unwrap();
    assert!(codex.properties.nested_sandbox);
    assert!(!codex.properties.self_read_only);
    assert!(codex.properties.outer_sandbox_args.is_empty());
    assert_eq!(codex.properties.modes.plan, "read-only");
    assert_eq!(codex.properties.modes.ask, "read-only");
    assert_eq!(codex.properties.modes.accept_edits, "workspace-write");
    assert_eq!(codex.properties.modes.auto, "agent");
    assert_eq!(codex.properties.modes.full_access, "agent-full-access");
    assert_eq!(
        codex.properties.native_approval(),
        Some(NativeApprovalFallback::AskOwner)
    );
}
