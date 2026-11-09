//! Toolchains as data (path model §6.5): which build tools a project uses, where each one's
//! caches and outputs go (one item's own, or one person's, shared by their items), and what Steve
//! sets for them. The definitions ship in `src/builtin.toml`; this code knows no tool by name, so
//! a new language is a new entry, never a code change.
//!
//! What a run gets from it is a list of variables ([`Toolchain::env`]) that move the caches out of
//! the user's home and the shared places into the item's `.steve/build/<id>` and the person's own
//! cache, plus the tool's fixed settings (telemetry off, copies instead of hard links).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const BUILTIN: &str = include_str!("builtin.toml");

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
    /// Kept in the merge queue's staging from one batch to the next (warm build output). `None`:
    /// the default, kept when it is an item's or seeded one.
    #[serde(default)]
    pub mq_keep: Option<bool>,
}

impl Dir {
    /// Whether the merge queue's staging keeps it across batches.
    pub fn mq_keep(&self) -> bool {
        self.mq_keep
            .unwrap_or(matches!(self.scope, Scope::Item | Scope::Seed))
    }
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

/// A marker match at a particular project directory. A monorepo may use the same
/// toolchain at several locations; callers must not lose those locations.
#[derive(Debug)]
pub struct DiscoveredToolchain<'a> {
    pub toolchain: &'a Toolchain,
    pub project: PathBuf,
}

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
    /// lane's lockfiles are looked for), in definition order. This compatibility
    /// API retains its original link and lossy filename behavior; use [`Self::discover`]
    /// for per-project discovery that ignores links.
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

    /// Match each project at the root and one level down, in definition order,
    /// with the root before children sorted by name. Neither links nor hidden
    /// directories nor declared in-tree output directories are entered.
    ///
    /// This is discovery, not a filesystem capability: a Node must still anchor
    /// and recheck directories before opening or changing anything in them.
    pub fn discover(&self, root: &Path) -> Vec<DiscoveredToolchain<'_>> {
        if !std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
            return Vec::new();
        }
        let mut projects = vec![root.to_path_buf()];
        if let Ok(rd) = std::fs::read_dir(root) {
            let output_names = self.in_tree_names();
            let mut children: Vec<_> = rd
                .flatten()
                .filter(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    e.file_type().is_ok_and(|t| t.is_dir())
                        && !name.starts_with('.')
                        && !output_names.iter().any(|output| {
                            relative_path(output).ok().is_some_and(|path| {
                                path.components()
                                    .find_map(|c| match c {
                                        std::path::Component::Normal(n) => Some(n),
                                        _ => None,
                                    })
                                    .is_some_and(|n| n == e.file_name())
                            })
                        })
                })
                .collect();
            children.sort_by_key(|e| e.file_name());
            projects.extend(children.into_iter().map(|e| e.path()));
        }
        let projects: Vec<_> = projects
            .into_iter()
            .map(|project| {
                let names: Vec<_> = std::fs::read_dir(&project)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_file() || t.is_dir()))
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect();
                (project, names)
            })
            .collect();
        self.0
            .iter()
            .flat_map(|toolchain| {
                projects
                    .iter()
                    .filter(move |(_, names)| {
                        toolchain
                            .detect
                            .iter()
                            .any(|pat| names.iter().any(|name| matches(pat, name)))
                    })
                    .map(move |(project, _)| DiscoveredToolchain {
                        toolchain,
                        project: project.clone(),
                    })
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

    /// The names of the in-tree directories the merge queue's staging keeps across batches.
    pub fn mq_keep_names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .0
            .iter()
            .flat_map(|t| {
                t.dirs
                    .iter()
                    .filter(|d| d.mq_keep())
                    .filter_map(|d| d.in_tree.as_deref())
            })
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

/// Why a declaration cannot yet be used as a directory. Unknown or malformed
/// declarations stay in the plan; consumers must not count them as zero bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryError {
    InvalidToolchainId,
    InvalidProject,
    InvalidPlace,
    NonUtf8Path,
    MissingPath,
    ConflictingPaths,
    EnvValueMismatch,
    InvalidRelativePath,
    InvalidTemplate,
    ScopeMismatch,
}

/// One declared directory at one project. The original declaration retains its
/// scope and disposal hints; they are not authority to access or delete it.
#[derive(Debug)]
pub struct PlannedDirectory<'a> {
    pub declaration: &'a Dir,
    pub project: PathBuf,
    pub path: Result<PathBuf, DirectoryError>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains(['/', '\\', ':'])
        && !id.chars().any(char::is_control)
        && id != "."
        && id != ".."
}

