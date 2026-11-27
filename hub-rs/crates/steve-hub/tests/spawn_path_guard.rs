//! macOS `posix_spawn` safety guard (#621): production code that modifies `PATH` or calls
//! `env_clear()` on a `std::process::Command` / `tokio::process::Command` must pass an executable
//! resolved by `steve_runtime::process::{resolve_program, require_absolute_program}` or an
//! absolute path literal/constant.
//!
//! Why: on macOS, Rust's `std::process::Command` uses `posix_spawn` only when there are no
//! `pre_exec`/credential hooks AND — whenever `PATH` was changed (including via `env_clear()`,
//! `.env("PATH", ...)`, `.env_remove("PATH")`, or `.envs(...)`) — the program is already a path
//! (absolute to also avoid `Relative + current_dir` `fork()` fallback). Otherwise `std` silently
//! falls back to `fork()` + `exec()`. In multithreaded Hub and Node processes, `fork()` can
//! deadlock on CoreFoundation / Security / libdispatch / allocator locks (#600, #621).
//!
//! The scanner walks every workspace crate's production module tree (`src/lib.rs` and `src/main.rs`,
//! excluding comments, string contents, and test/fake/testkit items via `node_boundary/scan.rs`).
//! Existing reviewed call sites whose program path is established outside the local function (or
//! compiled only on non-macOS) are listed in `tests/spawn_path_guard.txt`; any new unproved site
//! fails CI, and removed sites must be shortened with:
//! `STEVE_GUARD_BLESS=1 cargo test -p steve-hub --test spawn_path_guard -- spawn_commands_with_modified_path_resolve_program`

#[path = "support/ratchet.rs"]
#[allow(dead_code)]
mod ratchet;

#[path = "node_boundary/scan.rs"]
#[allow(dead_code)]
mod scan;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const LIST: &str = "tests/spawn_path_guard.txt";
const HEADER: &str = "# Production Command::new sites that clear env or may modify PATH without a local\n# absolute literal or resolve_program/require_absolute_program call; only shrinks.\n# file under hub-rs/crates<TAB>count. Every entry needs a matching RATIONALE in spawn_path_guard.rs.\n# STEVE_GUARD_BLESS=1 rewrites after removals and refuses growth.\n";

/// Reviewed reasons for every existing exception in `tests/spawn_path_guard.txt`.
#[rustfmt::skip]
const RATIONALE: &[(&str, &str)] = &[
    ("steve-hub/src/backup/engine.rs", "non-macOS branch only (#[cfg(not(target_os = \"macos\"))]); macOS delegates to steve_runtime::process::macos_spawn::spawn which calls resolve_program"),
    ("steve-hub/src/check.rs", "bin comes from sibling_steve_node() -> std::env::current_exe()?.with_file_name(\"steve-node\"), always absolute"),
    ("steve-hub/src/upgrades/cli.rs", "private helper called only with absolute system binaries (/usr/bin/plutil, /usr/bin/codesign, /bin/launchctl) or canonicalized owner.binary"),
    ("steve-install/src/setup/reconfigure.rs", "runtime.release is an absolute validated installation directory joined with bin/steve-hub"),
    ("steve-modules/src/vault/keystore/helper.rs", "HelperStore::find enforces bin.is_absolute() and std::fs::canonicalize before constructing HelperStore"),
    ("steve-node/src/exec.rs", "program is always /bin/sh or Confine::wrap_in(\"/bin/sh\", ...) (/usr/bin/sandbox-exec or bwrap), always absolute"),
    ("steve-node-cli/src/upgrade.rs", "bin is <staging_dir>/bin/steve-node or bin/steve inside the verified release staging directory"),
    ("steve-runtime/src/process/bounded.rs", "internal build_resolved_std_command(resolved: &Path, ...) helper invoked exclusively with resolve_program() output"),
];

