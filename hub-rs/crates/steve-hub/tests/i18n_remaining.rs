//! The Chinese string literals still in the code behind steve-hub's API responses, listed in
//! `tests/i18n_remaining.txt`.
//!
//! A heuristic, not a proof: it reads the source files whose text reaches API responses (`src/api/`, `error.rs`,
//! the update status, the model catalog and price sync, the channel wire), drops comments, test modules, `t!()`
//! calls and log lines, and lists every remaining literal with a Han character. Many of them stay Chinese on
//! purpose (stored values such as priority or status, timeline entries, text for a model; see steve-i18n's doc
//! comment), so the list is a ratchet: the test fails on a literal that isn't listed (move it to `t!()`, or add
//! it to the list if it must stay Chinese) and on a listed one that is gone (drop it from the list).
//!
//! `STEVE_I18N_BLESS=1 cargo test -p steve-hub --test i18n_remaining` rewrites the list from the code.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Sources whose text can reach an API response, relative to the crate.
const SCOPE: &[&str] = &[
    "src/api",
    "src/error.rs",
    "src/upgrades",
    "src/catalog.rs",
    "src/price_sync.rs",
    "src/wire/chan.rs",
];

const LIST: &str = "tests/i18n_remaining.txt";

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_file() {
        if p.extension().is_some_and(|x| x == "rs") {
            out.push(p.to_path_buf());
        }
        return;
    }
    let Ok(rd) = std::fs::read_dir(p) else {
        return;
    };
    for e in rd.flatten() {
        rust_files(&e.path(), out);
    }
}

/// The code of a file without comments, log lines, `t!()` lines and its test module.
fn display_code(text: &str) -> String {
    let t_call = regex::Regex::new(r"(^|[^\w])t!\(").expect("regex");
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let line = *line;
        let l = line.trim_start();
        // an inline test module sits at the end of a file (`mod tests;` is only a declaration)
        if line.starts_with("#[cfg(test)]")
            && lines
                .get(i + 1)
                .is_some_and(|n| n.starts_with("mod ") && n.trim_end().ends_with('{'))
        {
            break;
        }
        if l.starts_with("//")
            || l.contains("tracing::")
            || t_call.is_match(l)
            || ["warn!(", "info!(", "debug!(", "error!(", "trace!("]
                .iter()
                .any(|m| l.starts_with(m))
        {
            out.push("");
            continue;
        }
        out.push(line);
    }
    // a quote as a char literal would pair with the wrong string
    out.join("\n").replace("'\"'", "''").replace("'\\\"'", "''")
}

fn has_han(s: &str) -> bool {
    s.chars()
        .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3400}'..='\u{4dbf}').contains(&c))
}

/// `path<TAB>literal` for every Chinese literal in scope, the literal escaped onto one line.
fn remaining() -> BTreeSet<String> {
    let root = crate_dir();
    let lit = regex::Regex::new(r#""(?:[^"\\]|\\.)*""#).expect("regex");
    let mut files = Vec::new();
    for s in SCOPE {
        rust_files(&root.join(s), &mut files);
    }
    let mut out = BTreeSet::new();
    for f in files {
        if f.file_name()
            .is_some_and(|n| n == "tests.rs" || n.to_string_lossy().ends_with("_tests.rs"))
        {
            continue;
        }
        let text = std::fs::read_to_string(&f).expect("read source");
        let rel = f
            .strip_prefix(&root)
            .expect("in crate")
            .to_string_lossy()
            .replace('\\', "/");
        for m in lit.find_iter(&display_code(&text)) {
            if has_han(m.as_str()) {
                out.insert(format!("{rel}\t{}", m.as_str().replace('\n', "\\n")));
            }
        }
    }
    out
}

fn listed(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

#[test]
fn chinese_literals_in_api_display_paths_are_listed() {
    let now = remaining();
    let path = crate_dir().join(LIST);
    if std::env::var_os("STEVE_I18N_BLESS").is_some() {
        let mut body = String::from(
            "# Chinese literals still in steve-hub's API display paths (see tests/i18n_remaining.rs).\n\
             # One `path<TAB>literal` per line. Rewrite with STEVE_I18N_BLESS=1.\n",
        );
        for l in &now {
            body.push_str(l);
            body.push('\n');
        }
        std::fs::write(&path, body).expect("write list");
        return;
    }
    let was = listed(&std::fs::read_to_string(&path).unwrap_or_default());
    let new: Vec<_> = now.difference(&was).cloned().collect();
    let gone: Vec<_> = was.difference(&now).cloned().collect();
    eprintln!(
        "{} Chinese literals remain in API display paths:",
        now.len()
    );
    for l in &now {
        eprintln!("  {l}");
    }
    assert!(
        new.is_empty() && gone.is_empty(),
        "tests/i18n_remaining.txt is out of date (STEVE_I18N_BLESS=1 rewrites it).\n\
         Not listed (use t!() or list it if it must stay Chinese):\n  {}\n\
         Listed but gone:\n  {}",
        new.join("\n  "),
        gone.join("\n  ")
    );
}

#[test]
fn the_scan_skips_comments_logs_t_calls_and_tests() {
    let code = display_code(
        "fn a() { x(\"留下\"); }\n// \"注释\"\ntracing::warn!(\"日志\");\nlet s = t!(\"hub.a\", n = \"甲\");\n\
         let f = format!(\"格式\");\n\
         let q = '\"'; y(\"还在\");\n#[cfg(test)]\nmod decl;\nw(\"之后\");\n#[cfg(test)]\nmod tests {\n    z(\"测试\");\n}\n",
    );
    for gone in ["注释", "日志", "甲", "测试"] {
        assert!(!code.contains(gone), "{gone} in {code}");
    }
    assert!(
        code.contains("\"留下\"")
            && code.contains("\"还在\"")
            && code.contains("\"格式\"")
            && code.contains("\"之后\""),
        "{code}"
    );
}
