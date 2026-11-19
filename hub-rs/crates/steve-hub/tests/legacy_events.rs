//! Temporary event migration ratchet. Production constructors and raw publishers may only shrink.
//! STEVE_LEGACY_BLESS=1 cargo test -p steve-hub --test legacy_events rewrites the sorted baseline.
//! W1.3b must shrink it per crate, then remove this guard together with the last legacy path.
#[path = "node_boundary/scan.rs"]
#[allow(dead_code)]
mod scan;

use std::collections::BTreeMap;
use std::path::Path;

type Counts = BTreeMap<(String, String), usize>;
const LIST: &str = "tests/legacy_events.txt";

/// Returns whether this occurrence lies in the pattern argument of matches! (including qualified
/// std::matches!). Its subject is still an expression, so constructors in that argument count.
fn matches_pattern(code: &str, at: usize) -> bool {
    for open in scan::find_in(&code[..at], "matches!") {
        let mut depth = 0;
        let mut pattern = false;
        let mut opened = false;
        for byte in code.as_bytes()[open + "matches!".len()..at].iter().copied() {
            match byte {
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    opened = true;
                }
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if opened && depth == 0 {
                        break;
                    }
                }
                b',' if depth == 1 => pattern = true,
                _ => {}
            }
        }
        if depth > 0 && pattern {
            return true;
        }
    }
    false
}

fn occurrences(raw: &str, code: &str) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    let constructors =
        regex::Regex::new(r"\b(ChangeEvent|HubEvent)\s*::\s*[A-Za-z_]\w*\s*\{").unwrap();
    for hit in constructors.captures_iter(code) {
        let m = hit.get(0).unwrap();
        let open = m.end() - 1;
        let mut depth = 0;
        let mut end = None;
        for (offset, byte) in code.as_bytes()[open..].iter().copied().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                end = Some(open + offset + 1);
                break;
            }
        }
        let Some(end) = end else { continue };
        let tail = code[end..].trim_start();
        // A match/if-let pattern is not a publication. Syn also rejects pattern-only fields
        // (`ref id`, `..` with no update expression), while macro token trees remain covered.
        if tail.starts_with("=>")
            || tail.starts_with("if ")
            || tail.starts_with('|')
            || tail
                .strip_prefix('=')
                .is_some_and(|rest| !rest.starts_with('='))
            || matches_pattern(code, m.start())
        {
            continue;
        }
        if syn::parse_str::<syn::ExprStruct>(&raw[m.start()..end]).is_ok() {
            *counts.entry(format!("{}::", &hit[1])).or_default() += 1;
        }
    }
    let calls = regex::Regex::new(r"\bpublish_value\s*\(").unwrap();
    for hit in calls.find_iter(code) {
        let previous = code[..hit.start()].trim_end();
        if previous.strip_suffix("fn").is_some_and(|prefix| {
            prefix
                .as_bytes()
                .last()
                .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_')
        }) {
            continue;
        }
        *counts.entry("publish_value(".into()).or_default() += 1;
    }
    counts
}

fn found() -> Counts {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut out = BTreeMap::new();
    let mut names: Vec<_> = std::fs::read_dir(&crates)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.path().join("Cargo.toml").exists())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        for source in scan::crate_sources(&crates, &name) {
            let raw = std::fs::read_to_string(crates.join(&source.rel)).unwrap();
            for (pattern, count) in occurrences(&raw, &source.code) {
                out.insert((source.rel.clone(), pattern), count);
            }
        }
    }
    out
}

fn differences(now: &Counts, listed: &Counts) -> Vec<String> {
    let mut errors = Vec::new();
    for (key, n) in now {
        let old = listed.get(key).copied().unwrap_or(0);
        if *n > old {
            errors.push(format!(
                "{} {}: {n}, listed {old}: new legacy publication",
                key.0, key.1
            ));
        }
        if *n < old {
            errors.push(format!(
                "{} {}: {n}, listed {old}: lower the list",
                key.0, key.1
            ));
        }
    }
    for (key, old) in listed {
        if !now.contains_key(key) {
            errors.push(format!(
                "{} {}: gone, listed {old}: drop the entry",
                key.0, key.1
            ));
        }
    }
    errors
}

#[test]
fn legacy_publications_only_shrink() {
    let now = found();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(LIST);
    if std::env::var("STEVE_LEGACY_BLESS").as_deref() == Ok("1") {
        let rows: String = now
            .iter()
            .map(|((file, pattern), count)| format!("{file}\t{pattern}\t{count}\n"))
            .collect();
        std::fs::write(path, format!("# Remaining production legacy publications. Shrink per crate; remove with the last legacy path.\n{rows}")).unwrap();
        return;
    }
    let text = std::fs::read_to_string(path)
        .expect("legacy baseline; initialize with STEVE_LEGACY_BLESS=1");
    let mut listed = BTreeMap::new();
    for line in text
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
    {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 3, "invalid legacy baseline row: {line}");
        let count: usize = fields[2].parse().unwrap();
        assert!(count > 0, "zero rows must be removed: {line}");
        assert!(
            listed
                .insert((fields[0].into(), fields[1].into()), count)
                .is_none(),
            "duplicate baseline row: {line}"
        );
    }
    let errors = differences(&now, &listed);
    assert!(
        errors.is_empty(),
        "legacy event ratchet:\n{}",
        errors.join("\n")
    );
}

#[test]
fn scans_qualified_and_macro_constructors_and_calls_but_not_tests_comments_or_patterns() {
    let definition = "fn publish_value(value: Value) {}";
    assert!(occurrences(definition, &scan::production(definition).unwrap()).is_empty());
    let raw = r##"
        fn publish_value(&self, value: Value) {}
        fn run() {
            send(steve_store::ChangeEvent :: Item { id: "x".into() });
            install! { start(reg) { send(crate::bus::HubEvent::Item { id }); } }
            bus . publish_value (payload);
            Bus::publish_value(&bus, payload);
            let literal = "ChangeEvent::Item { id }; publish_value(payload)";
            // HubEvent::Item { id }; publish_value(payload)
            match event { ChangeEvent::Item { id } => (), HubEvent::Item { .. } => (), _ => () }
            let _ = matches!(event, ChangeEvent::Item { id });
            let _ = matches!(HubEvent::Item { id }, HubEvent::Item { id });
        }
        #[cfg(test)] mod tests { fn run() { send(ChangeEvent::Item { id }); bus.publish_value(v); } }
    "##;
    let code = scan::production(raw).unwrap();
    assert_eq!(
        occurrences(raw, &code),
        BTreeMap::from([
            ("ChangeEvent::".into(), 1),
            ("HubEvent::".into(), 2),
            ("publish_value(".into(), 2),
        ])
    );
}

#[test]
fn rejects_additions_and_requires_baseline_shrinkage() {
    let key = ("crate/src/lib.rs".into(), "ChangeEvent::".into());
    let listed = Counts::from([(key.clone(), 2)]);
    assert!(differences(&listed, &listed).is_empty());
    for now in [
        Counts::from([(key.clone(), 3)]),
        Counts::from([(key.clone(), 1)]),
        Counts::new(),
    ] {
        assert!(!differences(&now, &listed).is_empty());
    }
    assert!(!differences(&listed, &Counts::new()).is_empty());
}