/// Absolute path constants known to start with `/`.
const ABSOLUTE_PATH_CONSTS: &[&str] = &["SANDBOX_EXEC"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Hit {
    file: String,
    line: usize,
    program: String,
    reason: &'static str,
}

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn matching_delim(b: &[u8], open: usize, l: u8, r: u8) -> usize {
    let mut depth = 0usize;
    for (k, &c) in b.iter().enumerate().skip(open) {
        if c == l {
            depth += 1;
        } else if c == r {
            depth -= 1;
            if depth == 0 {
                return k;
            }
        }
    }
    b.len().saturating_sub(1)
}

/// Find the innermost `fn ... { ... }` block enclosing `at`, or the whole file if none.
fn enclosing_fn_body(code: &str, at: usize) -> (usize, usize) {
    let b = code.as_bytes();
    let mut stack = Vec::new();
    let mut fn_spans = Vec::new();
    for (i, &c) in b.iter().enumerate() {
        if c == b'{' {
            let is_fn_block = is_fn_body_open(code, i);
            stack.push((i, is_fn_block));
        } else if c == b'}'
            && let Some((open, is_fn_block)) = stack.pop()
            && is_fn_block
            && open < at
            && at < i
        {
            fn_spans.push((open, i));
        }
    }
    fn_spans
        .into_iter()
        .min_by_key(|(s, e)| e - s)
        .unwrap_or((0, code.len()))
}

/// Heuristic check whether `{` at `open` opens a `fn` body rather than `if`/`match`/`struct`/closure.
fn is_fn_body_open(code: &str, open: usize) -> bool {
    let before = &code[..open];
    let bytes = before.as_bytes();
    // Walk back over whitespace, where-clauses, return type, and parameter list to find `fn `.
    let mut i = bytes.len();
    let mut paren = 0i32;
    let mut angle = 0i32;
    let mut bracket = 0i32;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' => paren += 1,
            b'(' => {
                if paren > 0 {
                    paren -= 1;
                } else {
                    return false;
                }
            }
            b']' => bracket += 1,
            b'[' => {
                if bracket > 0 {
                    bracket -= 1;
                } else {
                    return false;
                }
            }
            b'>' if i > 0 && !matches!(bytes[i - 1], b'-' | b'=') => angle += 1,
            b'<' if angle > 0 => angle -= 1,
            b';' | b'{' | b'}' if paren == 0 && bracket == 0 => {
                return header_starts_fn(&before[i + 1..]);
            }
            _ => {}
        }
    }
    header_starts_fn(before)
}