fn relative_path(path: &str) -> Result<PathBuf, DirectoryError> {
    // Check both separators even on Unix, so a definition cannot change meaning
    // when it is sent to a Node on another OS.
    if path.is_empty() || path.contains(['\\', ':', '\0', '{', '}']) {
        return Err(DirectoryError::InvalidRelativePath);
    }
    let path = Path::new(path);
    if path.is_absolute()
        || path.components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err(DirectoryError::InvalidRelativePath);
    }
    Ok(path.to_path_buf())
}

fn fill_template(value: &str, item: &str, person: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        if let Some(tail) = rest.strip_prefix("{item}") {
            out.push_str(item);
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("{person}") {
            out.push_str(person);
            rest = tail;
        } else {
            out.push('{');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

impl Toolchain {
    /// What a run sets for this toolchain: its fixed settings, the chosen options' variables
    /// (`options`: a project's choice by name, else the default), and the variables that move its
    /// directories into `at`. Placeholders are expanded once; text in a real path
    /// is never reinterpreted as a template. This compatibility interface retains
    /// lossy conversion of non-UTF-8 places: strict consumers must first call
    /// [`Self::validate_places`] and inspect [`Self::directory_plan`].
    pub fn env(&self, at: &Places, options: &BTreeMap<String, bool>) -> Vec<(String, String)> {
        let item = at.item.join(&self.id);
        let person = at.person.join(&self.id);
        let fill = |v: &str| fill_template(v, &item.to_string_lossy(), &person.to_string_lossy());
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

    /// Check the directory context, even for a toolchain with no directory
    /// declarations. Strict consumers must call this before environment injection;
    /// an empty directory plan does not imply that the context is valid.
    pub fn validate_places(&self, at: &Places) -> Result<(), DirectoryError> {
        if !valid_id(&self.id) {
            return Err(DirectoryError::InvalidToolchainId);
        }
        for place in [&at.item, &at.person] {
            if place.to_str().is_none() {
                return Err(DirectoryError::NonUtf8Path);
            }
            if !place.is_absolute()
                || place
                    .components()
                    .any(|c| c == std::path::Component::ParentDir)
            {
                return Err(DirectoryError::InvalidPlace);
            }
        }
        Ok(())
    }

    /// The resolvable directories [`Self::env`] points at, to make before the run
    /// starts. This compatibility helper omits errors; new consumers must inspect
    /// [`Self::directory_plan`] before injecting environment or accounting bytes.
    pub fn dirs_to_make(&self, at: &Places) -> Vec<PathBuf> {
        self.directory_plan(Path::new(""), at)
            .into_iter()
            .filter(|d| d.declaration.env.is_some())
            .filter_map(|d| d.path.ok())
            .collect()
    }

    /// Resolve declarations without filesystem access. Paths under the project
    /// are relative; moved paths must start with the placeholder for their scope.
    /// No flag syntax is inferred from `value`: `=` can be part of a directory.
    ///
    /// The result is input to a Node's safe traversal, not OS authorization.
    pub fn directory_plan(&self, project: &Path, at: &Places) -> Vec<PlannedDirectory<'_>> {
        self.dirs
            .iter()
            .map(|declaration| PlannedDirectory {
                declaration,
                project: project.to_path_buf(),
                path: self.resolve_directory(declaration, project, at),
            })
            .collect()
    }

    fn resolve_directory(
        &self,
        dir: &Dir,
        project: &Path,
        at: &Places,
    ) -> Result<PathBuf, DirectoryError> {
        self.validate_places(at)?;
        if dir.env.is_some() != dir.value.is_some() {
            return Err(DirectoryError::EnvValueMismatch);
        }
        match (&dir.in_tree, &dir.value) {
            (Some(_), Some(_)) => Err(DirectoryError::ConflictingPaths),
            (None, None) => Err(DirectoryError::MissingPath),
            (Some(path), None) => {
                if dir.scope == Scope::Person {
                    return Err(DirectoryError::ScopeMismatch);
                }
                if project.to_str().is_none() {
                    return Err(DirectoryError::NonUtf8Path);
                }
                if !project.is_absolute()
                    || project
                        .components()
                        .any(|c| c == std::path::Component::ParentDir)
                {
                    return Err(DirectoryError::InvalidProject);
                }
                Ok(project.join(relative_path(path)?))
            }
            (None, Some(value)) => {
                let (placeholder, base) = match dir.scope {
                    Scope::Item | Scope::Seed => ("{item}", &at.item),
                    Scope::Person => ("{person}", &at.person),
                };
                let suffix = match value.strip_prefix(placeholder) {
                    Some(suffix) => suffix,
                    None if value.starts_with("{item}") || value.starts_with("{person}") => {
                        return Err(DirectoryError::ScopeMismatch);
                    }
                    None => return Err(DirectoryError::InvalidTemplate),
                };
                let root = base.join(&self.id);
                if suffix.is_empty() {
                    return Ok(root);
                }
                let suffix = suffix
                    .strip_prefix('/')
                    .ok_or(DirectoryError::InvalidTemplate)?;
                Ok(root.join(relative_path(suffix)?))
            }
        }
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
        assert!(!env.contains_key("CARGO_HOME"), "never a tool's whole home");
        assert_eq!(env["CARGO_INCREMENTAL"], "0", "light is on by default");
        let off = BTreeMap::from([("light".to_string(), false)]);
        let env: BTreeMap<String, String> = cargo.env(&at, &off).into_iter().collect();
        assert!(!env.contains_key("CARGO_INCREMENTAL"));
        let go = t.get("go").unwrap();
        let env: BTreeMap<String, String> = go.env(&at, &BTreeMap::new()).into_iter().collect();
        assert_eq!(env["GOCACHE"], "/n/cache/ws/u1/go/build");
        assert!(!env.contains_key("GOPATH") && !env.contains_key("GOFLAGS"));
        assert_eq!(
            go.dirs_to_make(&at),
            [
                PathBuf::from("/n/cache/ws/u1/go/build"),
                PathBuf::from("/n/cache/ws/u1/go/mod")
            ]
        );
    }

    /// What may never be moved or set: a tool's home with its config and credentials, what the
    /// project or its user sets themselves.
    #[test]
    fn no_tool_home_and_no_users_variable_is_taken_over() {
        let t = Toolchains::builtin();
        let taken: Vec<String> = t
            .all()
            .iter()
            .flat_map(|tc| {
                tc.env
                    .keys()
                    .cloned()
                    .chain(tc.dirs.iter().filter_map(|d| d.env.clone()))
            })
            .collect();
        for home in [
            "CARGO_HOME",
            "GRADLE_USER_HOME",
            "GOPATH",
            "GOFLAGS",
            "MAVEN_ARGS",
            "MIX_HOME",
            "HEX_HOME",
            "PUB_CACHE",
            "CP_HOME_DIR",
            "CCACHE_DIR",
            "YARN_CACHE_FOLDER",
            "npm_config_store_dir",
            "UV_PROJECT_ENVIRONMENT",
            "BUNDLE_PATH",
            "NUGET_PACKAGES",
        ] {
            assert!(!taken.iter().any(|k| k == home), "{home}");
        }
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
