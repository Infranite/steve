//! Include and exclude patterns of a [`crate::ScanSpec`]: the one place their meaning is defined,
//! for the Node that scans and for the Hub that checks what a user typed (the knowledge folder's
//! rules, unchanged). Pure: no filesystem here.

/// `include` when a scan does not set one (the knowledge folder's default).
pub const DEFAULT_INCLUDE: [&str; 3] = ["**/*.md", "**/*.markdown", "**/*.txt"];
/// Directories a scan never enters (besides every directory whose name starts with a dot).
pub const FIXED_EXCLUDE: [&str; 4] = [".git", "node_modules", ".obsidian", ".trash"];

/// Include and exclude globs, relative to the scanned root with `/` separators. `**` matches any
/// number of directories (also none), `*` and `?` stay within one path segment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Rules {
    /// Empty `include` means [`DEFAULT_INCLUDE`].
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

/// A directory name a scan never enters, whatever the rules say.
pub fn never_entered(name: &str) -> bool {
    name.starts_with('.') || FIXED_EXCLUDE.contains(&name)
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
        assert!(never_entered(".hidden") && never_entered("node_modules"));
        assert!(!never_entered("notes"));
    }
}
