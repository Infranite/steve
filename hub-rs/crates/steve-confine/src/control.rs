use std::path::{Path, PathBuf};

/// The parts of `STEVE_HOME` no agent reads, relative to it. A trailing `/` is a directory (all of it),
/// a trailing `*` a name prefix.
pub const PROTECTED: [&str; 14] = [
    "signing/",
    "secrets/",
    "backups/",
    // the upgrade journal: the control token's file, the supervisor's database copies, runner jobs
    "updates/",
    "steve.env*",
    "steve.db*",
    "data/steve.sqlite3*",
    "data/files/",
    // raw mail (steve-mail: `data/mail/<ws>/…/<id>.eml`) and the Hub's state per paired Node
    "data/mail/",
    "data/nodes/",
    "data/node/node.sqlite3*",
    "data/node/secrets/",
    "data/node/push/",
    "data/node/blobs/",
];

/// `steve_home` with symlinks resolved when it exists (the harnesses match real paths).
pub fn canonical_path(steve_home: &Path) -> PathBuf {
    std::fs::canonicalize(steve_home).unwrap_or_else(|_| steve_home.to_path_buf())
}

/// The parts of a Node's own home (its journal, and the secrets, pushes and attachments it keeps
/// for runs) no run reads, relative to it. For the Hub's local Node they are also in [`PROTECTED`].
pub const NODE_PROTECTED: [&str; 6] = [
    "node.sqlite3*",
    "secrets/",
    "push/",
    "blobs/",
    // every principal's own harness directories and caches: a run is given back its own
    "state/",
    "cache/",
];

/// A remote Node's own files, relative to its `$STEVE_HOME` (`~/.steve-node`): its identity, its
/// pairing and its owner's policy, roots and harness settings.
pub const NODE_FILES: [&str; 5] = [
    "node/identity.key",
    "node/node.json",
    "policy.json",
    "roots.json",
    "harness.json",
];

/// Steve's own programs and their builds under `STEVE_HOME`: a run may read them, never change
/// them (a run that could replace the Hub's binary would run as the Hub next time).
pub const SEALED: [&str; 6] = ["releases/", "current", "web/", "bin/", "build/", "plugins/"];

/// One SBPL path filter for `entry` under `root`: a trailing `/` is the directory, a trailing `*`
/// a name prefix (a regex), else the one path. `None`: a path SBPL could not quote safely.
fn sbpl_filter(root: &str, entry: &str) -> Option<Vec<String>> {
    if root
        .chars()
        .any(|c| c == '"' || c == '\\' || c.is_control())
    {
        return None;
    }
    Some(if let Some(dir) = entry.strip_suffix('/') {
        vec![format!("(subpath \"{root}/{dir}\")")]
    } else if let Some(prefix) = entry.strip_suffix('*') {
        let mut re = String::from("^");
        for c in format!("{root}/{prefix}").chars() {
            if ".^$|?*+()[]{}".contains(c) {
                re.push('\\');
            }
            re.push(c);
        }
        vec![format!("(regex #\"{re}\")")]
    } else {
        // the path itself (a symlink such as `current` is renamed or removed by its own name) and,
        // when it is a directory, what is under it
        vec![
            format!("(literal \"{root}/{entry}\")"),
            format!("(subpath \"{root}/{entry}\")"),
        ]
    })
}

/// `root` as given and, when it differs, with symlinks resolved (the sandbox sees real paths).
fn roots_of(root: &Path) -> Vec<String> {
    let mut out = vec![root.to_string_lossy().trim_end_matches('/').to_string()];
    let r = canonical_path(root)
        .to_string_lossy()
        .trim_end_matches('/')
        .to_string();
    if r != out[0] {
        out.push(r);
    }
    out
}

/// Every directory a rule's path goes through, from `/` down to the entry's parent: renaming one
/// would move the entry to a path no rule names (`mv ~/.steve/data d2; cat d2/files/…`).
fn ancestors_of(root: &str, entry: &str, out: &mut std::collections::BTreeSet<String>) {
    let mut p = PathBuf::from(format!("{root}/{}", entry.trim_end_matches(['/', '*'])));
    while let Some(up) = p.parent() {
        out.insert(up.to_string_lossy().into_owned());
        p = up.to_path_buf();
    }
}

