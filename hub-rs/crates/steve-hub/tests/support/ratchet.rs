//! Count inventories shared by source guards. Changes require an explicit shrink of the baseline.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
pub type Counts = BTreeMap<Vec<String>, usize>;

pub fn differences(now: &Counts, listed: &Counts, help: &str) -> Vec<String> {
    let keys: BTreeSet<_> = now.keys().chain(listed.keys()).collect();
    keys.into_iter()
        .filter_map(|key| {
            let actual = now.get(key).copied().unwrap_or(0);
            let old = listed.get(key).copied().unwrap_or(0);
            (actual != old).then(|| {
                format!(
                    "{}: found {actual}, listed {old}; {}",
                    key.join(" "),
                    if actual < old {
                        "shorten the list"
                    } else {
                        help
                    }
                )
            })
        })
        .collect()
}

pub fn read(file: &Path, columns: usize) -> Counts {
    let mut out = Counts::new();
    for line in std::fs::read_to_string(file)
        .unwrap_or_else(|e| panic!("{}: {e}", file.display()))
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), columns + 1, "bad count row: {line}");
        let count: usize = fields[columns].parse().expect("count");
        let key = fields[..columns].iter().map(|s| (*s).to_owned()).collect();
        assert!(
            count > 0 && out.insert(key, count).is_none(),
            "duplicate or zero count row: {line}"
        );
    }
    out
}

pub fn check(now: Counts, file: &Path, columns: usize, header: &str, help: &str) {
    let bless = std::env::var("STEVE_RUNTIME_BLESS").as_deref() == Ok("1");
    let listed = if file.exists() {
        read(file, columns)
    } else {
        assert!(bless, "missing inventory {}", file.display());
        Counts::new()
    };
    if bless {
        if file.exists() {
            let additions: Vec<_> = now
                .iter()
                .filter(|(k, n)| **n > listed.get(*k).copied().unwrap_or(0))
                .map(|(k, n)| format!("{}: {n}", k.join(" ")))
                .collect();
            assert!(
                additions.is_empty(),
                "inventory can only shrink, even with BLESS:\n{}",
                additions.join("\n")
            );
        }
        let mut text = header.to_owned();
        for (key, count) in now {
            text.push_str(&format!("{}\t{count}\n", key.join("\t")));
        }
        std::fs::write(file, text).unwrap();
    } else {
        let problems = differences(&now, &listed, help);
        assert!(
            problems.is_empty(),
            "source guard {}:\n{}",
            file.display(),
            problems.join("\n")
        );
    }
}