fn header_starts_fn(header: &str) -> bool {
    // Strip leading outer attributes `#[...]` if any appear right before `fn`.
    let trimmed = header.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Look for a `fn ` keyword at depth 0 before any `=` or `match`/`if`/`for`/`while`/`loop`.
    let bytes = trimmed.as_bytes();
    let mut depth = 0i32;
    for (i, &c) in bytes.iter().enumerate() {
        match c {
            b'(' | b'[' | b'<' => depth += 1,
            b')' | b']' | b'>' if depth > 0 => depth -= 1,
            _ if depth == 0 && trimmed[i..].starts_with("fn") => {
                let before_ok = i == 0 || !is_ident(bytes[i - 1]);
                let after_ok = bytes
                    .get(i + 2)
                    .is_some_and(|b| b.is_ascii_whitespace() || *b == b'<');
                if before_ok && after_ok {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Extract constructor patterns for `std::process::Command` / `tokio::process::Command`
/// (including renamed imports like `use std::process::Command as StdCommand;`).
fn command_ctor_patterns(code: &str) -> Vec<String> {
    let mut patterns = vec!["Command::new(".to_string()];
    for (_, flat) in scan::uses(code) {
        if let Some((path, alias)) = flat.split_once(" as ")
            && matches!(path, "std::process::Command" | "tokio::process::Command")
            && alias != "_"
            && alias != "Command"
        {
            let pat = format!("{alias}::new(");
            if !patterns.contains(&pat) {
                patterns.push(pat);
            }
        }
    }
    patterns
}

/// Return the end offset of the statement continuing from `from` (stopping at `;` or closing `}`
/// at delimiter depth 0).
fn statement_end(code: &str, from: usize, limit: usize) -> usize {
    let b = code.as_bytes();
    let mut depth = 0i32;
    let mut i = from;
    while i < limit {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' => {
                if depth == 0 {
                    return i;
                }
                depth -= 1;
            }
            b'}' => {
                if depth == 0 {
                    return i;
                }
                depth -= 1;
            }
            b';' if depth == 0 => return i + 1,
            _ => {}
        }
        i += 1;
    }
    limit
}

/// If `Command::new(...)` at `at` initializes a `let [mut] <var> = ...;` binding inside
/// `fn_start..at`, return `<var>`.
fn bound_command_var(code: &str, fn_start: usize, at: usize) -> Option<String> {
    let body_start = if code.as_bytes().get(fn_start) == Some(&b'{') {
        fn_start + 1
    } else {
        fn_start
    };
    let prefix = &code[body_start..at];
    let bytes = prefix.as_bytes();
    // Walk backward across statements/branches until we find `let [mut] <var> ... =` at the same
    // or enclosing expression level (so both `if x { let mut cmd = Command::new(...); }` and
    // `let mut cmd = if cond { Command::new(...) } else { ... };` match).
    let mut depth = 0i32;
    let mut cut = 0usize;
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            b'{' => {
                if depth > 0 {
                    depth -= 1;
                } else {
                    let after_brace = prefix[i + 1..].trim_start();
                    if after_brace
                        .strip_prefix("let")
                        .is_some_and(|r| r.as_bytes().first().is_some_and(u8::is_ascii_whitespace))
                    {
                        cut = i + 1;
                        break;
                    }
                }
            }
            b';' if depth == 0 => {
                cut = i + 1;
                break;
            }
            _ => {}
        }
    }
    let stmt = prefix[cut..].trim_start();
    let rest = stmt.strip_prefix("let")?;
    if !rest.as_bytes().first().is_some_and(u8::is_ascii_whitespace) {
        return None;
    }
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix("mut")
        .filter(|r| r.as_bytes().first().is_some_and(u8::is_ascii_whitespace))
        .map_or(rest, str::trim_start);
    let ident: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() {
        return None;
    }
    let after = &rest[ident.len()..];
    // Must actually assign `=` before reaching `Command::new`.
    after.contains('=').then_some(ident)
}

/// Check whether a method chain slice mutates `PATH` or clears the child environment.
fn chain_modifies_path_or_clears_env(
    code_slice: &str,
    raw_slice: &str,
    fn_mentions_path_literal: bool,
) -> Option<&'static str> {
    if code_slice.contains(".env_clear(") {
        return Some("env_clear()");
    }
    if code_slice.contains(".envs(") {
        return Some("envs(...)");
    }
    for method in [".env(", ".env_remove("] {
        for (off, _) in code_slice.match_indices(method) {
            let open = off + method.len() - 1;
            let close = matching_delim(code_slice.as_bytes(), open, b'(', b')');
            let args_raw = &raw_slice[open + 1..close];
            let first_arg = first_call_arg(args_raw, &code_slice[open + 1..close]);
            if first_arg == "\"PATH\"" {
                return Some(if method == ".env(" {
                    "env(\"PATH\", ...)"
                } else {
                    "env_remove(\"PATH\")"
                });
            }
            // Dynamic key (e.g. `for key in ["HOME", "PATH"] { cmd.env(key, v); }`)
            if fn_mentions_path_literal && !first_arg.starts_with('"') && !first_arg.is_empty() {
                return Some("dynamic env(key, ...) with PATH in function");
            }
        }
    }
    None
}

fn first_call_arg<'a>(args_raw: &'a str, args_code: &str) -> &'a str {
    let b = args_code.as_bytes();
    let mut depth = 0i32;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => return args_raw[..i].trim(),
            _ => {}
        }
    }
    args_raw.trim()
}

