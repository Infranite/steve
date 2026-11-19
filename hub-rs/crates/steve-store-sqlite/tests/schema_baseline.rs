mod common;

use common::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn check_file(path: &str, expected: &str) {
    let path = root().join(path);
    if std::env::var("STEVE_BASELINE_BLESS").as_deref() == Ok("1") {
        std::fs::write(&path, expected).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        expected,
        "{} differs; regenerate with STEVE_BASELINE_BLESS=1",
        path.display()
    );
}

#[test]
fn generated_baselines_and_inventories_are_current() {
    let c = legacy();
    let map = domains();
    let objects = objects(&c);
    for o in &objects {
        assert!(map.contains_key(&o.table), "missing domain: {}", o.table);
        assert!(
            ["table", "index", "trigger"].contains(&o.kind.as_str()),
            "unhandled object: {o:?}"
        );
    }
    let all_debts: Vec<_> = ["stream", "comms"]
        .into_iter()
        .flat_map(|cut| debts(&c, &map, cut))
        .collect();
    for domain in DOMAINS {
        let mut sql = String::from(
            "-- Generated from the frozen legacy opening flow; STEVE_BASELINE_BLESS=1 regenerates.\n",
        );
        for o in objects.iter().filter(|o| map[&o.table] == domain) {
            for d in all_debts.iter().filter(|d| d.object == o.name) {
                sql.push_str(&format!("-- debt:{}\n", d.id));
            }
            sql.push_str(&o.sql);
            sql.push_str(";\n\n");
        }
        sql.push_str(&seeds(&c, &map, domain));
        check_file(&format!("{domain}/0001_baseline.sql"), &sql);
        check_file(
            &format!("{domain}/LOCK"),
            &format!("0001_baseline.sql {}\n", digest(sql.as_bytes())),
        );
    }
    let contract = std::fs::read(root().join("contract/0001.sql")).unwrap();
    check_file(
        "contract/LOCK",
        &format!("0001.sql {}\n", digest(&contract)),
    );
    for (cut, write_cap) in [("stream", 39), ("comms", 151)] {
        let path = format!("debt_{cut}.txt");
        let old = std::fs::read_to_string(root().join(&path)).unwrap();
        let writes: BTreeSet<_> = old.lines().filter(|s| s.starts_with("write\t")).collect();
        assert!(writes.len() <= write_cap, "{cut} write debt increased");
        assert_eq!(
            writes.len(),
            old.lines().filter(|s| s.starts_with("write\t")).count(),
            "duplicate write debt"
        );
        for line in &writes {
            let fields: Vec<_> = line.split('\t').collect();
            assert_eq!(fields.len(), 3);
            assert!(fields[1].starts_with(&format!("{cut}/write/")) && !fields[2].is_empty());
        }
        let mut expected = debts(&c, &map, cut)
            .iter()
            .map(Debt::line)
            .collect::<String>();
        for write in writes {
            expected.push_str(write);
            expected.push('\n');
        }
        check_file(&path, &expected);
    }
    let inventory = std::fs::read_to_string(root().join("triggers.tsv")).unwrap();
    let replacements: BTreeMap<_, _> = inventory
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let f: Vec<_> = l.split('\t').collect();
            assert_eq!(f.len(), 4);
            (f[0], f)
        })
        .collect();
    let triggers: BTreeMap<_, _> = objects
        .iter()
        .filter(|o| o.kind == "trigger")
        .map(|o| (o.name.as_str(), o))
        .collect();
    assert!(
        triggers.len() <= replacements.len() && replacements.len() <= 42,
        "trigger count increased"
    );
    let mut expected = String::from("# name\ttable\tdomain\treplacement_operation\n");
    for (name, o) in triggers {
        let f = &replacements[name];
        assert!(!f[3].is_empty(), "missing replacement operation for {name}");
        expected.push_str(&format!(
            "{name}\t{}\t{}\t{}\n",
            o.table, map[&o.table], f[3]
        ));
    }
    check_file("triggers.tsv", &expected);
}

#[test]
fn debt_detection_preserves_compound_keys_and_trigger_references() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE parent(a TEXT,b TEXT,PRIMARY KEY(a,b));
        CREATE TABLE child(a TEXT,b TEXT,FOREIGN KEY(a,b) REFERENCES parent(a,b));
        CREATE TABLE inbox(a TEXT REFERENCES parent(a));
        CREATE TRIGGER forward AFTER INSERT ON inbox BEGIN
            UPDATE OR IGNORE parent SET b=b;
            SELECT a FROM main.\"parent\" WHERE b='child'; -- child is a literal, not a reference
        END;
        CREATE TRIGGER backward AFTER INSERT ON parent BEGIN
            INSERT INTO inbox VALUES(NEW.a);
        END;",
    )
    .unwrap();
    let map = [("parent", "core"), ("child", "stream"), ("inbox", "comms")]
        .into_iter()
        .map(|(t, d)| (t.into(), d.into()))
        .collect();
    let stream = debts(&c, &map, "stream");
    assert_eq!(stream.len(), 1);
    assert_eq!(stream[0].id, "stream/fk/child.(a,b) -> parent.(a,b)");
    let comms = debts(&c, &map, "comms");
    assert_eq!(comms.iter().filter(|d| d.kind == "fk").count(), 1);
    assert_eq!(comms.iter().filter(|d| d.kind == "trigger").count(), 2);
}
