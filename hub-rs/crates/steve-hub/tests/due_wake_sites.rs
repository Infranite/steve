//! Production DomainEvent subscriptions are reviewed sites, never implicit due-loop wakeups.
//! This syntactic ratchet counts tap calls regardless of the receiver spelling. It does not
//! infer types or prove that an existing subscriber is a valid consumer; the inventory reasons
//! distinguish business consumers from remaining wake debt. Use commit_wake for new due sources.
#[path = "support/ratchet.rs"]
mod ratchet;
#[path = "support/ratchet_tests.rs"]
mod ratchet_tests;
#[path = "node_boundary/scan.rs"]
#[allow(dead_code)]
mod scan;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const LIST: &str = "tests/due_wake_sites.txt";
const RATIONALE: &[(&str, &str)] = &[
    (
        "steve-hub/src/extension.rs",
        "Plugin event API forwards selected committed business facts; it is not a DueSource wake.",
    ),
    (
        "steve-hub/src/wire/install/domain/contacts.rs",
        "Existing contacts.watch due-loop wake debt; R7f removes this subscriber in its second commit.",
    ),
    (
        "steve-hub/src/wire/install/domain/extensions.rs",
        "Connection lifecycle hooks start/stop reactive adapter instances, not a DueSource wake.",
    ),
    (
        "steve-hub/src/wire/install/domain/knowledge.rs",
        "Memory/Bot changes invoke indexing of the changed business record.",
    ),
    (
        "steve-hub/src/wire/install/domain/mail.rs",
        "A new mailbox business event installs its triage template.",
    ),
    (
        "steve-hub/src/wire/install/domain/plugins.rs",
        "Plugin supervision reacts to connection lifecycle changes, not due-time scheduling.",
    ),
    (
        "steve-hub/src/wire/install/hub/research.rs",
        "Workspace projection follows Project/Knowledge business changes for research roots.",
    ),
    (
        "steve-hub/src/wire/install/hub/runtime.rs",
        "Workspace projection follows Project/Knowledge business changes for execution roots.",
    ),
    (
        "steve-hub/src/wire/install/hub/yolo.rs",
        "Decision and Yolo business events drive the reactive approval engine; ticks are separate.",
    ),
];

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

/// Call sites `.tap(...)` or `.tap::<...>(...)`, including whitespace, comments and aliases.
fn tap_sites(code: &str) -> Vec<usize> {
    let bytes = code.as_bytes();
    let mut sites = Vec::new();
    for (dot, _) in code.match_indices('.') {
        let mut at = skip_space(bytes, dot + 1);
        if code[at..].starts_with("r#") {
            at += 2;
        }
        let start = at;
        while bytes
            .get(at)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            at += 1;
        }
        if &code[start..at] != "tap" {
            continue;
        }
        at = skip_space(bytes, at);
        if bytes.get(at) == Some(&b'(') {
            sites.push(dot);
            continue;
        }
        if bytes.get(at) != Some(&b':') {
            continue;
        }
        at = skip_space(bytes, at + 1);
        if bytes.get(at) != Some(&b':') {
            continue;
        }
        at = skip_space(bytes, at + 1);
        if bytes.get(at) == Some(&b'<') {
            sites.push(dot);
        }
    }
    sites
}

fn found() -> ratchet::Counts {
    let mut crates: Vec<_> = std::fs::read_dir(crates_dir())
        .unwrap()
        .map(Result::unwrap)
        .filter(|e| e.path().join("Cargo.toml").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    crates.sort();
    let mut counts = BTreeMap::new();
    for name in crates {
        for source in scan::crate_sources(&crates_dir(), &name) {
            let count = tap_sites(&source.code).len();
            if count > 0 {
                counts.insert(vec![source.rel], count);
            }
        }
    }
    counts
}

fn header() -> String {
    let mut header = String::from(
        "# Production DomainEvent tap sites; reviewed inventory, only shrinks.\n# file<TAB>count. BLESS only records removals; a missing inventory requires explicit INIT.\n# Existing debt is not approval to use DomainEvent as a due-loop wake; use Store::commit_wake.\n",
    );
    for (file, reason) in RATIONALE {
        header.push_str(&format!("# {file}: {reason}\n"));
    }
    header
}

#[test]
fn production_domain_subscriptions_only_shrink() {
    let now = found();
    let reasons: BTreeSet<_> = RATIONALE
        .iter()
        .map(|(file, reason)| {
            assert!(!reason.trim().is_empty(), "{file} needs a review reason");
            *file
        })
        .collect();
    assert_eq!(reasons.len(), RATIONALE.len(), "duplicate review reason");
    assert_eq!(
        now.keys().map(|k| k[0].as_str()).collect::<BTreeSet<_>>(),
        reasons,
        "Every production file needs one reason; remove reasons after its last site disappears"
    );
    ratchet::check(
        now,
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(LIST),
        1,
        &header(),
        "new DomainEvent subscription: use commit_wake for a due source, or request explicit review",
    );
}

#[test]
fn tap_scanner_covers_spacing_generics_aliases_and_ignores_non_calls() {
    for (source, expected) in [
        ("fn f() { bus.tap(|_| true); }", 1),
        ("fn f() { bus . /* note */ tap ( |_| true ); }", 1),
        (
            "fn f() { alias.tap::<DomainEvent>(filter); other.tap :: <Kind>(f); }",
            2,
        ),
        ("fn f() { b.r#tap(f); }", 1),
        ("fn f() { b.tapper(f); b.tap; }", 0),
        ("// bus.tap(f)\nfn f() { let s = \"bus.tap(f)\"; }", 0),
        ("#[cfg(test)] mod tests { fn f() { bus.tap(f); } }", 0),
        (
            "#[cfg(feature = \"testkit\")] mod tests { fn f() { bus.tap(f); } }",
            0,
        ),
    ] {
        let code = scan::production(source).unwrap();
        assert_eq!(tap_sites(&code).len(), expected, "{source}");
    }
}
