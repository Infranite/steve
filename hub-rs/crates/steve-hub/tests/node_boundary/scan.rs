//! A small reader of Rust sources for the Node boundary guard: it follows each crate's module tree
//! from `src/lib.rs` and `src/main.rs`, leaves out comments, literals and test-only items
//! (`#[cfg(test)]`, the `fake` and `testkit` features), and finds the places a pattern occurs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One production source file: its path under `hub-rs/crates/` and its code with comments, string
/// and char literals and test-only items blanked out (same byte offsets, same lines).
pub struct Source {
    pub rel: String,
    pub code: String,
}

/// One occurrence of a pattern: the file under `hub-rs/crates/` and the 1-based line.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Site {
    pub file: String,
    pub line: usize,
}

/// Every production file of `crate_dir` (`hub-rs/crates/<name>`), reached from its roots.
pub fn crate_sources(crates: &Path, name: &str) -> Vec<Source> {
    let src = crates.join(name).join("src");
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for root in ["lib.rs", "main.rs"] {
        let p = src.join(root);
        if p.exists() {
            walk(crates, &p, &mut seen, &mut out);
        }
    }
    out
}

fn walk(crates: &Path, file: &Path, seen: &mut BTreeSet<PathBuf>, out: &mut Vec<Source>) {
    if !seen.insert(file.to_path_buf()) {
        return;
    }
    let raw = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    let code = production(&raw);
    let Some(code) = code else {
        return; // `#![cfg(test)]` on the whole file
    };
    for (name, path_attr) in module_decls(&raw, &code) {
        let dir = file.parent().unwrap();
        let child = match path_attr {
            Some(p) => Some(dir.join(p)),
            None => {
                let stem = file.file_stem().unwrap().to_str().unwrap();
                let base = if matches!(stem, "lib" | "main" | "mod") {
                    dir.to_path_buf()
                } else {
                    dir.join(stem)
                };
                [
                    base.join(format!("{name}.rs")),
                    base.join(&name).join("mod.rs"),
                ]
                .into_iter()
                .find(|p| p.exists())
            }
        };
        let child =
            child.unwrap_or_else(|| panic!("{}: no file for `mod {name};`", file.display()));
        walk(crates, &child, seen, out);
    }
    let rel = file
        .strip_prefix(crates)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    out.push(Source { rel, code });
}

/// The places `pattern` starts in the code, each as a file and line. A pattern starting with an
/// identifier character only matches at the start of a word.
pub fn find(sources: &[Source], pattern: &str) -> Vec<Site> {
    let mut sites = Vec::new();
    for s in sources {
        for at in find_in(&s.code, pattern) {
            sites.push(Site {
                file: s.rel.clone(),
                line: s.code[..at].matches('\n').count() + 1,
            });
        }
    }
    sites
}

/// The byte offsets where `pattern` starts in `code`, at a word start when it begins with one.
pub fn find_in(code: &str, pattern: &str) -> Vec<usize> {
    let word = pattern.as_bytes().first().is_some_and(|b| ident(*b));
    let bytes = code.as_bytes();
    code.match_indices(pattern)
        .map(|(at, _)| at)
        .filter(|&at| !word || at == 0 || !ident(bytes[at - 1]))
        .collect()
}

fn ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `src` with comments and literals blanked and test-only items removed; `None` when the whole
/// file is test-only.
pub fn production(src: &str) -> Option<String> {
    let blank = blank_literals(src);
    let lead = blank.len() - blank.trim_start().len();
    if blank[lead..].starts_with("#![") {
        let end = matching(blank.as_bytes(), lead + 2, b'[', b']');
        if test_only(&src[lead + 3..end]) {
            return None;
        }
    }
    Some(strip_test_items(src, &blank))
}

