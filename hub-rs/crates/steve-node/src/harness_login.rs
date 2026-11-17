//! The harness's sign-in on this Node in a session's harness directory ([`steve_acp::Login`]).
//!
//! The harness signs in by itself on the Node: its own login command, its own settings, set up by
//! whoever installed it on this machine. Steve never configures a provider and never handles a
//! provider key. Every principal's session here, the owner's and anyone else's (a schedule's
//! service principal too), uses that one sign-in:
//!
//! - **Credential files** (Codex's `auth.json`): a link in the session's directory to the Node's
//!   own file, never a copy. The harness reads the Node's login and, renewing it, replaces the
//!   file the link leads to, so there is one login and nothing that could rotate apart from it and
//!   sign it out. The link is made even before the Node signs in, so a later sign-in is seen. A
//!   file the harness or a person put there is left alone (a sign-in of that directory's own).
//! - **A credential store variable** (Claude Code's `CLAUDE_SECURESTORAGE_CONFIG_DIR`): set in the
//!   harness's environment by the caller ([`steve_acp::StateDir::node_store`]); nothing is written.
//! - **Sign-in entries of the settings file** (which provider or gateway, a key helper): in every
//!   session's file these entries are the Node's own as it has them now; everything else in that
//!   file stays the session's. A file the harness has never had is made only when the Node's has
//!   such entries.
//!
//! The Node keeps the list of the links it made (`.steve-login.json` beside them), so only those
//! are ever replaced or removed, and never through a link.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use steve_acp::Login;

use crate::attachments::{private_dir, publish, publish_private};

/// The list of what Steve made, beside it.
const MANIFEST: &str = ".steve-login.json";
/// The list of the provider files 0.2.114's model access wrote for service sessions, beside them.
const LEGACY_MODEL_ACCESS: &str = ".steve-model-access.json";
/// The largest settings file read.
const MAX_SETTINGS: u64 = 4 * 1024 * 1024;

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Made {
    #[serde(default)]
    links: Vec<String>,
}

/// Puts the Node's sign-in of `login` into `target`, the session's harness directory. `node` is the
/// harness's own directory on this Node. Run after the owner's settings were imported (their file
/// is then the one the entries go into). Blocking. Returns what was left alone and why (for the
/// log); an `Err` is a declaration that is not plain or a step that could not be made.
pub(crate) fn place(node: &Path, target: &Path, login: &Login) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    forget_model_access(target)?;
    link_files(node, target, login, &mut notes)?;
    if !login.settings.is_empty() && !login.entries.is_empty() {
        settings(node, target, login, &mut notes)?;
    }
    Ok(notes)
}

/// The credential files, linked where the harness reads them; with a store variable the harness
/// uses them where they are, and links made before go.
fn link_files(
    node: &Path,
    target: &Path,
    login: &Login,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let before = made(target);
    let mut now = Made::default();
    let links: Vec<&String> = if login.store_env.is_empty() {
        login.files.iter().collect()
    } else {
        vec![]
    };
    for rel in &links {
        if !Login::plain(rel) {
            return Err(format!("{rel}: not a plain relative path"));
        }
    }
    for rel in before.links.iter().filter(|r| !links.contains(r)) {
        remove_link(target, rel)?;
    }
    for rel in links {
        let src = node.join(rel);
        let dst = step_dirs(target, rel)?;
        match fs::symlink_metadata(&dst) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", dst.display())),
            Ok(m) if m.file_type().is_symlink() => {
                if fs::read_link(&dst).is_ok_and(|to| to == src) {
                    now.links.push(rel.clone());
                    continue;
                }
                // a link elsewhere is replaced, never followed
                fs::remove_file(&dst).map_err(|e| format!("{}: {e}", dst.display()))?;
            }
            Ok(_) => {
                notes.push(format!(
                    "{rel}: a sign-in of this directory's own is there, left alone"
                ));
                continue;
            }
        }
        std::os::unix::fs::symlink(&src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        now.links.push(rel.clone());
    }
    if now != before {
        if now == Made::default() {
            let _ = fs::remove_file(target.join(MANIFEST));
        } else {
            let list = serde_json::to_vec(&now).map_err(|e| e.to_string())?;
            publish(&target.join(MANIFEST), &list)?;
        }
    }
    Ok(())
}

