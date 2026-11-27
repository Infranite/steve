//! Pure git fingerprint parsing and diff-summary formatting.
//!
//! All live git inspection (`own_change`, `landed`, `diff`) runs on the Node (`steve-node/src/git.rs`
//! and `steve-node/src/housekeeping.rs`); `steve-core` never spawns `git` in production.

/// A fingerprint's parts: `head:tree[:base:patch]` (the short form is the one written before own
/// changes were recorded).
pub struct Print<'a> {
    pub head: &'a str,
    pub tree: &'a str,
    pub own: Option<(&'a str, &'a str)>,
}

pub fn parse(f: &str) -> Print<'_> {
    let mut p = f.split(':');
    let head = p.next().unwrap_or("");
    let tree = p.next().unwrap_or("");
    let own = match (p.next(), p.next()) {
        (Some(b), Some(x)) => Some((b, x)),
        _ => None,
    };
    Print { head, tree, own }
}

/// The pairs of trees (or commits) [`describe_with`] compares, for `frozen` and `now`.
pub(crate) fn describe_pairs(frozen: &str, now: &str) -> Vec<(String, String)> {
    let (a, b) = (parse(frozen), parse(now));
    let mut v = vec![(a.tree.to_string(), b.tree.to_string())];
    if let (Some((ba, _)), Some((bb, _))) = (a.own, b.own) {
        v.push((ba.to_string(), a.tree.to_string()));
        v.push((bb.to_string(), b.tree.to_string()));
    }
    v
}

/// What changed between two fingerprints of a worktree, in words for the timeline and the agent,
/// given the file paths that differ between two trees (`names` supplied by the Node).
pub(crate) fn describe_with(
    frozen: &str,
    now: &str,
    names: impl Fn(&str, &str) -> Vec<String>,
) -> String {
    let (a, b) = (parse(frozen), parse(now));
    let mut parts = vec![];
    if let (Some((ba, _)), Some((bb, _))) = (a.own, b.own)
        && ba != bb
    {
        parts.push("分支换到了新的基线".to_string());
    }
    let changed = names(a.tree, b.tree);
    /* only what the item itself touches, then or now: a new base brings other people's files along */
    let own: Vec<String> = match (a.own, b.own) {
        (Some((ba, _)), Some((bb, _))) => {
            let mut v = names(ba, a.tree);
            v.extend(names(bb, b.tree));
            v
        }
        _ => changed.clone(),
    };
    let files: Vec<&String> = changed.iter().filter(|f| own.contains(f)).collect();
    if !files.is_empty() {
        let shown: Vec<&str> = files.iter().take(3).map(|f| f.as_str()).collect();
        parts.push(if files.len() > 3 {
            format!("改了 {} 等 {} 个文件", shown.join("、"), files.len())
        } else {
            format!("改了 {}", shown.join("、"))
        });
    }
    if parts.is_empty() {
        "工作区的内容变了".into()
    } else {
        parts.join("，")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    Absent,
    /// The commit on the target that brought the change in.
    Landed {
        commit: String,
    },
    Unknown(String),
}

#[cfg(test)]
pub(crate) fn describe_change(dir: &std::path::Path, frozen: &str, now: &str) -> String {
    describe_with(frozen, now, |from, to| {
        crate::git::tests::git(dir, &["diff", "--name-only", from, to])
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_parse_in_both_forms() {
        let p = parse("h:t");
        assert_eq!((p.head, p.tree, p.own), ("h", "t", None));
        let p = parse("h:t:b:x");
        assert_eq!(p.own, Some(("b", "x")));
    }
}
