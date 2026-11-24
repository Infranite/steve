//! Harness parity guard (harness parity S0), in ratchet mode: production code chooses behaviour
//! by what an adapter declares or what ACP advertises, never by a harness's name. Every place that
//! still names one is listed in `tests/harness_parity.txt` (file, pattern, count); a new one fails
//! the test, and so does a listed one that is gone, so the list only shrinks. The adapter layer,
//! where the built-in adapters are described as data, is the one place names belong
//! (`steve-acp/src/config.rs`, `steve-acp/src/adapters.rs`).
//!
//! It reads every workspace crate's production sources (the module tree from `src/lib.rs` and
//! `src/main.rs`, without comments and test-only items) with the Node boundary guard's reader.
//! `STEVE_GUARD_BLESS=1 cargo test -p steve-hub --test harness_parity` rewrites the list.

#[path = "support/ratchet.rs"]
mod ratchet;

#[path = "node_boundary/scan.rs"]
#[allow(dead_code)]
mod scan;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A harness's name or CLI binary as a string literal.
const LITERALS: &[&str] = &[
    "\"claude-code\"",
    "\"claude-agent-acp\"",
    "\"claude-code-acp\"",
    "\"codex\"",
    "\"codex-acp\"",
    "\"claude\"",
    "\"claude-cli\"",
    "\"codex-cli\"",
    "\"pi\"",
    "\"openma-pi-acp\"",
    "\"gemini\"",
    "\"gemini-cli\"",
    "\"github-copilot-cli\"",
    "\"copilot\"",
    "\"opencode\"",
    "\"qwen-code\"",
    "\"qwen\"",
    "\"goose\"",
    "\"goose-acp\"",
    "\"kimi-code\"",
    "\"kimi\"",
    "\"grok-build\"",
    "\"grok\"",
    "\"kilo\"",
    "\"factory-droid\"",
    "\"droid\"",
    "\"codebuddy-code\"",
    "\"codebuddy\"",
    "\"cursor\"",
    "\"cursor-agent\"",
    "\"kiro\"",
    "\"kiro-cli\"",
    "\"mistral-vibe\"",
    "\"vibe\"",
    "\"vibe-acp\"",
    "\"qoder\"",
    "\"qodercli\"",
    "\"antigravity\"",
    "\"agy\"",
];

/// Code that branches on a harness by name.
const CODE: &[&str] = &[
    "models::CODEX",
    "CLAUDE_CODE",
    "Harness::Claude",
    "Harness::Codex",
    "harness ==",
    "harness !=",
    "codex: bool",
];

/// Where the built-in adapters are described as data.
const ADAPTER_LAYER: &[&str] = &["steve-acp/src/config.rs", "steve-acp/src/adapters.rs"];

const LIST: &str = "tests/harness_parity.txt";

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// `cursor` is also a pagination field in JSON protocols, not a choice of Cursor's adapter.
/// Only object keys inside `json!` are exempt: values, comparisons and lookups still count.
fn pagination_key(raw: &str, code: &str, at: usize, literal: &str) -> bool {
    if literal != "\"cursor\"" || !raw[at + literal.len()..].trim_start().starts_with(':') {
        return false;
    }
    let mut braces = Vec::new();
    for (i, b) in code.bytes().take(at).enumerate() {
        match b {
            b'{' => braces.push(i),
            b'}' => {
                braces.pop();
            }
            _ => {}
        }
    }
    braces.into_iter().any(|i| {
        let prefix: String = code[..i].chars().filter(|c| !c.is_whitespace()).collect();
        prefix.strip_suffix("json!(").is_some_and(|head| {
            head.chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        })
    })
}

#[test]
fn pagination_keys_are_not_adapter_names_but_values_and_branches_are() {
    for (raw, exempt) in [
        (r#"json!({"cursor": c})"#, true),
        (r#"serde_json::json ! ( { "cursor" : c })"#, true),
        (r#"json!({"page": {"cursor": c}})"#, true),
        (r#"custom_json!({"cursor": c})"#, false),
        (r#"json!({"harness": "cursor"})"#, false),
        (r#"harness == "cursor""#, false),
        (r#"harnesses.get("cursor")"#, false),
        (
            r#"match harness { "cursor" => run(), _ => other() }"#,
            false,
        ),
    ] {
        let code = scan::production(raw).unwrap();
        let at = raw.find("\"cursor\"").unwrap();
        assert_eq!(
            pagination_key(raw, &code, at, "\"cursor\""),
            exempt,
            "{raw}"
        );
    }
}

fn found() -> BTreeMap<(String, String), usize> {
    let mut names: Vec<String> = std::fs::read_dir(crates_dir())
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("Cargo.toml").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut out = BTreeMap::new();
    for n in names {
        for s in scan::crate_sources(&crates_dir(), &n) {
            if ADAPTER_LAYER.contains(&s.rel.as_str()) {
                continue;
            }
            let raw = std::fs::read_to_string(crates_dir().join(&s.rel)).unwrap();
            let code = s.code.as_bytes();
            // a literal counts where the reader kept its opening quote (production code, not a
            // comment or a test)
            for lit in LITERALS {
                let n = raw
                    .match_indices(lit)
                    .filter(|(at, _)| code.get(*at) == Some(&b'"'))
                    .filter(|(at, _)| !pagination_key(&raw, &s.code, *at, lit))
                    .count();
                if n > 0 {
                    out.insert((s.rel.clone(), lit.to_string()), n);
                }
            }
            for pat in CODE {
                let n = scan::find_in(&s.code, pat).len();
                if n > 0 {
                    out.insert((s.rel.clone(), pat.to_string()), n);
                }
            }
        }
    }
    out
}

#[test]
fn literals_cover_all_builtin_adapter_ids_and_bins() {
    for id in steve_harness_spec::builtin_ids() {
        let lit = format!("\"{id}\"");
        assert!(
            LITERALS.contains(&lit.as_str()),
            "missing builtin adapter id literal {lit} in LITERALS"
        );
    }
    for (_id, cli_bin, detect_bins) in steve_harness_spec::builtin_bins() {
        for bin in std::iter::once(cli_bin).chain(detect_bins.iter().copied()) {
            let lit = format!("\"{bin}\"");
            assert!(
                LITERALS.contains(&lit.as_str()),
                "missing builtin binary literal {lit} in LITERALS"
            );
        }
    }
}

#[test]
fn harnesses_are_not_told_apart_by_name_outside_the_adapter_layer() {
    let now = found();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(LIST);
    ratchet::check(
        now.into_iter()
            .map(|((file, pattern), n)| (vec![file, pattern], n))
            .collect(),
        &path,
        2,
        "# Places that still tell harnesses apart by name; only shrinks.\n# file<TAB>pattern<TAB>count. STEVE_GUARD_BLESS=1 rewrites after removals.\n",
        "choose by a declared adapter property or an ACP capability",
    );
}
