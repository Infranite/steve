//! The owner's own instructions and extensions in a session's harness directory
//! ([`steve_acp::StateDir::context`]): the user-level `AGENTS.md` / `CLAUDE.md`, skills, subagents and
//! commands the harness reads from its directory, which a session of the owner finds in its own
//! directory as it would in the owner's without Steve.
//!
//! Each is a link to the owner's file or directory, not a copy: the harness reads what the owner has
//! now, an edit is seen by the next session, and nothing is duplicated. The Node keeps a list of the
//! links it made (`.steve-context.json` beside them) and, each time the harness opens, makes them the
//! owner's current ones: new ones linked, gone ones unlinked. A file or directory the harness or a
//! person made there is left alone. A Bot skill Steve wrote under the same name in the skills
//! directory gives way: the owner's own skill is what they meant.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::attachments::{private_dir, publish};

/// The list of the links Steve made, beside them.
const MANIFEST: &str = ".steve-context.json";

/// Links `specs` (relative to `own`, the owner's harness directory) into `target`, the session's.
/// `skills`: the harness's skills directory under `target`, where Bot skills Steve wrote may give
/// way. Blocking. Returns what was left alone and why (for the log); an `Err` is a spec that is not
/// a plain relative path or a step that could not be made.
pub(crate) fn link(
    own: &Path,
    target: &Path,
    specs: &[String],
    skills: Option<&str>,
) -> Result<Vec<String>, String> {
    let before = written(target);
    let want = wanted(own, specs)?;
    let mut notes = Vec::new();
    // gone from the owner's (or no longer asked for): only a link Steve made goes
    for rel in before.iter().filter(|r| !want.contains_key(*r)) {
        if plain(rel).is_err() {
            continue;
        }
        let p = target.join(rel);
        if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
            fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    let mut made = Vec::new();
    for (rel, src) in &want {
        let segs: Vec<&str> = rel.split('/').collect();
        // every step a real directory made here, never one planted as a link
        let mut dst = target.to_path_buf();
        for seg in &segs[..segs.len() - 1] {
            dst.push(seg);
            private_dir(&dst)?;
        }
        dst.push(segs[segs.len() - 1]);
        match fs::symlink_metadata(&dst) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", dst.display())),
            Ok(m) if m.file_type().is_symlink() && before.contains(rel) => {
                if fs::read_link(&dst).is_ok_and(|to| &to == src) {
                    made.push(rel.clone());
                    continue;
                }
                fs::remove_file(&dst).map_err(|e| format!("{}: {e}", dst.display()))?;
            }
            Ok(m) if m.is_dir() && skills.is_some_and(|s| bot_skill(target, s, rel)) => {
                // a Bot skill of the same name: the owner's wins
                let _ = fs::remove_file(dst.join("SKILL.md"));
                if fs::remove_dir(&dst).is_err() {
                    notes.push(format!("{rel}: a Bot skill with other files in it is left"));
                    continue;
                }
            }
            Ok(_) => {
                notes.push(format!("{rel}: already there, left alone"));
                continue;
            }
        }
        std::os::unix::fs::symlink(src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        made.push(rel.clone());
    }
    if made.is_empty() && before.is_empty() {
        return Ok(notes);
    }
    private_dir(target)?;
    let list = serde_json::to_vec(&made).map_err(|e| e.to_string())?;
    publish(&target.join(MANIFEST), &list)?;
    Ok(notes)
}

/// What the owner has of `specs` now: relative path in the session's directory → the owner's.
fn wanted(own: &Path, specs: &[String]) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut want = BTreeMap::new();
    for spec in specs {
        if let Some(dir) = spec.strip_suffix("/*") {
            plain(dir)?;
            let Ok(entries) = fs::read_dir(own.join(dir)) else {
                continue;
            };
            for e in entries.flatten() {
                let Some(name) = e.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                // the harness's own (`.system`), and a link that leads nowhere
                if name.starts_with('.') || plain(&name).is_err() || fs::metadata(e.path()).is_err()
                {
                    continue;
                }
                want.insert(format!("{dir}/{name}"), e.path());
            }
        } else {
            plain(spec)?;
            let src = own.join(spec);
            if fs::metadata(&src).is_ok() {
                want.insert(spec.clone(), src);
            }
        }
    }
    Ok(want)
}

/// A relative path of plain names.
fn plain(rel: &str) -> Result<(), String> {
    let ok = !rel.is_empty()
        && rel.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && !p.chars().any(|c| c == '\\' || c.is_control())
        });
    if ok {
        Ok(())
    } else {
        Err(format!(
            "context entry {rel:?} is not a plain relative path"
        ))
    }
}

/// `rel` is a skill Steve wrote for a Bot in `target`/`skills`.
fn bot_skill(target: &Path, skills: &str, rel: &str) -> bool {
    let Some(name) = rel.strip_prefix(skills).and_then(|r| r.strip_prefix('/')) else {
        return false;
    };
    !name.contains('/') && crate::bot_skills::steve_made(&target.join(skills), name)
}

