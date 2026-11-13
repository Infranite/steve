//! A plugin library's address and its index (`steve-store.json`): what the Hub reads to know which
//! plugins a library offers. The format is described in [`super`]; this module parses and checks it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// The index file's name at the root of a library.
pub const INDEX_FILE: &str = "steve-store.json";
/// The index format this Hub reads.
pub const STORE_FORMAT: u32 = 1;
/// The run contract of a channel plugin this Hub speaks ([`super`]).
pub const CHANNEL_CONTRACT: u32 = 1;
pub const KIND_CHANNEL: &str = "channel";
/// A skin for the web client: the archive carries a theme manifest, nothing runs ([`super`]).
pub const KIND_THEME: &str = "theme";
/// The theme contract this Hub speaks: `entry` is a theme manifest in the web client's format.
pub const THEME_CONTRACT: u32 = 1;
/// The artifact key of an archive that is the same on every machine (a theme is data).
pub const ANY_TARGET: &str = "any";

pub const MAX_INDEX_BYTES: usize = 1024 * 1024;
const MAX_PLUGINS: usize = 200;
const MAX_FIELDS: usize = 20;

/// Where a library is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// A repository on GitHub: the index is the file at the root of its default branch.
    GitHub { owner: String, repo: String },
    /// The index file itself, over HTTPS (plain HTTP only on this machine).
    Url(String),
    /// A directory on the Hub's machine with the index file in it.
    Dir(PathBuf),
}

fn loopback_http(url: &reqwest::Url) -> bool {
    url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !s.starts_with('.')
}