/// Whether `fn_body` contains a string literal `"PATH"` in production code.
fn contains_path_literal(raw: &str, code: &str, start: usize, end: usize) -> bool {
    let raw_slice = &raw[start..end];
    let code_bytes = code.as_bytes();
    raw_slice
        .match_indices("\"PATH\"")
        .any(|(off, _)| code_bytes.get(start + off) == Some(&b'"'))
}

/// Determine whether a `Command::new(...)` at `at` (with argument ending at `close_paren`)
/// modifies `PATH` or clears env, either chained directly or via its bound variable `<var>`.
fn command_modifies_path_or_clears_env(
    raw: &str,
    code: &str,
    fn_start: usize,
    fn_end: usize,
    at: usize,
    close_paren: usize,
) -> Option<&'static str> {
    let fn_has_path = contains_path_literal(raw, code, fn_start, fn_end);
    let stmt_end = statement_end(code, close_paren + 1, fn_end);
    if let Some(reason) = chain_modifies_path_or_clears_env(
        &code[close_paren + 1..stmt_end],
        &raw[close_paren + 1..stmt_end],
        fn_has_path,
    ) {
        return Some(reason);
    }
    let var = bound_command_var(code, fn_start, at)?;
    let tail_code = &code[stmt_end..fn_end];
    for rel in scan::find_in(tail_code, &var) {
        let abs = stmt_end + rel;
        let after_ident = abs + var.len();
        if code
            .as_bytes()
            .get(after_ident)
            .copied()
            .is_some_and(is_ident)
        {
            continue;
        }
        // Skip field accesses like `other.cmd.foo` where `var` is preceded by `.`.
        if code[stmt_end..abs].trim_end().ends_with('.') {
            continue;
        }
        let after_ws = code[after_ident..fn_end].trim_start();
        if !after_ws.starts_with('.') {
            continue;
        }
        let dot_pos = fn_end - after_ws.len();
        let call_end = statement_end(code, dot_pos, fn_end);
        if let Some(reason) = chain_modifies_path_or_clears_env(
            &code[dot_pos..call_end],
            &raw[dot_pos..call_end],
            fn_has_path,
        ) {
            return Some(reason);
        }
    }
    None
}

fn strip_ref_and_parens(mut expr: &str) -> &str {
    loop {
        let trimmed = expr.trim();
        if let Some(rest) = trimmed.strip_prefix("&mut")
            && rest
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_whitespace() || *b == b'(')
        {
            expr = rest;
        } else if let Some(rest) = trimmed.strip_prefix('&') {
            expr = rest;
        } else if trimmed.starts_with('(') && trimmed.ends_with(')') {
            let inner = &trimmed[1..trimmed.len() - 1];
            if balanced_parens(inner) {
                expr = inner;
            } else {
                return trimmed;
            }
        } else {
            return trimmed;
        }
    }
}