/// The sign-in entries of the session's settings file made the Node's own; the rest of it stays.
fn settings(
    node: &Path,
    target: &Path,
    login: &Login,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let rel = login.settings.as_str();
    if !Login::plain(rel) {
        return Err(format!("{rel}: not a plain relative path"));
    }
    let format = Format::of(rel).ok_or_else(|| format!("{rel}: not a .toml or .json file"))?;
    let theirs = match read_node_settings(&node.join(rel)) {
        None => Doc::empty(format),
        Some(raw) => match Doc::parse(format, &raw) {
            Ok(d) => d,
            Err(why) => {
                // the Node's harness cannot read it either: nothing is changed here
                notes.push(format!("{rel}: the Node's settings were not read ({why})"));
                return Ok(());
            }
        },
    };
    let dst = step_dirs(target, rel)?;
    let (ours, existed) = match fs::symlink_metadata(&dst) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Doc::empty(format), false),
        Err(e) => return Err(format!("{}: {e}", dst.display())),
        Ok(m) if m.is_dir() => return Err(format!("{} is a directory", dst.display())),
        Ok(m) if !m.is_file() || m.len() > MAX_SETTINGS => {
            notes.push(format!("{rel}: not a file of its own here, left alone"));
            return Ok(());
        }
        Ok(_) => {
            let raw = fs::read(&dst).map_err(|e| format!("{}: {e}", dst.display()))?;
            match Doc::parse(format, &raw) {
                Ok(d) => (d, true),
                Err(why) => {
                    notes.push(format!("{rel}: not readable here ({why}), left alone"));
                    return Ok(());
                }
            }
        }
    };
    let merged = ours.with_entries_of(&theirs, &login.entries);
    if merged == ours || (!existed && merged.is_empty()) {
        return Ok(());
    }
    // one rename: never written through whatever is at the path
    publish_private(&dst, &merged.render()?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Toml,
    Json,
}

impl Format {
    fn of(rel: &str) -> Option<Format> {
        if rel.ends_with(".toml") {
            Some(Format::Toml)
        } else if rel.ends_with(".json") {
            Some(Format::Json)
        } else {
            None
        }
    }
}

/// A settings file's top-level table.
#[derive(Debug, Clone, PartialEq)]
enum Doc {
    Toml(toml::Table),
    Json(serde_json::Map<String, Value>),
}

/// Whether the name `name` is matched by `pat` (a trailing `*` matches any rest).
fn matches(pat: &str, name: &str) -> bool {
    match pat.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pat == name,
    }
}

/// `entries` as `(name, entry of its table)` pairs, the usable ones only.
fn split(entries: &[String]) -> Vec<(&str, Option<&str>)> {
    entries
        .iter()
        .filter(|e| Login::entry_ok(e))
        .map(|e| match e.split_once('.') {
            Some((h, s)) => (h, Some(s)),
            None => (e.as_str(), None),
        })
        .collect()
}

