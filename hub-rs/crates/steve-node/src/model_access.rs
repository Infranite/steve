//! A service session's model provider in its own harness directory ([`steve_node_api::ModelAccess`]):
//! the harness's own provider file (the adapter's [`steve_acp::ModelProvider`]) written from the
//! Hub's setting, so the harness reaches the provider the Hub names instead of falling back to its
//! default login. Nothing of the Node owner's directory is read or copied: a service principal's
//! directory starts empty and gets only this.
//!
//! The file is Steve's: written whole each time the harness opens (the setting may have changed),
//! and taken away at the next open once the Hub gives none. The Node keeps the list of what it
//! wrote (`.steve-model-access.json` beside it), so a file the harness or a person made under
//! another name is never touched. The provider's key is not in the file; the harness reads it from
//! its environment.

use std::fs;
use std::path::Path;

use steve_acp::ModelProvider;

use crate::attachments::{private_dir, publish};

/// The list of the files Steve wrote, beside them.
const MANIFEST: &str = ".steve-model-access.json";

/// Writes `provider`'s file for `base_url` into `target` (the harness's directory), or with
/// `base_url` `None` takes away the one Steve wrote before. Blocking.
pub(crate) fn place(
    target: &Path,
    provider: &ModelProvider,
    base_url: Option<&str>,
) -> Result<(), String> {
    if !provider.valid() {
        return Err("the adapter's model provider is not a plain file".into());
    }
    let before = written(target);
    // a file written before under another name (the adapter changed) goes
    for rel in before.iter().filter(|r| **r != provider.file) {
        remove_ours(target, rel)?;
    }
    let Some(url) = base_url else {
        if before.iter().any(|r| *r == provider.file) {
            remove_ours(target, &provider.file)?;
        }
        if fs::symlink_metadata(target.join(MANIFEST)).is_ok() {
            fs::remove_file(target.join(MANIFEST))
                .map_err(|e| format!("{}: {e}", target.display()))?;
        }
        return Ok(());
    };
    // every step a real directory made here, never one planted as a link
    let segs: Vec<&str> = provider.file.split('/').collect();
    let mut dst = target.to_path_buf();
    for seg in &segs[..segs.len() - 1] {
        dst.push(seg);
        private_dir(&dst)?;
    }
    dst.push(segs[segs.len() - 1]);
    if fs::symlink_metadata(&dst).is_ok_and(|m| m.is_dir()) {
        return Err(format!("{} is a directory", dst.display()));
    }
    // one rename: a link left at the path is replaced, never written through
    publish(&dst, &provider.render(url))?;
    let list = serde_json::to_vec(&[&provider.file]).map_err(|e| e.to_string())?;
    publish(&target.join(MANIFEST), &list)
}

/// Removes `rel` under `target` when it is a file (or a link) there, never what a link leads to.
fn remove_ours(target: &Path, rel: &str) -> Result<(), String> {
    if rel.is_empty()
        || rel
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Ok(());
    }
    let p = target.join(rel);
    match fs::symlink_metadata(&p) {
        Ok(m) if !m.is_dir() => fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display())),
        _ => Ok(()),
    }
}

/// The files Steve wrote here before (none when the list is missing or unreadable).
fn written(target: &Path) -> Vec<String> {
    let f = target.join(MANIFEST);
    if !fs::symlink_metadata(&f).is_ok_and(|m| m.file_type().is_file()) {
        return vec![];
    }
    fs::read(&f)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_acp::ProviderFormat;

    fn toml() -> ModelProvider {
        ModelProvider {
            file: "config.toml".into(),
            format: ProviderFormat::ModelProvidersToml {
                table: "steve".into(),
                wire_api: "responses".into(),
            },
            key_env: "STEVE_MODEL_ACCESS_KEY".into(),
            required: true,
        }
    }

    #[test]
    fn written_whole_each_time_and_taken_away_when_the_hub_gives_none() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path();
        place(t, &toml(), Some("https://one.example.test")).unwrap();
        let first = fs::read_to_string(t.join("config.toml")).unwrap();
        assert!(first.contains("https://one.example.test"));
        assert!(!first.contains("KEY="), "no key in the file");
        place(t, &toml(), Some("https://two.example.test")).unwrap();
        let second = fs::read_to_string(t.join("config.toml")).unwrap();
        assert!(second.contains("https://two.example.test") && !second.contains("one."));
        // something of the harness's own stays
        fs::write(t.join("history.jsonl"), "h").unwrap();
        place(t, &toml(), None).unwrap();
        assert!(!t.join("config.toml").exists());
        assert!(!t.join(MANIFEST).exists());
        assert!(t.join("history.jsonl").exists());
        // a file nobody wrote through Steve is not taken away
        fs::write(t.join("config.toml"), "mine").unwrap();
        place(t, &toml(), None).unwrap();
        assert_eq!(fs::read_to_string(t.join("config.toml")).unwrap(), "mine");
    }

    #[test]
    fn a_planted_link_is_replaced_not_written_through() {
        let d = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let victim = elsewhere.path().join("config.toml");
        fs::write(&victim, "owner's").unwrap();
        std::os::unix::fs::symlink(&victim, d.path().join("config.toml")).unwrap();
        place(d.path(), &toml(), Some("https://gw.example.test")).unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "owner's");
        assert!(
            !fs::symlink_metadata(d.path().join("config.toml"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
