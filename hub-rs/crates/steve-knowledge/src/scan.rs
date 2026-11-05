//! Walking a folder source: a self-written, read-only traversal that never
//! leaves the root. Symbolic links are resolved and dropped when they point outside it; linked
//! directories are not entered (their real path inside the root is walked anyway, and this rules
//! out loops). Fixed exclusions: `.git/`, `node_modules/`, `.obsidian/`, `.trash/` and every
//! directory whose name starts with a dot.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// `include` when the source does not set one.
pub const DEFAULT_INCLUDE: [&str; 3] = ["**/*.md", "**/*.markdown", "**/*.txt"];
/// Directories never entered (besides all dot-directories).
pub const FIXED_EXCLUDE: [&str; 4] = [".git", "node_modules", ".obsidian", ".trash"];
/// Larger files are not read (`skipped`).
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Files per source; the rest are counted as `skipped`.
pub const MAX_FILES: usize = 20_000;

/// Include and exclude globs, relative to the root with `/` separators. `**` matches any number of
/// directories (also none), `*` and `?` stay within one path segment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Rules {
    /// Empty `include` means the default.
    pub fn new(include: Vec<String>, exclude: Vec<String>) -> Self {
        let include = if include.is_empty() {
            DEFAULT_INCLUDE.iter().map(|s| s.to_string()).collect()
        } else {
            include
        };
        Self { include, exclude }
    }

    pub fn wants_file(&self, rel: &str) -> bool {
        self.include.iter().any(|p| glob(p, rel)) && !self.exclude.iter().any(|p| glob(p, rel))
    }

    /// A directory is skipped when an exclude pattern matches it, or matches it as `dir/**`.
    pub fn skips_dir(&self, rel: &str) -> bool {
        self.exclude.iter().any(|p| {
            glob(p, rel)
                || p.strip_suffix("/**")
                    .or_else(|| p.strip_suffix('/'))
                    .is_some_and(|d| glob(d, rel))
        })
    }
}

/// Is a user-given pattern acceptable: relative, no `..`, no backslashes?
pub fn valid_pattern(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 200
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.split('/').any(|s| s == "..")
}

/// Glob match of a relative path (see [`Rules`]).
pub fn glob(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let s: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    segs(&p, &s)
}

fn segs(p: &[&str], s: &[&str]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((&"**", rest)) => segs(rest, s) || (!s.is_empty() && segs(p, &s[1..])),
        Some((first, rest)) => !s.is_empty() && seg(first, s[0]) && segs(rest, &s[1..]),
    }
}

/// `*` / `?` within one segment (iterative, with backtracking to the last star).
fn seg(p: &str, s: &str) -> bool {
    let p: Vec<char> = p.chars().collect();
    let s: Vec<char> = s.chars().collect();
    let (mut i, mut j) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while j < s.len() {
        if i < p.len() && (p[i] == '?' || p[i] == s[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some((i, j));
            i += 1;
        } else if let Some((si, sj)) = star {
            i = si + 1;
            j = sj + 1;
            star = Some((si, sj + 1));
        } else {
            return false;
        }
    }
    p[i..].iter().all(|c| *c == '*')
}

/// One file the walk found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the root, `/`-separated (the document's `ref`).
    pub rel: String,
    /// Where the bytes are (the resolved target for a link inside the root).
    pub path: PathBuf,
    pub size: u64,
    /// Unix milliseconds.
    pub mtime: i64,
}

/// What a walk found.
#[derive(Debug, Default)]
pub struct Walk {
    pub files: Vec<Entry>,
    /// Over [`MAX_FILE_BYTES`].
    pub too_big: Vec<Entry>,
    /// Wanted files beyond [`MAX_FILES`].
    pub over_limit: usize,
    /// Links pointing outside the root (or nowhere), not followed.
    pub links_refused: usize,
}

