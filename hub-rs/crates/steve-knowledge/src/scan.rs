//! Walking a folder source: a self-written, read-only traversal that never
//! leaves the root. Symbolic links are resolved and dropped when they point outside it; linked
//! directories are not entered (their real path inside the root is walked anyway, and this rules
//! out loops). Fixed exclusions: `.git/`, `node_modules/`, `.obsidian/`, `.trash/` and every
//! directory whose name starts with a dot.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use steve_node_api::glob::never_entered;
pub use steve_node_api::glob::{DEFAULT_INCLUDE, FIXED_EXCLUDE, Rules, glob, valid_pattern};

/// Larger files are not read (`skipped`).
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Files per source; the rest are counted as `skipped`.
pub const MAX_FILES: usize = 20_000;

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

/// Why a walk did not see the whole folder. A walk with any of these must not be taken as
/// proof that a file is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Gap {
    /// A subdirectory could not be listed.
    Unreadable,
    /// A directory entry or its type could not be read.
    Entry,
    /// Wanted files beyond [`MAX_FILES`].
    OverLimit,
    /// The scan was cut short (a Node's scan: a restart, a cancel, the deadline).
    Interrupted,
}

impl Gap {
    /// Stable code for stats and the API.
    pub fn code(self) -> &'static str {
        match self {
            Gap::Unreadable => "unreadable",
            Gap::Entry => "entry",
            Gap::OverLimit => "over_limit",
            Gap::Interrupted => "interrupted",
        }
    }
}

/// Why a wanted path the walk saw was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// A link resolving outside the root: its document fails, as when the loader refuses it.
    Outside,
    /// Its metadata (or its link) could not be read: its document stays as it is.
    Unreadable,
    /// The Node that scanned it refuses the path: its document fails, as when reading it would.
    Denied,
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
    /// Wanted paths that exist but could not be read, and why. Never a reason to delete.
    pub held: Vec<(String, Held)>,
    /// Why parts of the folder were not seen (sorted, no repeats). Empty = complete.
    pub gaps: Vec<Gap>,
}

impl Walk {
    /// Did the walk see everything under the root? Only then may a document whose file was
    /// not found be deleted.
    pub fn complete(&self) -> bool {
        self.gaps.is_empty()
    }

    fn hold(&mut self, rules: &Rules, rel: String, why: Held) {
        if rules.wants_file(&rel) {
            self.held.push((rel, why));
        }
    }

    pub(crate) fn gap(&mut self, g: Gap) {
        if let Err(i) = self.gaps.binary_search(&g) {
            self.gaps.insert(i, g);
        }
    }
}

fn mtime_ms(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

/// Walks `root` (canonical) in name order. Unreadable subdirectories are skipped and recorded
/// in [`Walk::gaps`]; only an unreadable root is an error.
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
                out.gap(Gap::Unreadable);
                continue;
            }
        };
        first = false;
        let mut entries: Vec<fs::DirEntry> = Vec::new();
        for e in rd {
            match e {
                Ok(e) => entries.push(e),
                Err(_) => out.gap(Gap::Entry),
            }
        }
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
            let Ok(ft) = e.file_type() else {
                out.gap(Gap::Entry); // could be a directory
                continue;
            };
            let (path, meta) = if ft.is_symlink() {
                match fs::canonicalize(e.path()) {
                    Ok(t) if t.starts_with(root) => match fs::metadata(&t) {
                        Ok(m) if m.is_file() => (t, m),
                        Ok(_) => continue, // linked directories are not entered
                        Err(_) => {
                            out.hold(rules, rel, Held::Unreadable);
                            continue;
                        }
                    },
                    Ok(_) => {
                        out.links_refused += 1;
                        out.hold(rules, rel, Held::Outside);
                        continue;
                    }
                    // dangling: the file is gone, as the loader sees it
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        out.links_refused += 1;
                        continue;
                    }
                    Err(_) => {
                        out.links_refused += 1;
                        out.hold(rules, rel, Held::Unreadable);
                        continue;
                    }
                }
            } else if ft.is_dir() {
                if never_entered(&name) {
                    continue;
                }
                if !rules.skips_dir(&rel) {
                    subdirs.push((e.path(), rel));
                }
                continue;
            } else if ft.is_file() {
                match e.metadata() {
                    Ok(m) => (e.path(), m),
                    Err(_) => {
                        out.hold(rules, rel, Held::Unreadable);
                        continue;
                    }
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
                out.gap(Gap::OverLimit);
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

    #[test]
    fn a_full_walk_is_complete() {
        let d = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(d.path()).unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/a.md"), "x").unwrap();
        let w = walk(&root, &Rules::new(vec![], vec![])).unwrap();
        assert!(w.complete() && w.held.is_empty(), "{w:?}");
    }

    #[test]
    fn an_unreadable_subdirectory_makes_the_walk_incomplete() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(d.path()).unwrap();
        for p in ["a.md", "locked/b.md"] {
            let f = root.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(&f, "x").unwrap();
        }
        let locked = root.join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(&locked).is_ok(); // root ignores the mode
        let w = walk(&root, &Rules::new(vec![], vec![])).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if readable {
            return;
        }
        assert_eq!(w.files.len(), 1);
        assert_eq!(w.gaps, [Gap::Unreadable]);
        assert!(!w.complete());
    }

    #[test]
    fn over_the_file_limit_is_incomplete() {
        let d = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(d.path()).unwrap();
        for i in 0..=MAX_FILES {
            fs::write(root.join(format!("{i:05}.md")), "").unwrap();
        }
        let w = walk(&root, &Rules::new(vec![], vec![])).unwrap();
        assert_eq!((w.files.len(), w.over_limit), (MAX_FILES, 1));
        assert_eq!(w.gaps, [Gap::OverLimit]);
    }

    #[test]
    fn outside_links_are_held_when_wanted() {
        let d = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(d.path()).unwrap();
        let (root, outside) = (base.join("root"), base.join("outside"));
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("s.md"), "x").unwrap();
        let ln = |to: &Path, at: &str| std::os::unix::fs::symlink(to, root.join(at)).unwrap();
        ln(&outside.join("s.md"), "out.md");
        ln(&outside, "out-dir");
        ln(&root.join("missing.md"), "dangling.md");
        let w = walk(&root, &Rules::new(vec![], vec![])).unwrap();
        assert_eq!(w.links_refused, 3);
        assert_eq!(w.held, [("out.md".to_string(), Held::Outside)]);
        assert!(w.complete(), "a refused link is seen, not missed");
    }
}