fn balanced_parens(s: &str) -> bool {
    let mut depth = 0i32;
    for b in s.bytes() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

fn is_absolute_literal_expr(expr: &str) -> bool {
    let expr = strip_ref_and_parens(expr);
    if expr.starts_with("\"/") {
        return true;
    }
    for ctor in ["Path::new(", "PathBuf::from(", "OsStr::new("] {
        if let Some(rest) = expr.strip_prefix(ctor)
            && let Some(inner) = rest.strip_suffix(')')
            && inner.trim().starts_with("\"/")
        {
            return true;
        }
    }
    ABSOLUTE_PATH_CONSTS.contains(&expr)
}

fn expr_uses_resolver(expr: &str) -> bool {
    expr.contains("resolve_program(") || expr.contains("require_absolute_program(")
}

/// Whether `arg_raw` (from `Command::new(arg_raw)`) is statically proven to be an absolute path
/// literal/constant or initialized via `resolve_program` / `require_absolute_program` in the
/// enclosing function before `at`.
fn program_is_statically_safe(
    raw: &str,
    code: &str,
    fn_start: usize,
    at: usize,
    arg_raw: &str,
) -> bool {
    let core = strip_ref_and_parens(arg_raw);
    if is_absolute_literal_expr(core) || expr_uses_resolver(core) {
        return true;
    }
    if core.is_empty() || !core.bytes().all(is_ident) {
        return false;
    }
    // Look for the latest `let [mut] <core> [: ...] = <init>;` before `at` in the same function.
    let prefix_code = &code[fn_start..at];
    let prefix_raw = &raw[fn_start..at];
    let mut latest_init: Option<&str> = None;
    for off in scan::find_in(prefix_code, "let ") {
        let rest = prefix_code[off + 4..].trim_start();
        let rest = rest
            .strip_prefix("mut")
            .filter(|r| r.as_bytes().first().is_some_and(u8::is_ascii_whitespace))
            .map_or(rest, str::trim_start);
        if !rest.starts_with(core) {
            continue;
        }
        let after_ident = &rest[core.len()..];
        if after_ident
            .as_bytes()
            .first()
            .copied()
            .is_some_and(is_ident)
        {
            continue;
        }
        let after_ident_trimmed = after_ident.trim_start();
        // Skip optional type annotation `: Type` before `=`.
        let eq_offset_in_rest = if let Some(after_eq) = after_ident_trimmed.strip_prefix('=') {
            if after_eq.starts_with('=') {
                continue;
            }
            rest.len() - after_eq.len()
        } else if after_ident_trimmed.starts_with(':') {
            let Some(eq_rel) = after_ident_trimmed.find('=') else {
                continue;
            };
            if after_ident_trimmed.as_bytes().get(eq_rel + 1) == Some(&b'=') {
                continue;
            }
            rest.len() - (after_ident_trimmed.len() - eq_rel - 1)
        } else {
            continue;
        };
        let init_start =
            (off + 4) + (prefix_code[off + 4..].len() - rest.len()) + eq_offset_in_rest;
        let init_end = statement_end(prefix_code, init_start, prefix_code.len());
        latest_init = Some(prefix_raw[init_start..init_end].trim());
    }
    latest_init.is_some_and(|init| {
        let init = init.trim_end_matches(';').trim();
        expr_uses_resolver(init) || is_absolute_literal_expr(init)
    })
}

fn scan_source(file: &str, raw: &str, code: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for pat in command_ctor_patterns(code) {
        for at in scan::find_in(code, &pat) {
            let open = at + pat.len() - 1;
            let close = matching_delim(code.as_bytes(), open, b'(', b')');
            let (fn_start, fn_end) = enclosing_fn_body(code, at);
            let Some(reason) =
                command_modifies_path_or_clears_env(raw, code, fn_start, fn_end, at, close)
            else {
                continue;
            };
            let arg_raw = raw[open + 1..close].trim();
            if program_is_statically_safe(raw, code, fn_start, at, arg_raw) {
                continue;
            }
            hits.push(Hit {
                file: file.to_string(),
                line: code[..at].matches('\n').count() + 1,
                program: arg_raw.to_string(),
                reason,
            });
        }
    }
    hits.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.program.cmp(&b.program)));
    hits
}

fn found() -> (BTreeMap<String, usize>, Vec<Hit>) {
    let crates = crates_dir();
    let mut names: Vec<String> = std::fs::read_dir(&crates)
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("Cargo.toml").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut all_hits = Vec::new();
    for name in names {
        for s in scan::crate_sources(&crates, &name) {
            let raw = std::fs::read_to_string(crates.join(&s.rel)).unwrap();
            let hits = scan_source(&s.rel, &raw, &s.code);
            if !hits.is_empty() {
                *counts.entry(s.rel.clone()).or_default() += hits.len();
                all_hits.extend(hits);
            }
        }
    }
    (counts, all_hits)
}

