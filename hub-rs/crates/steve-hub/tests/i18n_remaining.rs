//! The Chinese string literals still in the code behind steve-hub's API responses, listed in
//! `tests/i18n_remaining.txt`.
//!
//! A heuristic, not a proof: it reads the source files whose text reaches API responses (`src/api/`, `error.rs`,
//! the update status, the model catalog and price sync, the channel wire), drops comments, test modules, `t!()`
//! calls and log lines, and lists every remaining literal with a Han character. Many of them stay Chinese on
//! purpose (stored values such as priority or status, timeline entries, text for a model; see steve-i18n's doc
//! comment), so the list is a ratchet: the test fails on a literal that isn't listed (move it to `t!()`) and on a listed one that is gone (drop it from the list).
//!
//! `STEVE_GUARD_BLESS=1 cargo test -p steve-hub --test i18n_remaining` rewrites the list from the code.

#[path = "support/ratchet.rs"]
mod ratchet;

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

#[test]
fn chinese_literals_in_api_display_paths_are_listed() {
    let now = remaining()
        .into_iter()
        .map(|line| (line.split('\t').map(str::to_owned).collect(), 1))
        .collect();
    ratchet::check(
        now,
        &crate_dir().join(LIST),
        2,
        "# Chinese literals still in steve-hub API display paths; only shrinks.\n# path<TAB>literal<TAB>count (distinct literal per file). STEVE_GUARD_BLESS=1 rewrites after removals.\n",
        "move API display text to t!()",
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
