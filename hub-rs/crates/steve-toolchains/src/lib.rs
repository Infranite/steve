//! Toolchains as data (path model §6.5): which build tools a project uses, where each one's
//! caches and outputs go (one item's own, or one person's, shared by their items), and what Steve
//! sets for them. The definitions ship in `data/builtin.toml`; this code knows no tool by name, so
//! a new language is a new entry, never a code change.
//!
//! What a run gets from it is a list of variables ([`Toolchain::env`]) that move the caches out of
//! the user's home and the shared places into the item's `.steve/build/<id>` and the person's own
//! cache, plus the tool's fixed settings (telemetry off, copies instead of hard links).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const BUILTIN: &str = include_str!("../data/builtin.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Each item its own, never shared.
    Item,
    /// Each item its own, to be started from that person's last good build (seeds come later).
    Seed,
    /// One person's, shared by their items: only what the tool locks itself or only adds to.
    Person,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dir {
    pub name: String,
    pub scope: Scope,
    /// The variable that moves it.
    #[serde(default)]
    pub env: Option<String>,
    /// What [`Self::env`] is set to: `{item}` and `{person}` are the item's and the person's
    /// directories for this toolchain.
    #[serde(default)]
    pub value: Option<String>,
    /// Where it stays, relative to the project, when it cannot be moved.
    #[serde(default)]
    pub in_tree: Option<String>,
    #[serde(default)]
    pub concurrent_safe: bool,
    /// Only a cache: the whole directory may be given back when the disk is low.
    #[serde(default)]
    pub disposable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolOption {
    pub default: bool,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Toolchain {
    pub id: String,
    pub detect: Vec<String>,
    #[serde(default)]
    pub key: Vec<String>,
    /// Files the tool writes itself: a merge regenerates them rather than resolving them by hand.
    #[serde(default)]
    pub lockfiles: Vec<String>,
    #[serde(default)]
    pub dirs: Vec<Dir>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub options: BTreeMap<String, ToolOption>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    toolchain: Vec<Toolchain>,
}

/// A set of definitions: the built-in ones, then what a Node or workspace adds or overrides by id.
#[derive(Debug, Clone)]
pub struct Toolchains(Vec<Toolchain>);

impl Toolchains {
    pub fn builtin() -> Toolchains {
        Toolchains::parse(BUILTIN).expect("the built-in toolchains parse")
    }

    pub fn parse(text: &str) -> Result<Toolchains, String> {
        let f: File = toml::from_str(text).map_err(|e| e.to_string())?;
        Ok(Toolchains(f.toolchain))
    }

    /// `more` over these: an entry with the same id replaces this one, a new id is added.
    pub fn with(mut self, more: Toolchains) -> Toolchains {
        for t in more.0 {
            match self.0.iter_mut().find(|x| x.id == t.id) {
                Some(x) => *x = t,
                None => self.0.push(t),
            }
        }
        self
    }

    pub fn all(&self) -> &[Toolchain] {
        &self.0
    }

    pub fn get(&self, id: &str) -> Option<&Toolchain> {
        self.0.iter().find(|t| t.id == id)
    }

    /// The toolchains a project in `root` uses: a marker at its root or one level down (as the
    /// lane's lockfiles are looked for), in definition order.
    pub fn detect(&self, root: &Path) -> Vec<&Toolchain> {
        let mut names: Vec<String> = Vec::new();
        let mut look = |dir: &Path| {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    names.push(e.file_name().to_string_lossy().into_owned());
                }
            }
        };
        look(root);
        if let Ok(rd) = std::fs::read_dir(root) {
            for e in rd.flatten() {
                let n = e.file_name();
                let n = n.to_string_lossy();
                // one level down, not into what is only build output or dependencies
                if e.file_type().is_ok_and(|t| t.is_dir())
                    && !n.starts_with('.')
                    && !self.in_tree_names().contains(&n.as_ref())
                {
                    look(&e.path());
                }
            }
        }
        self.0
            .iter()
            .filter(|t| {
                t.detect
                    .iter()
                    .any(|pat| names.iter().any(|n| matches(pat, n)))
            })
            .collect()
    }

    /// The names of the directories that stay in a project's tree (every toolchain's).
    pub fn in_tree_names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .0
            .iter()
            .flat_map(|t| t.dirs.iter().filter_map(|d| d.in_tree.as_deref()))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Every toolchain's lockfiles (file names).
    pub fn lockfile_names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .0
            .iter()
            .flat_map(|t| t.lockfiles.iter().map(String::as_str))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The names of the directories that are only caches and may be given back whole.
    pub fn disposable_names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .0
            .iter()
            .flat_map(|t| {
                t.dirs
                    .iter()
                    .filter(|d| d.disposable)
                    .filter_map(|d| d.in_tree.as_deref())
            })
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

/// `name` matches `pat`, where `*` is any run of characters.
fn matches(pat: &str, name: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || !name.ends_with(last) || name.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// Where one run's toolchain directories are.
#[derive(Debug, Clone)]
pub struct Places {
    /// The item's own build directory (`<work>/.steve/build`).
    pub item: PathBuf,
    /// The person's own caches.
    pub person: PathBuf,
}

impl Toolchain {
    /// What a run sets for this toolchain: its fixed settings, the chosen options' variables
    /// (`options`: a project's choice by name, else the default), and the variables that move its
    /// directories into `at`.
    pub fn env(&self, at: &Places, options: &BTreeMap<String, bool>) -> Vec<(String, String)> {
        let item = at.item.join(&self.id);
        let person = at.person.join(&self.id);
        let fill = |v: &str| {
            v.replace("{item}", &item.to_string_lossy())
                .replace("{person}", &person.to_string_lossy())
        };
        let mut out: Vec<(String, String)> =
            self.env.iter().map(|(k, v)| (k.clone(), fill(v))).collect();
        for (name, o) in &self.options {
            if options.get(name).copied().unwrap_or(o.default) {
                out.extend(o.env.iter().map(|(k, v)| (k.clone(), fill(v))));
            }
        }
        for d in &self.dirs {
            if let (Some(k), Some(v)) = (&d.env, &d.value) {
                out.push((k.clone(), fill(v)));
            }
        }
        out
    }

    /// The directories [`Self::env`] points at, to make before the run starts.
    pub fn dirs_to_make(&self, at: &Places) -> Vec<PathBuf> {
        let item = at.item.join(&self.id);
        let person = at.person.join(&self.id);
        self.dirs
            .iter()
            .filter(|d| d.env.is_some())
            .filter_map(|d| {
                let v = d.value.as_deref()?;
                // the path in it: a flag's value after `=`, or the whole value
                let p = v.rsplit_once('=').map_or(v, |(_, p)| p);
                let p = p
                    .replace("{item}", &item.to_string_lossy())
                    .replace("{person}", &person.to_string_lossy());
                Path::new(&p).is_absolute().then(|| PathBuf::from(p))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_set_parses_and_ids_are_unique() {
        let t = Toolchains::builtin();
        let mut ids: Vec<&str> = t.all().iter().map(|t| t.id.as_str()).collect();
        assert!(ids.len() >= 23, "{ids:?}");
        ids.sort();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate ids");
        for tc in t.all() {
            assert!(!tc.detect.is_empty(), "{}", tc.id);
            for d in &tc.dirs {
                assert!(
                    d.env.is_some() == d.value.is_some(),
                    "{}/{}: env and value go together",
                    tc.id,
                    d.name
                );
                assert!(
                    d.env.is_some() || d.in_tree.is_some(),
                    "{}/{}: moved or in the tree",
                    tc.id,
                    d.name
                );
                if d.scope == Scope::Person {
                    assert!(d.concurrent_safe, "{}/{}: shared, so safe", tc.id, d.name);
                }
            }
        }
    }

    /// What ST-45 gives back on a full disk stays exactly what it was.
    #[test]
    fn the_disposable_names_are_st45s() {
        assert_eq!(
            Toolchains::builtin().disposable_names(),
            [
                ".gradle",
                ".mypy_cache",
                ".next",
                ".nuxt",
                ".parcel-cache",
                ".pytest_cache",
                ".ruff_cache",
                ".svelte-kit",
                ".turbo",
                "node_modules"
            ]
        );
    }

    /// The lane's lockfiles stay exactly what they were.
    #[test]
    fn the_lockfiles_are_the_lanes() {
        assert_eq!(
            Toolchains::builtin().lockfile_names(),
            [
                "Cargo.lock",
                "go.sum",
                "package-lock.json",
                "pnpm-lock.yaml",
                "poetry.lock",
                "uv.lock",
                "yarn.lock"
            ]
        );
    }

    #[test]
    fn patterns() {
        assert!(matches("Cargo.toml", "Cargo.toml"));
        assert!(!matches("Cargo.toml", "Cargo.lock"));
        assert!(matches("bun.lock*", "bun.lockb"));
        assert!(matches("*.csproj", "app.csproj"));
        assert!(matches("requirements*.txt", "requirements-dev.txt"));
        assert!(!matches("*.csproj", "csproj"));
        assert!(matches("next.config.*", "next.config.mjs"));
    }

    #[test]
    fn detects_at_the_root_and_one_level_down_never_in_build_output() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Cargo.toml"), "").unwrap();
        std::fs::create_dir_all(d.path().join("web")).unwrap();
        std::fs::write(d.path().join("web/package-lock.json"), "").unwrap();
        std::fs::create_dir_all(d.path().join("node_modules/x")).unwrap();
        std::fs::write(d.path().join("node_modules/go.mod"), "").unwrap();
        std::fs::create_dir_all(d.path().join("a/b")).unwrap();
        std::fs::write(d.path().join("a/b/pom.xml"), "").unwrap();
        let t = Toolchains::builtin();
        let ids: Vec<&str> = t.detect(d.path()).iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["cargo", "npm"]);
    }

    #[test]
    fn a_run_gets_its_items_and_persons_places_and_the_options() {
        let t = Toolchains::builtin();
        let at = Places {
            item: "/w/ST-1/.steve/build".into(),
            person: "/n/cache/ws/u1".into(),
        };
        let cargo = t.get("cargo").unwrap();
        let env: BTreeMap<String, String> = cargo.env(&at, &BTreeMap::new()).into_iter().collect();
        assert_eq!(env["CARGO_TARGET_DIR"], "/w/ST-1/.steve/build/cargo/target");
        assert_eq!(env["CARGO_HOME"], "/n/cache/ws/u1/cargo");
        assert_eq!(env["CARGO_INCREMENTAL"], "0", "light is on by default");
        let off = BTreeMap::from([("light".to_string(), false)]);
        let env: BTreeMap<String, String> = cargo.env(&at, &off).into_iter().collect();
        assert!(!env.contains_key("CARGO_INCREMENTAL"));
        let maven = t.get("maven").unwrap();
        assert_eq!(
            maven.env(&at, &BTreeMap::new()),
            [(
                "MAVEN_ARGS".to_string(),
                "-Dmaven.repo.local=/n/cache/ws/u1/maven/repository".to_string()
            )]
        );
        assert_eq!(
            maven.dirs_to_make(&at),
            [PathBuf::from("/n/cache/ws/u1/maven/repository")]
        );
    }

    #[test]
    fn a_node_or_workspace_overrides_by_id_and_adds() {
        let more = Toolchains::parse(
            r#"
[[toolchain]]
id = "go"
detect = ["go.work"]
[[toolchain]]
id = "zig"
detect = ["build.zig"]
dirs = [{ name = "cache", scope = "person", env = "ZIG_GLOBAL_CACHE_DIR", value = "{person}", concurrent_safe = true }]
"#,
        )
        .unwrap();
        let t = Toolchains::builtin().with(more);
        assert_eq!(t.get("go").unwrap().detect, ["go.work"]);
        assert!(t.get("zig").is_some());
        assert!(Toolchains::parse("[[toolchain]]\nid = \"x\"\ndetect = []\nbogus = 1").is_err());
    }
}