/// Comments, string, byte-string, raw-string and char literals become spaces; newlines stay, so
/// offsets and line numbers are those of the source. String delimiters stay.
pub fn blank_literals(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for c in &mut out[from..to] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let prev_ident = i > 0 && ident(b[i - 1]);
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            let end = src[i..].find('\n').map_or(b.len(), |n| i + n);
            blank(&mut out, i, end);
            i = end;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 0;
            let mut j = i;
            while j < b.len() {
                if b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
                    depth += 1;
                    j += 2;
                } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            blank(&mut out, i, j);
            i = j;
        } else if !prev_ident && (c == b'r' || (c == b'b' && b.get(i + 1) == Some(&b'r'))) && {
            let k = if c == b'b' { i + 2 } else { i + 1 };
            let hashes = b[k..].iter().take_while(|&&h| h == b'#').count();
            b.get(k + hashes) == Some(&b'"')
        } {
            let k = if c == b'b' { i + 2 } else { i + 1 };
            let hashes = b[k..].iter().take_while(|&&h| h == b'#').count();
            let open = k + hashes + 1;
            let close: String = std::iter::once('"')
                .chain(std::iter::repeat_n('#', hashes))
                .collect();
            let end = src[open..].find(&close).map_or(b.len(), |n| open + n);
            blank(&mut out, open, end);
            i = (end + close.len()).min(b.len());
        } else if c == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            let j = j.min(b.len());
            blank(&mut out, i + 1, j);
            i = j + 1;
        } else if c == b'\'' {
            // a char literal ('x', '\n', '\u{..}', 'é') or a lifetime / label ('a)
            if b.get(i + 1) == Some(&b'\\') {
                let end = src[i + 2..].find('\'').map_or(b.len(), |n| i + 2 + n);
                blank(&mut out, i + 1, end);
                i = end + 1;
            } else if let Some(ch) = src[i + 1..].chars().next() {
                let after = i + 1 + ch.len_utf8();
                if b.get(after) == Some(&b'\'') {
                    blank(&mut out, i + 1, after);
                    i = after + 1;
                } else {
                    i += 1;
                }
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("blanking keeps UTF-8")
}

/// Whether an attribute body (`cfg(test)`, `cfg(all(test, unix))`, `cfg(feature = "fake")`) only
/// holds in test and fake builds.
fn test_only(attr: &str) -> bool {
    let Some(expr) = attr.trim().strip_prefix("cfg") else {
        return false;
    };
    if !expr.trim_start().starts_with('(') || expr.contains("not(") {
        return false;
    }
    find_in(expr, "test")
        .iter()
        .any(|&at| !expr.as_bytes().get(at + 4).copied().is_some_and(ident))
        || expr.contains("\"fake\"")
        || expr.contains("\"testkit\"")
}

/// Blanks every item under a test-only `#[cfg(..)]`: up to its `;`, or its `{..}` block.
fn strip_test_items(raw: &str, code: &str) -> String {
    let b = code.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'#' && b.get(i + 1) == Some(&b'[') {
            let attr_start = i;
            let mut test = false;
            // the run of outer attributes on this item
            let mut j = i;
            while j < b.len() && b[j] == b'#' && b.get(j + 1) == Some(&b'[') {
                let end = matching(b, j + 1, b'[', b']');
                test |= test_only(&raw[j + 2..end]);
                j = end + 1;
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
            }
            if test {
                let end = item_end(b, j);
                for c in &mut out[attr_start..end] {
                    if *c != b'\n' {
                        *c = b' ';
                    }
                }
                i = end;
            } else {
                i = j.max(i + 1);
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("blanking keeps UTF-8")
}

/// The offset of the bracket closing the one at `open`.
fn matching(b: &[u8], open: usize, l: u8, r: u8) -> usize {
    let mut depth = 0;
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
    b.len() - 1
}

/// The end of the item starting at `from`: past its first `;` at depth 0, or its first `{..}`. An
/// attributed field or argument (`#[cfg(test)] tool_service,`) ends at its `,`, or before the
/// bracket that closes its list.
fn item_end(b: &[u8], from: usize) -> usize {
    let (mut depth, mut angle) = (0i32, 0i32);
    let mut k = from;
    while k < b.len() {
        match b[k] {
            b'(' | b'[' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return k,
            b')' | b']' => depth -= 1,
            // generics (`fn f<A, B>`, `HashMap<K, V>`), not `->` or `=>`
            b'<' => angle += 1,
            b'>' if k > 0 && !matches!(b[k - 1], b'-' | b'=') => angle -= 1,
            b',' if depth == 0 && angle <= 0 => return k + 1,
            b';' if depth == 0 => return k + 1,
            b'{' if depth == 0 => return matching(b, k, b'{', b'}') + 1,
            _ => {}
        }
        k += 1;
    }
    b.len()
}

/// The `mod name;` declarations left in the production code, each with its `#[path]` if any.
fn module_decls(raw: &str, code: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for at in find_in(code, "mod ") {
        let rest = &code[at + 4..];
        let name: String = rest
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let after = rest.trim_start()[name.len()..].trim_start();
        if name.is_empty() || !after.starts_with(';') {
            continue;
        }
        // a `#[path = ".."]` right before it (the literal is read from the source)
        let before = code[..at].trim_end();
        let before = match before.strip_suffix("pub") {
            Some(b) => b,
            None if before.ends_with(')') => before.rfind("pub(").map_or(before, |p| &before[..p]),
            None => before,
        }
        .trim_end();
        let path = before.strip_suffix(']').and_then(|b| {
            let open = b.rfind("#[")?;
            let attr = &b[open + 2..];
            let attr = attr.trim_start();
            if !attr.starts_with("path") {
                return None;
            }
            let lit = &raw[open + 2..before.len() - 1];
            let q = lit.find('"')?;
            let e = lit[q + 1..].find('"')?;
            Some(lit[q + 1..q + 1 + e].to_string())
        });
        out.push((name, path));
    }
    out
}

/// Every `use` declaration in the code, flattened to one path per imported name (`a::b::c`, or
/// `a::b::c as d`, or `a::b::*`), each with the offset of its `use`. `use a::{b, c::{self, d as
/// e}};` gives `a::b`, `a::c`, `a::c::d as e`.
pub fn uses(code: &str) -> Vec<(usize, String)> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for at in find_in(code, "use ") {
        let end = code[at..].find(';').map_or(b.len(), |n| at + n);
        let tree: String = squeeze(&code[at + 4..end]);
        let mut paths = Vec::new();
        flatten("", &tree, &mut paths);
        out.extend(paths.into_iter().map(|p| (at, p)));
    }
    out
}

/// Whitespace removed, except one space around `as`.
fn squeeze(tree: &str) -> String {
    let words: Vec<&str> = tree.split_whitespace().collect();
    let mut s = String::new();
    for (k, w) in words.iter().enumerate() {
        if *w == "as" || (k > 0 && words[k - 1] == "as") {
            s.push(' ');
        }
        s.push_str(w);
    }
    s
}

fn flatten(prefix: &str, tree: &str, out: &mut Vec<String>) {
    let tree = tree.trim().trim_start_matches("::");
    let join = |a: &str, b: &str| {
        let b = b.trim_end_matches("::");
        match (a.is_empty(), b.is_empty()) {
            (true, _) => b.to_string(),
            (_, true) => a.to_string(),
            _ => format!("{a}::{b}"),
        }
    };
    if tree.is_empty() {
        return;
    }
    let Some(open) = tree.find('{') else {
        out.push(if tree == "self" {
            prefix.to_string()
        } else if let Some(alias) = tree.strip_prefix("self as ") {
            format!("{prefix} as {alias}")
        } else {
            join(prefix, tree)
        });
        return;
    };
    let head = join(prefix, &tree[..open]);
    let close = matching(tree.as_bytes(), open, b'{', b'}');
    let inner = &tree[open + 1..close];
    let (mut depth, mut from) = (0, 0);
    for (k, c) in inner.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                flatten(&head, &inner[from..k], out);
                from = k + 1;
            }
            _ => {}
        }
    }
    flatten(&head, &inner[from..], out);
}