/// The links Steve made here before (none when the list is missing or unreadable).
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

    fn specs(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_owners_instructions_and_skills_are_linked_and_follow_the_owner() {
        let own = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (own, state) = (own.path(), state.path());
        fs::write(own.join("AGENTS.md"), "be brief").unwrap();
        fs::create_dir_all(own.join("skills/review")).unwrap();
        fs::write(own.join("skills/review/SKILL.md"), "review").unwrap();
        // the harness's own, and a skill kept elsewhere behind a link (as ~/.agents/skills often is)
        fs::create_dir_all(own.join("skills/.system/x")).unwrap();
        let shared = tempfile::tempdir().unwrap();
        fs::create_dir_all(shared.path().join("lark")).unwrap();
        fs::write(shared.path().join("lark/SKILL.md"), "lark").unwrap();
        std::os::unix::fs::symlink(shared.path().join("lark"), own.join("skills/lark")).unwrap();
        std::os::unix::fs::symlink(own.join("nowhere"), own.join("skills/dangling")).unwrap();
        let s = specs(&["AGENTS.md", "AGENTS.override.md", "skills/*", "prompts"]);

        assert!(link(own, state, &s, Some("skills")).unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(state.join("AGENTS.md")).unwrap(),
            "be brief"
        );
        assert_eq!(
            fs::read_to_string(state.join("skills/review/SKILL.md")).unwrap(),
            "review"
        );
        assert_eq!(
            fs::read_to_string(state.join("skills/lark/SKILL.md")).unwrap(),
            "lark"
        );
        assert!(fs::symlink_metadata(state.join("skills")).unwrap().is_dir());
        assert!(!state.join("skills/.system").exists());
        assert!(fs::symlink_metadata(state.join("skills/dangling")).is_err());
        assert!(!state.join("AGENTS.override.md").exists());

        // an edit is seen at once; what the owner adds or removes follows at the next open
        fs::write(own.join("AGENTS.md"), "be thorough").unwrap();
        assert_eq!(
            fs::read_to_string(state.join("AGENTS.md")).unwrap(),
            "be thorough"
        );
        fs::write(own.join("AGENTS.override.md"), "override").unwrap();
        fs::remove_dir_all(own.join("skills/review")).unwrap();
        link(own, state, &s, Some("skills")).unwrap();
        assert!(state.join("AGENTS.override.md").exists());
        assert!(fs::symlink_metadata(state.join("skills/review")).is_err());
        // the owner's files were never touched through the links
        assert_eq!(
            fs::read_to_string(shared.path().join("lark/SKILL.md")).unwrap(),
            "lark"
        );

        // nothing asked for any more: Steve's links go, nothing else
        fs::write(state.join("mine.md"), "mine").unwrap();
        link(own, state, &[], Some("skills")).unwrap();
        assert!(fs::symlink_metadata(state.join("AGENTS.md")).is_err());
        assert!(fs::symlink_metadata(state.join("skills/lark")).is_err());
        assert!(state.join("mine.md").exists());
        assert!(own.join("AGENTS.md").exists());
    }

    #[test]
    fn what_the_harness_or_a_person_made_stays_and_a_bot_skill_gives_way() {
        let own = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (own, state) = (own.path(), state.path());
        for n in ["review", "notes", "kept"] {
            fs::create_dir_all(own.join("skills").join(n)).unwrap();
            fs::write(
                own.join("skills").join(n).join("SKILL.md"),
                format!("owner {n}"),
            )
            .unwrap();
        }
        fs::write(own.join("CLAUDE.md"), "owner").unwrap();
        // the session's own CLAUDE.md, a Bot skill of the same name, one with a file of the user's
        // in it, and a skill the harness made
        fs::write(state.join("CLAUDE.md"), "the session's").unwrap();
        let bot = |n: &str| steve_node_api::Skill {
            name: n.into(),
            description: "d".into(),
            content: "bot".into(),
        };
        crate::bot_skills::place(state, "skills", &[bot("review"), bot("notes")]).unwrap();
        fs::write(state.join("skills/notes/todo.txt"), "keep").unwrap();
        fs::create_dir_all(state.join("skills/kept")).unwrap();
        fs::write(state.join("skills/kept/SKILL.md"), "harness").unwrap();

        let notes = link(
            own,
            state,
            &specs(&["CLAUDE.md", "skills/*"]),
            Some("skills"),
        )
        .unwrap();
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert_eq!(
            fs::read_to_string(state.join("CLAUDE.md")).unwrap(),
            "the session's"
        );
        assert_eq!(
            fs::read_to_string(state.join("skills/review/SKILL.md")).unwrap(),
            "owner review"
        );
        assert_eq!(
            fs::read_to_string(state.join("skills/kept/SKILL.md")).unwrap(),
            "harness"
        );
        assert!(state.join("skills/notes/todo.txt").exists());

        // the Bot's skills placed again: the owner's skill under that name is not written over
        crate::bot_skills::place(state, "skills", &[bot("review"), bot("notes")]).unwrap();
        assert_eq!(
            fs::read_to_string(state.join("skills/review/SKILL.md")).unwrap(),
            "owner review"
        );
        assert_eq!(
            fs::read_to_string(own.join("skills/review/SKILL.md")).unwrap(),
            "owner review"
        );
        // and removing it never reaches through the link into the owner's directory
        crate::bot_skills::place(state, "skills", &[]).unwrap();
        assert_eq!(
            fs::read_to_string(own.join("skills/review/SKILL.md")).unwrap(),
            "owner review"
        );
    }

    #[test]
    fn entries_must_be_plain_and_a_planted_link_is_not_followed() {
        let own = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        for bad in ["", "../x", "/abs", "a//b", "a/./b", "../*"] {
            assert!(
                link(own.path(), state.path(), &specs(&[bad]), None).is_err(),
                "{bad}"
            );
        }
        fs::create_dir_all(own.path().join("skills/a")).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), state.path().join("skills")).unwrap();
        assert!(link(own.path(), state.path(), &specs(&["skills/*"]), None).is_err());
        assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }
}
