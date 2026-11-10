//! Every crate's `locales/zh-CN.json` and `locales/en.json` agree: the same keys, the same `%{name}`
//! placeholders, the crate's key prefix, and every `t!("…")` in its `src/` has a zh-CN entry.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use steve_i18n::LOCALES;

/// The crates whose locale files must exist (other crates get them when they first need one).
const EXPECTED: &[&str] = &[
    "steve-hub",
    "steve-core",
    "steve-channels",
    "steve-flow",
    "steve-modules",
    "steve-vault",
    "steve-auth",
    "steve-mail",
    "steve-github",
    "steve-knowledge",
    "steve-stats",
    "steve-store",
    "steve-acp",
    "steve-research",
    "steve-node",
    "steve-install",
    "steve-i18n",
];

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .to_path_buf()
}

fn load(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let obj = v
        .as_object()
        .ok_or_else(|| format!("{}: not one flat object", path.display()))?;
    let mut out = BTreeMap::new();
    for (k, v) in obj {
        let s = v
            .as_str()
            .ok_or_else(|| format!("{}: `{k}` is not a string (keys are flat)", path.display()))?;
        out.insert(k.clone(), s.to_string());
    }
    Ok(out)
}

fn placeholders(s: &str) -> BTreeSet<String> {
    let re = regex::Regex::new(r"%\{(\w+)\}").expect("regex");
    re.captures_iter(s).map(|c| c[1].to_string()).collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Everything wrong with one crate's catalogs.
fn check_crate(dir: &Path) -> Vec<String> {
    let name = dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let prefix = format!("{}.", name.strip_prefix("steve-").unwrap_or(&name));
    let mut errs = Vec::new();
    let mut cats = BTreeMap::new();
    for l in LOCALES {
        match load(&dir.join("locales").join(format!("{l}.json"))) {
            Ok(m) => {
                cats.insert(l, m);
            }
            Err(e) => errs.push(e),
        }
    }
    if cats.len() != LOCALES.len() {
        return errs;
    }
    let zh = &cats[steve_i18n::ZH_CN];
    for (l, m) in &cats {
        for k in m.keys() {
            if !k.starts_with(&prefix) {
                errs.push(format!("{name} {l}: `{k}` must start with `{prefix}`"));
            }
        }
        if *l == steve_i18n::ZH_CN {
            continue;
        }
        for k in zh.keys().filter(|k| !m.contains_key(*k)) {
            errs.push(format!("{name} {l}: missing `{k}`"));
        }
        for k in m.keys().filter(|k| !zh.contains_key(*k)) {
            errs.push(format!("{name} {l}: `{k}` is not in zh-CN"));
        }
        for (k, v) in m {
            if let Some(src) = zh.get(k)
                && placeholders(src) != placeholders(v)
            {
                errs.push(format!(
                    "{name} {l}: `{k}` has placeholders {:?}, zh-CN has {:?}",
                    placeholders(v),
                    placeholders(src)
                ));
            }
        }
    }
    let used = regex::Regex::new(r#"\bt!\(\s*"([^"]+)""#).expect("regex");
    let mut files = Vec::new();
    rust_files(&dir.join("src"), &mut files);
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        // comments (the examples in doc comments) don't count
        let code: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for c in used.captures_iter(&code) {
            if !zh.contains_key(&c[1]) {
                errs.push(format!(
                    "{}: `{}` is not in locales/zh-CN.json",
                    f.display(),
                    &c[1]
                ));
            }
        }
    }
    errs
}

#[test]
fn every_crate_has_matching_catalogs() {
    let root = crates_dir();
    let mut errs = Vec::new();
    for c in EXPECTED {
        if !root.join(c).join("locales").is_dir() {
            errs.push(format!("{c}: no locales/ directory"));
        }
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("crates/")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("locales").is_dir())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty());
    for d in dirs {
        errs.extend(check_crate(&d));
    }
    assert!(errs.is_empty(), "locale catalogs:\n{}", errs.join("\n"));
}

#[test]
fn the_check_catches_drift() {
    let tmp = std::env::temp_dir().join(format!("steve-i18n-drift-{}", std::process::id()));
    let dir = tmp.join("steve-demo");
    std::fs::create_dir_all(dir.join("locales")).expect("dir");
    std::fs::create_dir_all(dir.join("src")).expect("dir");
    std::fs::write(
        dir.join("locales/zh-CN.json"),
        r#"{"demo.a.b": "甲 %{x}", "demo.a.only_zh": "乙", "other.c": "丙"}"#,
    )
    .expect("write");
    std::fs::write(
        dir.join("locales/en.json"),
        r#"{"demo.a.b": "A %{y}", "demo.a.only_en": "B", "other.c": "C"}"#,
    )
    .expect("write");
    std::fs::write(
        dir.join("src/lib.rs"),
        r#"fn f() { t!("demo.a.unknown"); t!("demo.a.b", x = 1); }"#,
    )
    .expect("write");
    let errs = check_crate(&dir).join("\n");
    let _ = std::fs::remove_dir_all(&tmp);
    for want in [
        "missing `demo.a.only_zh`",
        "`demo.a.only_en` is not in zh-CN",
        "`demo.a.b` has placeholders",
        "`other.c` must start with `demo.`",
        "`demo.a.unknown` is not in locales/zh-CN.json",
    ] {
        assert!(errs.contains(want), "{want} not reported in:\n{errs}");
    }
    assert!(!errs.contains("`demo.a.b` is not in locales"), "{errs}");
}