#[test]
fn spawn_commands_with_modified_path_resolve_program() {
    let (counts, hits) = found();
    let now: ratchet::Counts = counts
        .into_iter()
        .map(|(file, n)| (vec![file], n))
        .collect();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(LIST);
    let mode = ratchet::mode();
    if mode == ratchet::Mode::Check && path.exists() {
        let listed = ratchet::read(&path, 1);
        let problems = ratchet::differences(
            &now,
            &listed,
            "resolve program with steve_runtime::process::resolve_program / require_absolute_program or pass an absolute path literal",
        );
        if !problems.is_empty() {
            let detail: Vec<String> = hits
                .iter()
                .map(|h| {
                    format!(
                        "  {}:{} Command::new({}) with {}",
                        h.file, h.line, h.program, h.reason
                    )
                })
                .collect();
            panic!(
                "source guard {}:\n{}\n\nDetected sites:\n{}",
                path.display(),
                problems.join("\n"),
                detail.join("\n")
            );
        }
    } else {
        ratchet::check_at(
            now,
            &path,
            1,
            HEADER,
            "resolve program with steve_runtime::process::resolve_program / require_absolute_program or pass an absolute path literal",
            mode,
        );
    }
}

#[test]
fn every_listed_exception_has_a_rationale_and_none_is_stale() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(LIST);
    let listed = ratchet::read(&path, 1);
    let listed_files: BTreeSet<&str> = listed.keys().map(|k| k[0].as_str()).collect();
    let rationale_files: BTreeSet<&str> = RATIONALE.iter().map(|(f, _)| *f).collect();
    assert_eq!(
        RATIONALE.len(),
        rationale_files.len(),
        "duplicate file in RATIONALE"
    );
    let missing: Vec<_> = listed_files.difference(&rationale_files).collect();
    let stale: Vec<_> = rationale_files.difference(&listed_files).collect();
    assert!(
        missing.is_empty() && stale.is_empty(),
        "spawn_path_guard RATIONALE out of sync with {LIST}: missing={missing:?}, stale={stale:?}"
    );
}

#[test]
fn scanner_flags_unresolved_env_clear_and_path_and_accepts_resolved_or_absolute() {
    let sample = r#"
use std::process::Command as StdCmd;

fn bad_env_clear(prog: &str) {
    let mut cmd = std::process::Command::new(prog);
    cmd.env_clear();
}

fn bad_path_override() {
    let _ = std::process::Command::new("npx")
        .env("PATH", "/custom/bin")
        .spawn();
}

fn bad_envs(prog: &str, env: Vec<(String, String)>) {
    let mut cmd = tokio::process::Command::new(prog);
    cmd.envs(env);
}

fn bad_renamed(prog: &str) {
    let mut cmd = StdCmd::new(prog);
    cmd.env_clear();
}

fn bad_multiline_and_nested(opt: Option<&str>) {
    if let Some(bin) = opt {
        let mut command = std::process::Command::new(bin);
        command
            .arg("--check")
            .env_clear();
    }
}

fn good_resolved(prog: &std::ffi::OsStr, path: Option<&std::ffi::OsStr>) -> std::io::Result<()> {
    let resolved = steve_runtime::process::resolve_program(prog, path, None)?;
    let mut cmd = std::process::Command::new(&resolved);
    cmd.env_clear().env("PATH", "/custom");
    Ok(())
}

fn good_required(shell: &std::ffi::OsStr) -> std::io::Result<()> {
    let shell = steve_runtime::process::require_absolute_program(shell, "SHELL")?;
    let _ = std::process::Command::new(&shell)
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    Ok(())
}

fn good_absolute() {
    let _ = std::process::Command::new("/bin/sh")
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
}

fn good_unchanged_path() {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("status").env("GIT_TERMINAL_PROMPT", "0");
}

fn good_detached() {
    let mut cmd = steve_runtime::process::DetachedCommand::new("sh");
    cmd.env_clear().env("PATH", "/usr/bin:/bin");
}
"#;
    let code = scan::production(sample).unwrap();
    let hits = scan_source("test.rs", sample, &code);
    let progs: Vec<&str> = hits.iter().map(|h| h.program.as_str()).collect();
    assert_eq!(
        progs,
        vec!["prog", "\"npx\"", "prog", "prog", "bin"],
        "{hits:?}"
    );
}