/// Whether the flattened import `flat` brings in `item` (a full path) under a name the call
/// patterns may not see. `any`: the item itself, anything under it, or a renamed or glob import
/// of a module above it. Otherwise only a rename of the item itself counts (its calls are found by
/// its own name, `Command::new(`, so only a new name hides them).
pub fn imports(flat: &str, item: &str, any: bool) -> bool {
    let (path, alias) = match flat.split_once(" as ") {
        Some((p, a)) => (p, Some(a)),
        None => (flat, None),
    };
    let last = item.rsplit("::").next().unwrap_or(item);
    let renamed = alias.is_some_and(|a| a != "_" && a != last);
    if !any {
        return path == item && renamed;
    }
    let (base, glob) = match path.strip_suffix("::*") {
        Some(b) => (b, true),
        None => (path, false),
    };
    let inside = base == item || base.starts_with(&format!("{item}::"));
    let above = item.starts_with(&format!("{base}::"));
    inside || (above && (glob || renamed))
}

/// The imported names in `sources` that `hit` picks, one site each.
pub fn find_imports(sources: &[Source], hit: impl Fn(&str) -> bool) -> Vec<Site> {
    let mut sites = Vec::new();
    for s in sources {
        for (at, flat) in uses(&s.code) {
            if hit(&flat) {
                sites.push(Site {
                    file: s.rel.clone(),
                    line: s.code[..at].matches('\n').count() + 1,
                });
            }
        }
    }
    sites
}