impl Address {
    /// What a person typed: `https://github.com/<owner>/<repo>`, `<owner>/<repo>`, the HTTPS
    /// address of an index file, or an absolute directory.
    pub fn parse(input: &str) -> Result<Address> {
        let s = input.trim();
        if s.is_empty() || s.len() > 500 || s.chars().any(char::is_control) {
            bail!("not a library address")
        }
        if s.starts_with('/') {
            return Ok(Address::Dir(PathBuf::from(s.trim_end_matches('/'))));
        }
        if !s.contains("://") {
            if let Some((owner, repo)) = s.split_once('/')
                && name_ok(owner)
                && name_ok(repo)
            {
                return Ok(Address::GitHub {
                    owner: owner.into(),
                    repo: repo.into(),
                });
            }
            bail!("not a library address")
        }
        let url = reqwest::Url::parse(s).context("not a library address")?;
        if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
            bail!("a library address carries no credential and no query")
        }
        if url.scheme() != "https" && !loopback_http(&url) {
            bail!("a library address is https://")
        }
        if url.host_str() == Some("github.com") {
            let parts: Vec<&str> = url.path().trim_matches('/').split('/').collect();
            if let [owner, repo] = parts[..] {
                let repo = repo.strip_suffix(".git").unwrap_or(repo);
                if name_ok(owner) && name_ok(repo) {
                    return Ok(Address::GitHub {
                        owner: owner.into(),
                        repo: repo.into(),
                    });
                }
            }
            bail!("a GitHub library address is https://github.com/<owner>/<repo>")
        }
        if !url.path().ends_with(".json") {
            bail!("the address of a library that is not on GitHub is its index file (….json)")
        }
        Ok(Address::Url(url.to_string()))
    }

    /// As it is stored and shown.
    pub fn display(&self) -> String {
        match self {
            Address::GitHub { owner, repo } => format!("https://github.com/{owner}/{repo}"),
            Address::Url(u) => u.clone(),
            Address::Dir(d) => d.display().to_string(),
        }
    }

    /// Where the index is read from (a URL, or a file for [`Address::Dir`]).
    pub fn index_location(&self) -> Location {
        match self {
            Address::GitHub { owner, repo } => Location::Url(format!(
                "https://raw.githubusercontent.com/{owner}/{repo}/HEAD/{INDEX_FILE}"
            )),
            Address::Url(u) => Location::Url(u.clone()),
            Address::Dir(d) => Location::File(d.join(INDEX_FILE)),
        }
    }

    /// A short id for the library on this Hub, from its address: lowercase letters, digits, `-`.
    pub fn slug(&self) -> String {
        let raw = match self {
            Address::GitHub { owner, repo } => format!("{owner}-{repo}"),
            Address::Url(u) => reqwest::Url::parse(u)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_else(|| "library".into()),
            Address::Dir(d) => d
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "local".into()),
        };
        let mut out = String::new();
        for c in raw.chars().flat_map(char::to_lowercase) {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                out.push(c)
            } else if !out.ends_with('-') {
                out.push('-')
            }
        }
        let out = out.trim_matches('-');
        let out: String = out.chars().take(40).collect();
        if out.is_empty() {
            "library".into()
        } else {
            out
        }
    }

    /// Where an artifact named in the index is: an absolute `https://` address, or a path relative
    /// to the index (the only form a directory library has, and it stays inside the directory).
    pub fn artifact_location(&self, reference: &str) -> Result<Location> {
        if reference.contains("://") {
            let url = reqwest::Url::parse(reference).context("artifact address")?;
            if url.scheme() != "https" && !loopback_http(&url) {
                bail!("an artifact address is https://")
            }
            if matches!(self, Address::Dir(_)) {
                // a directory library is self-contained: what it installs is what is in it
                bail!("a directory library names its artifacts by relative path")
            }
            return Ok(Location::Url(url.to_string()));
        }
        let rel = Path::new(reference);
        if reference.is_empty()
            || rel.is_absolute()
            || rel
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            bail!("artifact path {reference:?} leaves the library")
        }
        match self.index_location() {
            Location::File(index) => Ok(Location::File(
                index.parent().context("index has no directory")?.join(rel),
            )),
            Location::Url(index) => Ok(Location::Url(
                reqwest::Url::parse(&index)?.join(reference)?.to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Url(String),
    File(PathBuf),
}

/// Text a library gives in one language, or per language (`{"en": "…", "zh-CN": "…"}`); the client
/// picks the reader's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Text {
    One(String),
    ByLanguage(BTreeMap<String, String>),
}

impl Text {
    fn ok(&self, max: usize) -> bool {
        let one = |s: &String| !s.trim().is_empty() && s.len() <= max;
        match self {
            Text::One(s) => one(s),
            Text::ByLanguage(m) => !m.is_empty() && m.len() <= 20 && m.values().all(one),
        }
    }

    /// English, else whatever there is: for logs and for names the Hub itself stores.
    pub fn plain(&self) -> &str {
        match self {
            Text::One(s) => s,
            Text::ByLanguage(m) => m
                .get("en")
                .or_else(|| m.values().next())
                .map(String::as_str)
                .unwrap_or_default(),
        }
    }
}

/// One setting a person fills in when connecting a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub key: String,
    pub label: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<Text>,
    /// Never shown again once entered.
    #[serde(default)]
    pub secret: bool,
    #[serde(default = "yes")]
    pub required: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// `https://…`, or a path relative to the index.
    pub url: String,
    /// Of the archive, 64 hex digits.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plugin {
    pub id: String,
    pub name: Text,
    pub summary: Text,
    pub kind: String,
    pub version: String,
    /// The version of its kind's run contract it was built for.
    pub contract: u32,
    /// The executable, relative to the root of the unpacked archive.
    pub entry: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default)]
    pub config: Vec<Field>,
    /// By the target the Hub was built for (`aarch64-apple-darwin`, …).
    #[serde(default)]
    pub artifacts: BTreeMap<String, Artifact>,
}