/// The control plane as seatbelt (macOS `sandbox-exec`) rules, for any process a Node starts for a
/// run: [`PROTECTED`] and [`NODE_FILES`] under `steve_home` and [`NODE_PROTECTED`] under `node_home`
/// can be neither read nor written, [`SEALED`] under `steve_home` not written, and no directory on
/// the way to them renamed or removed (making files inside those stays open). Seatbelt checks the
/// real path whatever a process names: a symlink, a hard link it would make, another case, `..`.
/// They go after a profile's own rules, so they win over its allows. `None`: a path SBPL cannot
/// quote.
pub(super) fn control_rules(steve_home: &Path, node_home: &Path) -> Option<String> {
    let mut hidden = Vec::new();
    let mut sealed = Vec::new();
    let mut ways = std::collections::BTreeSet::new();
    for root in roots_of(steve_home) {
        for e in PROTECTED.iter().chain(NODE_FILES.iter()) {
            hidden.extend(sbpl_filter(&root, e)?);
            ancestors_of(&root, e, &mut ways);
        }
        for e in SEALED {
            sealed.extend(sbpl_filter(&root, e)?);
            ancestors_of(&root, e, &mut ways);
        }
    }
    for root in roots_of(node_home) {
        for e in NODE_PROTECTED {
            hidden.extend(sbpl_filter(&root, e)?);
            ancestors_of(&root, e, &mut ways);
        }
    }
    let ways: Vec<String> = ways.iter().map(|w| format!("(literal \"{w}\")")).collect();
    Some(format!(
        "(deny file-read* file-write*\n  {})\n(deny file-write*\n  {})\n(deny file-write*\n  {})\n",
        hidden.join("\n  "),
        sealed.join("\n  "),
        ways.join("\n  ")
    ))
}

/// Where runs write under `STEVE_HOME`: the items' worktrees, the scratch directories and the
/// Node's own directories for them (its jobs, its worktrees).
pub const RUN_DIRS: [&str; 3] = ["worktrees", "scratch", "data/node"];

/// The same protection as [`crate::seatbelt_rules`] for bubblewrap (Linux `bwrap`), as an allowlist: all
/// of `steve_home` is covered by an empty tmpfs (so nothing there is readable, including files the
/// Hub writes later), the run directories in it and `node_home` are bound back writable, and inside
/// those [`NODE_PROTECTED`] is hidden again: a directory by its own tmpfs, a file by `/dev/null` (a
/// name prefix matches the files there now; the journal's are made when the Node opens). Real,
/// absolute paths, in mount order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BwrapPlan {
    pub masked: Vec<PathBuf>,
    pub writable: Vec<PathBuf>,
    pub hidden_dirs: Vec<PathBuf>,
    pub hidden_files: Vec<PathBuf>,
}

pub fn bwrap_plan(steve_home: &Path, node_home: &Path) -> BwrapPlan {
    let (sh, nh) = (canonical_path(steve_home), canonical_path(node_home));
    let mut p = BwrapPlan::default();
    if sh.is_dir() {
        p.masked.push(sh.clone());
    }
    for d in RUN_DIRS {
        if sh.join(d).is_dir() {
            p.writable.push(sh.join(d));
        }
    }
    if nh.is_dir() && !p.writable.iter().any(|w| nh.starts_with(w)) {
        p.writable.push(nh.clone());
    }
    for e in NODE_PROTECTED {
        match (e.strip_suffix('/'), e.strip_suffix('*')) {
            (Some(dir), _) => p.hidden_dirs.push(nh.join(dir)),
            (_, Some(prefix)) => {
                if let Ok(rd) = std::fs::read_dir(&nh) {
                    for f in rd.flatten() {
                        if f.file_name().to_string_lossy().starts_with(prefix) {
                            p.hidden_files.push(f.path());
                        }
                    }
                }
            }
            _ => p.hidden_files.push(nh.join(e)),
        }
    }
    p.hidden_files.retain(|f| f.is_file());
    for v in [&mut p.hidden_dirs, &mut p.hidden_files] {
        v.sort();
        v.dedup();
    }
    p
}