/// [`Doc::with_entries_of`] for one table type: its table variant and how an entry
/// is removed (keeping the order of the rest).
macro_rules! merge_tables {
    ($name:ident, $map:ty, $table:path, $remove:ident) => {
        fn $name(ours: &$map, theirs: &$map, entries: &[(&str, Option<&str>)]) -> $map {
            let mut out = ours.clone();
            // what the entries name goes, wherever it came from
            for (head, sub) in entries {
                let names: Vec<String> = out.keys().filter(|k| matches(head, k)).cloned().collect();
                for k in names {
                    match sub {
                        None => {
                            out.$remove(&k);
                        }
                        Some(sub) => {
                            let emptied = match out.get_mut(&k) {
                                Some($table(t)) if !t.is_empty() => {
                                    let subs: Vec<String> =
                                        t.keys().filter(|k2| matches(sub, k2)).cloned().collect();
                                    for k2 in subs {
                                        t.$remove(&k2);
                                    }
                                    t.is_empty()
                                }
                                _ => false,
                            };
                            if emptied {
                                out.$remove(&k);
                            }
                        }
                    }
                }
            }
            // and the Node's own takes its place
            for (head, sub) in entries {
                for (k, v) in theirs.iter().filter(|(k, _)| matches(head, k)) {
                    match (sub, v) {
                        (None, _) => {
                            out.insert(k.clone(), v.clone());
                        }
                        (Some(sub), $table(t)) => {
                            for (k2, v2) in t.iter().filter(|(k2, _)| matches(sub, k2)) {
                                if !matches!(out.get(k), Some($table(_))) {
                                    out.insert(k.clone(), $table(Default::default()));
                                }
                                if let Some($table(o)) = out.get_mut(k) {
                                    o.insert(k2.clone(), v2.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            out
        }
    };
}

merge_tables!(merge_toml, toml::Table, toml::Value::Table, remove);
merge_tables!(
    merge_json,
    serde_json::Map<String, Value>,
    Value::Object,
    shift_remove
);

impl Doc {
    fn empty(f: Format) -> Doc {
        match f {
            Format::Toml => Doc::Toml(toml::Table::new()),
            Format::Json => Doc::Json(serde_json::Map::new()),
        }
    }

    fn parse(f: Format, raw: &[u8]) -> Result<Doc, String> {
        let text = std::str::from_utf8(raw).map_err(|_| "not UTF-8".to_string())?;
        match f {
            Format::Toml => toml::from_str::<toml::Table>(text)
                .map(Doc::Toml)
                .map_err(|e| e.message().to_string()),
            Format::Json if text.trim().is_empty() => Ok(Doc::empty(f)),
            Format::Json => match serde_json::from_str::<Value>(text) {
                Ok(Value::Object(m)) => Ok(Doc::Json(m)),
                Ok(_) => Err("not an object".into()),
                Err(e) => Err(e.to_string()),
            },
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Doc::Toml(t) => t.is_empty(),
            Doc::Json(m) => m.is_empty(),
        }
    }

    /// This table with `entries` made `theirs`: what they name here goes, what they name there
    /// comes in its place, everything else stays. Deterministic.
    fn with_entries_of(&self, theirs: &Doc, entries: &[String]) -> Doc {
        let entries = split(entries);
        match (self, theirs) {
            (Doc::Toml(a), Doc::Toml(b)) => Doc::Toml(merge_toml(a, b, &entries)),
            (Doc::Json(a), Doc::Json(b)) => Doc::Json(merge_json(a, b, &entries)),
            _ => self.clone(),
        }
    }

    fn render(&self) -> Result<Vec<u8>, String> {
        match self {
            Doc::Toml(t) => toml::to_string(t)
                .map(String::into_bytes)
                .map_err(|e| e.to_string()),
            Doc::Json(m) => {
                let mut b = serde_json::to_vec_pretty(m).map_err(|e| e.to_string())?;
                b.push(b'\n');
                Ok(b)
            }
        }
    }
}

/// The entries of the settings file `rel` (its content `raw`) that `entries` name, in the same
/// format; `None` when it has none of them. Deterministic: the same file gives the same bytes.
#[cfg(test)]
fn pick(rel: &str, raw: &[u8], entries: &[String]) -> Result<Option<Vec<u8>>, String> {
    let f = Format::of(rel).ok_or("not a .toml or .json file")?;
    let picked = Doc::empty(f).with_entries_of(&Doc::parse(f, raw)?, entries);
    if picked.is_empty() {
        return Ok(None);
    }
    picked.render().map(Some)
}

/// The Node's own settings file, when it is one (a link to one is followed: it is the Node's).
fn read_node_settings(p: &Path) -> Option<Vec<u8>> {
    let m = fs::metadata(p).ok()?;
    if !m.is_file() || m.len() > MAX_SETTINGS {
        return None;
    }
    fs::read(p).ok()
}

/// What Steve made in `target` before.
fn made(target: &Path) -> Made {
    let p = target.join(MANIFEST);
    if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
        return Made::default();
    }
    fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Takes away the provider file 0.2.114's model access wrote into a service session's directory
/// (it named a provider whose key only that version set), and its list.
fn forget_model_access(target: &Path) -> Result<(), String> {
    let list = target.join(LEGACY_MODEL_ACCESS);
    if !fs::symlink_metadata(&list).is_ok_and(|m| m.file_type().is_file()) {
        return Ok(());
    }
    let files: Vec<String> = fs::read(&list)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    for rel in files {
        remove_file(target, &rel)?;
    }
    fs::remove_file(&list).map_err(|e| format!("{}: {e}", list.display()))
}

/// `target/rel`, every step before it a real directory made here, never one planted as a link.
fn step_dirs(target: &Path, rel: &str) -> Result<std::path::PathBuf, String> {
    let segs: Vec<&str> = rel.split('/').collect();
    let mut dst = target.to_path_buf();
    for seg in &segs[..segs.len() - 1] {
        dst.push(seg);
        private_dir(&dst)?;
    }
    dst.push(segs[segs.len() - 1]);
    Ok(dst)
}

/// Removes the link at `rel` (only a link: what it leads to is never touched).
fn remove_link(target: &Path, rel: &str) -> Result<(), String> {
    if !Login::plain(rel) {
        return Ok(());
    }
    let p = target.join(rel);
    match fs::symlink_metadata(&p) {
        Ok(m) if m.file_type().is_symlink() => {
            fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))
        }
        _ => Ok(()),
    }
}

/// Removes the file (or a link) at `rel`, never a directory or what a link leads to.
fn remove_file(target: &Path, rel: &str) -> Result<(), String> {
    if !Login::plain(rel) {
        return Ok(());
    }
    let p = target.join(rel);
    match fs::symlink_metadata(&p) {
        Ok(m) if !m.is_dir() => fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display())),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(id: &str) -> Login {
        steve_acp::adapters::spec(id, None)
            .properties
            .state
            .unwrap()
            .login
    }

    #[test]
    fn the_login_is_a_link_to_the_nodes_and_a_refresh_lands_there() {
        let d = tempfile::tempdir().unwrap();
        let (node, svc) = (d.path().join("node"), d.path().join("svc"));
        fs::create_dir_all(&node).unwrap();
        fs::create_dir_all(&svc).unwrap();
        // linked before the Node signs in: a sign-in made later is seen
        place(&node, &svc, &login("codex")).unwrap();
        let link = svc.join("auth.json");
        assert_eq!(fs::read_link(&link).unwrap(), node.join("auth.json"));
        fs::write(node.join("auth.json"), "{\"tokens\":1}").unwrap();
        assert_eq!(fs::read_to_string(&link).unwrap(), "{\"tokens\":1}");
        // the harness renews as Codex does: resolve the link, then rename a new file over its
        // target in the target's own directory: still one login, the link still a link
        let to = fs::canonicalize(&link).unwrap();
        let tmp = to.parent().unwrap().join(".auth-tmp");
        fs::write(&tmp, "{\"tokens\":2}").unwrap();
        fs::rename(&tmp, &to).unwrap();
        assert_eq!(
            fs::read_to_string(node.join("auth.json")).unwrap(),
            "{\"tokens\":2}"
        );
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // and as Pi does: written through the link
        fs::write(&link, "{\"tokens\":3}").unwrap();
        assert_eq!(
            fs::read_to_string(node.join("auth.json")).unwrap(),
            "{\"tokens\":3}"
        );
        // again: unchanged; a link planted elsewhere is replaced, not followed
        place(&node, &svc, &login("codex")).unwrap();
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(d.path().join("elsewhere"), &link).unwrap();
        place(&node, &svc, &login("codex")).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), node.join("auth.json"));
        assert!(!d.path().join("elsewhere").exists());
        // a sign-in of the directory's own is left alone
        fs::remove_file(&link).unwrap();
        fs::write(&link, "own").unwrap();
        let notes = place(&node, &svc, &login("codex")).unwrap();
        assert!(notes.iter().any(|n| n.contains("auth.json")), "{notes:?}");
        assert_eq!(fs::read_to_string(&link).unwrap(), "own");
        assert_eq!(
            fs::read_to_string(node.join("auth.json")).unwrap(),
            "{\"tokens\":3}"
        );
    }

    #[test]
    fn a_store_variable_links_nothing() {
        let d = tempfile::tempdir().unwrap();
        let (node, svc) = (d.path().join("node"), d.path().join("svc"));
        fs::create_dir_all(&node).unwrap();
        fs::create_dir_all(&svc).unwrap();
        fs::write(node.join(".credentials.json"), "{}").unwrap();
        place(&node, &svc, &login("claude-code")).unwrap();
        assert!(fs::symlink_metadata(svc.join(".credentials.json")).is_err());
        assert!(!svc.join(MANIFEST).exists());
    }

    #[test]
    fn the_sign_in_entries_are_the_nodes_and_the_rest_stays() {
        let d = tempfile::tempdir().unwrap();
        let (node, svc) = (d.path().join("node"), d.path().join("svc"));
        for p in [&node, &svc] {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(
            node.join("config.toml"),
            "model = \"m\"\nmodel_provider = \"gw\"\nnotify = [\"x\"]\n\
             [model_providers.gw]\nbase_url = \"https://gw.example.test/v1\"\nwire_api = \"responses\"\n\
             [model_providers.gw.auth]\ncommand = \"/bin/cat\"\nargs = [\"/k\"]\n\
             [mcp_servers.mine]\ncommand = \"secret-tool\"\n",
        )
        .unwrap();
        place(&node, &svc, &login("codex")).unwrap();
        let read = |p: &Path| -> toml::Table {
            toml::from_str(&fs::read_to_string(p.join("config.toml")).unwrap()).unwrap()
        };
        let t = read(&svc);
        assert_eq!(t["model_provider"].as_str(), Some("gw"));
        assert_eq!(
            t["model_providers"]["gw"]["auth"]["command"].as_str(),
            Some("/bin/cat")
        );
        for personal in ["model", "notify", "mcp_servers"] {
            assert!(!t.contains_key(personal), "{personal}: {t:?}");
        }
        // what the harness writes there itself stays; the Node's change of provider comes in
        let mut own = t.clone();
        own.insert("model".into(), "mine".into());
        fs::write(svc.join("config.toml"), toml::to_string(&own).unwrap()).unwrap();
        fs::write(
            node.join("config.toml"),
            "model_provider = \"other\"\n[model_providers.other]\nbase_url = \"https://o.example.test\"\n",
        )
        .unwrap();
        place(&node, &svc, &login("codex")).unwrap();
        let t = read(&svc);
        assert_eq!(t["model"].as_str(), Some("mine"));
        assert_eq!(t["model_provider"].as_str(), Some("other"));
        assert!(t["model_providers"].get("gw").is_none(), "{t:?}");
        // the Node names no provider any more: neither does the session; its own entries stay
        fs::write(node.join("config.toml"), "model = \"m\"\n").unwrap();
        place(&node, &svc, &login("codex")).unwrap();
        let t = read(&svc);
        assert!(!t.contains_key("model_provider") && !t.contains_key("model_providers"));
        assert_eq!(t["model"].as_str(), Some("mine"));
        // a settings file nobody had is not made when the Node's has no sign-in entries
        let fresh = d.path().join("fresh");
        fs::create_dir_all(&fresh).unwrap();
        place(&node, &fresh, &login("codex")).unwrap();
        assert!(!fresh.join("config.toml").exists());
        // an unchanged file is not rewritten
        let before = fs::metadata(svc.join("config.toml"))
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        place(&node, &svc, &login("codex")).unwrap();
        let after = fs::metadata(svc.join("config.toml"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn a_settings_link_or_unreadable_file_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let (node, svc) = (d.path().join("node"), d.path().join("svc"));
        for p in [&node, &svc] {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(node.join("config.toml"), "model_provider = \"gw\"\n").unwrap();
        let victim = d.path().join("victim.toml");
        fs::write(&victim, "x = 1\n").unwrap();
        std::os::unix::fs::symlink(&victim, svc.join("config.toml")).unwrap();
        let notes = place(&node, &svc, &login("codex")).unwrap();
        assert!(!notes.is_empty());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "x = 1\n");
        fs::remove_file(svc.join("config.toml")).unwrap();
        fs::write(svc.join("config.toml"), "not = [toml").unwrap();
        place(&node, &svc, &login("codex")).unwrap();
        assert_eq!(
            fs::read_to_string(svc.join("config.toml")).unwrap(),
            "not = [toml"
        );
    }

    #[test]
    fn the_provider_file_of_0_2_114_goes() {
        let d = tempfile::tempdir().unwrap();
        let (node, svc) = (d.path().join("node"), d.path().join("svc"));
        for p in [&node, &svc] {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(
            svc.join("config.toml"),
            "model_provider = \"steve\"\n[model_providers.steve]\nenv_key = \"STEVE_MODEL_ACCESS_KEY\"\n",
        )
        .unwrap();
        fs::write(svc.join(LEGACY_MODEL_ACCESS), "[\"config.toml\"]").unwrap();
        fs::write(svc.join("history.jsonl"), "h").unwrap();
        place(&node, &svc, &login("codex")).unwrap();
        assert!(!svc.join("config.toml").exists());
        assert!(!svc.join(LEGACY_MODEL_ACCESS).exists());
        assert!(svc.join("history.jsonl").exists());
    }

    #[test]
    fn json_entries_of_a_table_and_prefixes() {
        let raw = br#"{"apiKeyHelper":"/bin/cat /k","model":"x","permissions":{"allow":[]},
            "env":{"ANTHROPIC_BASE_URL":"https://gw.example.test","ANTHROPIC_DEFAULT_OPUS_MODEL":"o","GITHUB_TOKEN":"t"}}"#;
        let login = login("claude-code");
        let out = pick("settings.json", raw, &login.entries).unwrap().unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"apiKeyHelper":"/bin/cat /k","env":{
                "ANTHROPIC_BASE_URL":"https://gw.example.test","ANTHROPIC_DEFAULT_OPUS_MODEL":"o"}})
        );
        assert_eq!(
            pick("settings.json", br#"{"model":"x"}"#, &login.entries).unwrap(),
            None
        );
        assert!(pick("settings.yaml", b"a: 1", &login.entries).is_err());
        assert_eq!(
            pick("settings.json", raw, &login.entries).unwrap(),
            pick("settings.json", raw, &login.entries).unwrap()
        );
        // into a session's own file: its other entries, and its own `env` entries, stay
        let ours = Doc::parse(
            Format::Json,
            br#"{"model":"mine","env":{"GITHUB_TOKEN":"g","ANTHROPIC_BASE_URL":"old"}}"#,
        )
        .unwrap();
        let theirs = Doc::parse(Format::Json, raw).unwrap();
        let Doc::Json(m) = ours.with_entries_of(&theirs, &login.entries) else {
            unreachable!()
        };
        assert_eq!(
            Value::Object(m),
            serde_json::json!({"model":"mine","env":{"GITHUB_TOKEN":"g",
                "ANTHROPIC_BASE_URL":"https://gw.example.test","ANTHROPIC_DEFAULT_OPUS_MODEL":"o"},
                "apiKeyHelper":"/bin/cat /k"})
        );
    }
}