fn mtime_ms(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

/// Walks `root` (canonical) in name order. Unreadable subdirectories are skipped; only an
/// unreadable root is an error.
pub fn walk(root: &Path, rules: &Rules) -> io::Result<Walk> {
    let mut out = Walk::default();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    let mut first = true;
    while let Some((dir, prefix)) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if first => return Err(e),
            Err(e) => {
                tracing::debug!(?dir, "kb.folder_scan: unreadable directory: {e}");
                continue;
            }
        };
        first = false;
        let mut entries: Vec<fs::DirEntry> = rd.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        let mut subdirs = Vec::new();
        for e in entries {
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                continue; // not UTF-8: cannot be a `ref`
            };
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let Ok(ft) = e.file_type() else { continue };
            let (path, meta) = if ft.is_symlink() {
                match fs::canonicalize(e.path()) {
                    Ok(t) if t.starts_with(root) => match fs::metadata(&t) {
                        Ok(m) if m.is_file() => (t, m),
                        _ => continue, // linked directories are not entered
                    },
                    _ => {
                        out.links_refused += 1;
                        continue;
                    }
                }
            } else if ft.is_dir() {
                if name.starts_with('.') || FIXED_EXCLUDE.contains(&name.as_str()) {
                    continue;
                }
                if !rules.skips_dir(&rel) {
                    subdirs.push((e.path(), rel));
                }
                continue;
            } else if ft.is_file() {
                match e.metadata() {
                    Ok(m) => (e.path(), m),
                    Err(_) => continue,
                }
            } else {
                continue; // sockets, fifos, devices
            };
            if !rules.wants_file(&rel) {
                continue;
            }
            let entry = Entry {
                rel,
                path,
                size: meta.len(),
                mtime: mtime_ms(&meta),
            };
            if entry.size > MAX_FILE_BYTES {
                out.too_big.push(entry);
            } else if out.files.len() >= MAX_FILES {
                out.over_limit += 1;
            } else {
                out.files.push(entry);
            }
        }
        // Reverse so the stack pops them in name order.
        stack.extend(subdirs.into_iter().rev());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob("**/*.md", "a.md"));
        assert!(glob("**/*.md", "x/y/a.md"));
        assert!(!glob("**/*.md", "a.mdx"));
        assert!(glob("*.txt", "a.txt"));
        assert!(!glob("*.txt", "d/a.txt"));
        assert!(glob("d/**", "d/a/b.md"));
        assert!(glob("草稿/*", "草稿/想法.md"));
        assert!(glob("a?c.md", "abc.md"));
        assert!(glob("**/*draft*", "x/my-draft-1.md"));
        let r = Rules::new(vec![], vec!["私人/**".into(), "**/*.tmp.md".into()]);
        assert!(r.wants_file("a.md") && r.wants_file("x/b.txt"));
        assert!(!r.wants_file("x/b.tmp.md"));
        assert!(r.skips_dir("私人") && !r.skips_dir("公开"));
        assert!(valid_pattern("**/*.md"));
        assert!(!valid_pattern("/etc/*") && !valid_pattern("../x") && !valid_pattern(""));
    }

    #[test]
    fn walk_order_exclusions_and_limits() {
        let d = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(d.path()).unwrap();
        for p in [
            "b.md",
            "a.md",
            "sub/c.md",
            ".obsidian/x.md",
            ".hidden/y.md",
            "node_modules/z.md",
            "skip/w.md",
            "img.png",
        ] {
            let f = root.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(&f, "x").unwrap();
        }
        fs::write(root.join("big.md"), vec![b'a'; MAX_FILE_BYTES as usize + 1]).unwrap();
        let w = walk(&root, &Rules::new(vec![], vec!["skip/**".into()])).unwrap();
        let rels: Vec<&str> = w.files.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, ["a.md", "b.md", "sub/c.md"]);
        assert_eq!(w.too_big.len(), 1);
        assert!(walk(&root.join("nope"), &Rules::default()).is_err());
    }
}