impl Plugin {
    /// Why this Hub cannot run it, when it cannot: a kind or a contract from a newer Steve.
    pub fn unsupported(&self) -> Option<&'static str> {
        let contract = match self.kind.as_str() {
            KIND_CHANNEL => CHANNEL_CONTRACT,
            KIND_THEME => THEME_CONTRACT,
            _ => return Some("kind"),
        };
        (self.contract != contract).then_some("contract")
    }

    /// Whether it is a process the Hub runs (a channel), not data the web client reads (a theme).
    pub fn runs(&self) -> bool {
        self.kind == KIND_CHANNEL
    }

    /// The archive for a Hub built for `target`: its own, else one for [`ANY_TARGET`].
    pub fn artifact(&self, target: &str) -> Option<&Artifact> {
        self.artifacts
            .get(target)
            .or_else(|| self.artifacts.get(ANY_TARGET))
    }

    pub fn semver(&self) -> semver::Version {
        // checked when the index was read
        semver::Version::parse(&self.version).unwrap_or_else(|_| semver::Version::new(0, 0, 0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub store: u32,
    pub name: Text,
    #[serde(default)]
    pub plugins: Vec<Plugin>,
}

/// A plugin, setting or library id: a lowercase letter or digit, then up to 39 of those and `-`
/// or `_`. It is used as a directory name.
pub fn id_ok(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
}

fn entry_ok(s: &str) -> bool {
    let p = Path::new(s);
    !s.is_empty()
        && s.len() <= 200
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Reads an index, refusing one this Hub cannot be sure it understands. A plugin of an unknown
/// kind is kept (and listed as needing a newer Steve); a malformed one makes the whole index
/// wrong, because a library is published by one party and half of it is not a state to install
/// from.
pub fn parse(bytes: &[u8]) -> Result<Index> {
    if bytes.len() > MAX_INDEX_BYTES {
        bail!("the index is larger than 1 MiB")
    }
    let head: serde_json::Value = serde_json::from_slice(bytes).context("the index is not JSON")?;
    match head.get("store").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(STORE_FORMAT) => {}
        Some(v) => bail!("the index is format {v}; this Steve reads format {STORE_FORMAT}"),
        None => bail!("the index does not say its format (\"store\")"),
    }
    let index: Index = serde_json::from_value(head).context("the index")?;
    if !index.name.ok(100) {
        bail!("the library has no name")
    }
    if index.plugins.len() > MAX_PLUGINS {
        bail!("the index lists more than {MAX_PLUGINS} plugins")
    }
    let mut seen = std::collections::BTreeSet::new();
    for p in &index.plugins {
        let at = |what: &str| format!("plugin {:?}: {what}", p.id);
        if !id_ok(&p.id) {
            bail!(at("its id is lowercase letters, digits, '-' and '_'"))
        }
        if !seen.insert(p.id.as_str()) {
            bail!(at("listed twice"))
        }
        if !p.name.ok(100) || !p.summary.ok(500) {
            bail!(at("needs a name and a summary"))
        }
        if p.kind.is_empty() || p.kind.len() > 40 {
            bail!(at("needs a kind"))
        }
        if semver::Version::parse(&p.version).is_err() {
            bail!(at("its version is not a semantic version"))
        }
        if !entry_ok(&p.entry) {
            bail!(at("its entry is a path inside the archive"))
        }
        if let Some(h) = &p.homepage
            && !(h.starts_with("https://") && h.len() <= 500)
        {
            bail!(at("its homepage is https://"))
        }
        if p.config.len() > MAX_FIELDS {
            bail!(at("has too many settings"))
        }
        let mut keys = std::collections::BTreeSet::new();
        for f in &p.config {
            if !id_ok(&f.key) || !keys.insert(f.key.as_str()) {
                bail!(at(&format!("setting {:?}", f.key)))
            }
            if !f.label.ok(100) || f.hint.as_ref().is_some_and(|h| !h.ok(300)) {
                bail!(at(&format!("setting {:?} needs a label", f.key)))
            }
        }
        for (target, a) in &p.artifacts {
            if target.is_empty() || target.len() > 80 {
                bail!(at("an artifact without a target"))
            }
            if a.sha256.len() != 64 || !a.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!(at(&format!("the artifact for {target} needs a sha256")))
            }
            if a.url.is_empty() || a.url.len() > 1000 {
                bail!(at(&format!("the artifact for {target} needs an address")))
            }
        }
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> serde_json::Value {
        serde_json::json!({
            "store": 1,
            "name": "Examples",
            "plugins": [{
                "id": "echo",
                "name": {"en": "Echo", "zh-CN": "回声"},
                "summary": "Says it back",
                "kind": "channel",
                "version": "0.1.0",
                "contract": 1,
                "entry": "bin/echo",
                "config": [{"key": "address", "label": "Address"}, {"key": "token", "label": "Token", "secret": true}],
                "artifacts": {"aarch64-apple-darwin": {"url": "echo.tar.gz", "sha256": "0".repeat(64)}}
            }]
        })
    }

    fn parsed(v: &serde_json::Value) -> Result<Index> {
        parse(&serde_json::to_vec(v).unwrap())
    }

    #[test]
    fn an_address_is_a_github_repository_an_index_file_or_a_directory() {
        let gh = Address::GitHub {
            owner: "Infranite".into(),
            repo: "steve-extensions".into(),
        };
        for s in [
            "https://github.com/Infranite/steve-extensions",
            "https://github.com/Infranite/steve-extensions/",
            "https://github.com/Infranite/steve-extensions.git",
            " Infranite/steve-extensions ",
        ] {
            assert_eq!(Address::parse(s).unwrap(), gh, "{s}");
        }
        assert_eq!(
            gh.index_location(),
            Location::Url(
                "https://raw.githubusercontent.com/Infranite/steve-extensions/HEAD/steve-store.json".into()
            )
        );
        assert_eq!(gh.slug(), "infranite-steve-extensions");
        assert_eq!(
            Address::parse("https://plugins.example/steve-store.json").unwrap(),
            Address::Url("https://plugins.example/steve-store.json".into())
        );
        assert_eq!(
            Address::parse("/srv/plugins/").unwrap(),
            Address::Dir("/srv/plugins".into())
        );
        for bad in [
            "",
            "http://plugins.example/steve-store.json",
            "https://user:pw@plugins.example/steve-store.json",
            "https://plugins.example/steve-store.json?token=1",
            "https://plugins.example/",
            "https://github.com/Infranite",
            "relative/dir/deeper",
            "ftp://plugins.example/steve-store.json",
        ] {
            assert!(Address::parse(bad).is_err(), "{bad}");
        }
        assert!(Address::parse("http://127.0.0.1:8000/steve-store.json").is_ok());
    }

    #[test]
    fn an_artifact_is_beside_the_index_or_at_an_https_address() {
        let dir = Address::Dir("/srv/plugins".into());
        assert_eq!(
            dir.artifact_location("dist/echo.tar.gz").unwrap(),
            Location::File("/srv/plugins/dist/echo.tar.gz".into())
        );
        for bad in [
            "../echo.tar.gz",
            "/etc/passwd",
            "",
            "https://plugins.example/echo.tar.gz",
        ] {
            assert!(dir.artifact_location(bad).is_err(), "{bad}");
        }
        let url = Address::Url("https://plugins.example/lib/steve-store.json".into());
        assert_eq!(
            url.artifact_location("echo.tar.gz").unwrap(),
            Location::Url("https://plugins.example/lib/echo.tar.gz".into())
        );
        assert_eq!(
            url.artifact_location("https://cdn.example/echo.tar.gz")
                .unwrap(),
            Location::Url("https://cdn.example/echo.tar.gz".into())
        );
        assert!(
            url.artifact_location("http://cdn.example/echo.tar.gz")
                .is_err()
        );
    }

    #[test]
    fn an_index_is_read_whole_or_refused() {
        let index = parsed(&sample()).unwrap();
        let p = &index.plugins[0];
        assert_eq!(p.name.plain(), "Echo");
        assert_eq!(p.unsupported(), None);
        assert!(p.config[0].required && !p.config[0].secret && p.config[1].secret);

        let mut newer = sample();
        newer["store"] = 2.into();
        assert!(parsed(&newer).unwrap_err().to_string().contains("format 2"));

        let cases: [(&str, serde_json::Value); 6] = [
            ("id", "Echo".into()),
            ("version", "one".into()),
            ("entry", "../echo".into()),
            ("entry", "/bin/echo".into()),
            ("homepage", "http://example.org".into()),
            (
                "artifacts",
                serde_json::json!({"x": {"url": "a", "sha256": "abc"}}),
            ),
        ];
        for (key, value) in cases {
            let mut v = sample();
            v["plugins"][0][key] = value;
            assert!(parsed(&v).is_err(), "{key}");
        }
        let mut twice = sample();
        let again = twice["plugins"][0].clone();
        twice["plugins"].as_array_mut().unwrap().push(again);
        assert!(parsed(&twice).is_err());
    }

    #[test]
    fn a_theme_is_supported_and_its_archive_may_be_for_any_machine() {
        let mut v = sample();
        v["plugins"][0]["kind"] = "theme".into();
        v["plugins"][0]["entry"] = "theme.json".into();
        v["plugins"][0]["artifacts"] =
            serde_json::json!({"any": {"url": "ocean.tar.gz", "sha256": "1".repeat(64)}});
        let p = &parsed(&v).unwrap().plugins[0];
        assert_eq!(p.unsupported(), None);
        assert!(!p.runs());
        assert_eq!(
            p.artifact("x86_64-unknown-linux-gnu").unwrap().url,
            "ocean.tar.gz"
        );
        v["plugins"][0]["contract"] = 2.into();
        assert_eq!(
            parsed(&v).unwrap().plugins[0].unsupported(),
            Some("contract")
        );
    }

    #[test]
    fn a_kind_from_a_newer_steve_is_listed_but_not_runnable() {
        let mut v = sample();
        v["plugins"][0]["kind"] = "tool".into();
        v["plugins"][0]["something_new"] = true.into();
        assert_eq!(parsed(&v).unwrap().plugins[0].unsupported(), Some("kind"));
        let mut v = sample();
        v["plugins"][0]["contract"] = 2.into();
        assert_eq!(
            parsed(&v).unwrap().plugins[0].unsupported(),
            Some("contract")
        );
    }
}
