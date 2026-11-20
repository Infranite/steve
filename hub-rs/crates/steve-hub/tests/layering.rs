//! The crate graph has two inventories: reviewed target edges and debt edges with a removal owner.
//! Only debt ratchets; target edits must be explicit in review. Both use the shared count comparison.
//! STEVE_GUARD_BLESS=1 rewrites debt after removals and refuses growth.

#[path = "support/ratchet.rs"]
mod ratchet;

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

const LIST: &str = "tests/layering.txt";

fn found() -> BTreeSet<String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
        .expect("cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).expect("metadata JSON");
    let packages = meta["packages"].as_array().expect("packages");
    let members: BTreeSet<&str> = packages.iter().filter_map(|p| p["name"].as_str()).collect();
    let mut lines = BTreeSet::new();
    for p in packages {
        let name = p["name"].as_str().expect("package name");
        for d in p["dependencies"].as_array().into_iter().flatten() {
            let dep = d["name"].as_str().expect("dependency name");
            if dep == "rusqlite" {
                lines.insert(format!("rusqlite {name}"));
            }
            let production = d["kind"].is_null() || d["kind"] == "build";
            if production && members.contains(dep) {
                lines.insert(format!("edge {name} {dep}"));
            }
        }
    }
    lines
}

const DEBT: &str = "tests/layering_debt.txt";
const DEBT_HEADER: &str = "# Edges to remove; only this segment ratchets.\n# removal wave<TAB>responsible group<TAB>edge-or-rusqlite key<TAB>count\n# c1 awaits context ownership at wave-4 C1; wave is the edge removal deadline.\n# g0/g2/g4 rows have active owners; g2 owns the legacy storage facade retirement.\n# STEVE_GUARD_BLESS=1 rewrites after removals; new or growing edges always fail.\n";

fn partition(
    now: BTreeSet<String>,
    target: &ratchet::Counts,
    debt: &ratchet::Counts,
) -> (ratchet::Counts, ratchet::Counts) {
    let mut owners = std::collections::BTreeMap::new();
    for (key, count) in debt {
        assert!(
            *count == 1 && key[0].starts_with('w') && (key[1] == "c1" || key[1].starts_with('g')),
            "invalid debt: {key:?}"
        );
        assert!(
            !target.contains_key(&vec![key[2].clone()]),
            "edge in both segments: {key:?}"
        );
        assert!(
            owners.insert(key[2].clone(), key.clone()).is_none(),
            "duplicate debt edge: {key:?}"
        );
    }
    assert!(
        target.values().all(|n| *n == 1),
        "target edges must have count 1"
    );
    let mut target_now = ratchet::Counts::new();
    let mut debt_now = ratchet::Counts::new();
    for edge in now {
        let key = vec![edge.clone()];
        if target.contains_key(&key) {
            target_now.insert(key, 1);
        } else {
            debt_now.insert(
                owners
                    .get(&edge)
                    .cloned()
                    .unwrap_or_else(|| vec!["unassigned".into(), "unassigned".into(), edge]),
                1,
            );
        }
    }
    (target_now, debt_now)
}

fn debt_counts(debt: &ratchet::Counts) -> (usize, usize) {
    let pending = debt.keys().filter(|key| key[1] == "c1").count();
    (debt.len() - pending, pending)
}

#[test]
fn the_crate_graph_only_loses_edges() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let target = ratchet::read(&root.join(LIST), 1);
    let debt = ratchet::read(&root.join(DEBT), 3);
    let (target_now, debt_now) = partition(found(), &target, &debt);
    let (assigned, pending) = debt_counts(&debt_now);
    eprintln!(
        "layering: target {} edges; debt {} (assigned {assigned}, C1 awaiting {pending})",
        target_now.len(),
        debt_now.len(),
    );
    let problems = ratchet::differences(&target_now, &target, "review the target architecture");
    assert!(
        problems.is_empty(),
        "target architecture changed; edit layering.txt explicitly:\n{}",
        problems.join("\n")
    );
    ratchet::check(
        debt_now,
        &root.join(DEBT),
        3,
        DEBT_HEADER,
        "depend on a lower layer contract; SQL belongs to steve-store-sqlite",
    );
}

#[test]
fn target_and_debt_are_disjoint_and_only_debt_can_be_blessed_down() {
    let target = ratchet::Counts::from([(vec!["edge app contract".into()], 1)]);
    let debt_key = vec!["w4".into(), "g0".into(), "edge app legacy".into()];
    let debt = ratchet::Counts::from([(debt_key.clone(), 1)]);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("debt.txt");
    ratchet::check_at(debt.clone(), &file, 3, "", "fix", ratchet::Mode::Init);
    let (actual_target, shrunk_debt) =
        partition(BTreeSet::from(["edge app contract".into()]), &target, &debt);
    assert_eq!(actual_target, target);
    assert!(shrunk_debt.is_empty());
    ratchet::check_at(shrunk_debt, &file, 3, "", "fix", ratchet::Mode::Bless);
    assert!(ratchet::read(&file, 3).is_empty());
    let (gone_target, new_debt) =
        partition(BTreeSet::from(["edge app new".into()]), &target, &debt);
    assert!(!ratchet::differences(&gone_target, &target, "review target").is_empty());
    assert!(!ratchet::growth(&new_debt, &debt).is_empty());
    assert!(
        std::panic::catch_unwind(|| partition(
            BTreeSet::new(),
            &target,
            &ratchet::Counts::from([(
                vec!["w4".into(), "g0".into(), "edge app contract".into()],
                1
            )])
        ))
        .is_err()
    );
}

#[test]
fn c1_debt_is_not_reported_as_an_assigned_group() {
    let target = ratchet::Counts::new();
    let debt = ratchet::Counts::from([
        (vec!["w3".into(), "g4".into(), "edge app node".into()], 1),
        (vec!["w4".into(), "c1".into(), "edge app legacy".into()], 1),
    ]);
    let (_, actual) = partition(
        BTreeSet::from(["edge app node".into(), "edge app legacy".into()]),
        &target,
        &debt,
    );
    assert_eq!(actual, debt);
    assert_eq!(debt_counts(&actual), (1, 1));
}
