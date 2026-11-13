//! The skills of a session's Bot ([`OpenSession::skills`]) as files where its harness reads them:
//! `<state dir>/<adapter's skills dir>/<name>/SKILL.md`, the adapter's declared place
//! (`AdapterProperties::skills`) under the session's own harness directory, which is the
//! principal's for this Bot and harness. Nothing here knows a harness by name.
//!
//! The Node keeps a list of the skills it wrote (`.steve-skills.json` beside them) and, each time
//! the harness opens, makes that directory's Steve-made skills exactly the session's: new and
//! changed ones written whole, removed or disabled ones taken away. A skill the harness or a person
//! put there is left alone; of a removed skill only its `SKILL.md` goes, and its directory only
//! when nothing else is in it.

use std::fs;
use std::path::{Path, PathBuf};

use steve_node_api::Skill;

use crate::attachments::{private_dir, publish};

/// The list of the skills Steve wrote, beside them.
const MANIFEST: &str = ".steve-skills.json";

/// Puts `skills` under `state`/`sub`. Blocking. `Err`: why not (nothing is left half written:
/// each file is replaced whole).
pub(crate) fn place(state: &Path, sub: &str, skills: &[Skill]) -> Result<(), String> {
    let dir = skills_dir(state, sub)?;
    let before = written(&dir);
    if before.is_empty() && skills.is_empty() {
        return Ok(());
    }
    // every step a real directory made here, never one planted as a link
    let mut step = state.to_path_buf();
    for part in sub.split('/') {
        step.push(part);
        private_dir(&step)?;
    }
    for gone in before
        .iter()
        .filter(|n| Skill::name_ok(n) && !skills.iter().any(|s| &s.name == *n))
    {
        let d = dir.join(gone);
        let f = d.join("SKILL.md");
        if fs::symlink_metadata(&f).is_ok_and(|m| m.file_type().is_file()) {
            fs::remove_file(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        }
        // only when nothing else is in it
        let _ = fs::remove_dir(&d);
    }
    for s in skills {
        if !Skill::name_ok(&s.name) {
            continue;
        }
        let d = dir.join(&s.name);
        private_dir(&d)?;
        let f = d.join("SKILL.md");
        let text = s.markdown();
        let same = fs::symlink_metadata(&f).is_ok_and(|m| m.file_type().is_file())
            && fs::read(&f).is_ok_and(|b| b == text.as_bytes());
        if !same {
            publish(&f, text.as_bytes())?;
        }
    }
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    let list = serde_json::to_vec(&names).map_err(|e| e.to_string())?;
    publish(&dir.join(MANIFEST), &list)
}

/// `state`/`sub`; `sub` must be a plain relative path.
fn skills_dir(state: &Path, sub: &str) -> Result<PathBuf, String> {
    let plain = !sub.is_empty()
        && sub.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        });
    if !plain {
        return Err(format!(
            "skills directory {sub:?} is not a plain relative path"
        ));
    }
    Ok(state.join(sub))
}

/// The names of the skills Steve wrote here before (none when the list is missing or unreadable).
fn written(dir: &Path) -> Vec<String> {
    let f = dir.join(MANIFEST);
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

    fn skill(name: &str, content: &str) -> Skill {
        Skill {
            name: name.into(),
            description: format!("Use for {name}: \"quoted\" and: colons"),
            content: content.into(),
        }
    }

    #[test]
    fn the_steve_made_skills_become_exactly_the_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        place(
            state,
            "skills",
            &[skill("review", "# Review\n"), skill("ship-it", "Ship.")],
        )
        .unwrap();
        let review = std::fs::read_to_string(state.join("skills/review/SKILL.md")).unwrap();
        assert_eq!(
            review,
            "---\nname: review\ndescription: \"Use for review: \\\"quoted\\\" and: colons\"\n---\n\n# Review\n"
        );
        assert!(state.join("skills/ship-it/SKILL.md").is_file());
        // one the harness made itself, and a file the user left in a skill of Steve's
        std::fs::create_dir_all(state.join("skills/own")).unwrap();
        std::fs::write(state.join("skills/own/SKILL.md"), "mine").unwrap();
        std::fs::write(state.join("skills/ship-it/notes.txt"), "keep").unwrap();
        place(state, "skills", &[skill("review", "Changed.")]).unwrap();
        assert!(
            std::fs::read_to_string(state.join("skills/review/SKILL.md"))
                .unwrap()
                .ends_with("\n\nChanged.\n")
        );
        assert!(!state.join("skills/ship-it/SKILL.md").exists());
        assert!(state.join("skills/ship-it/notes.txt").exists());
        assert_eq!(
            std::fs::read_to_string(state.join("skills/own/SKILL.md")).unwrap(),
            "mine"
        );
        // none left: Steve's go, the rest stays
        place(state, "skills", &[]).unwrap();
        assert!(!state.join("skills/review").exists());
        assert!(state.join("skills/own/SKILL.md").exists());
        // nothing ever given: nothing made
        let empty = tempfile::tempdir().unwrap();
        place(empty.path(), "skills", &[]).unwrap();
        assert!(!empty.path().join("skills").exists());
    }

    #[test]
    fn a_declared_place_must_be_plain_and_links_are_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        for bad in ["", "../x", "/abs", "a//b", "a/./b"] {
            assert!(place(tmp.path(), bad, &[skill("a", "x")]).is_err(), "{bad}");
        }
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), tmp.path().join("skills")).unwrap();
        assert!(place(tmp.path(), "skills", &[skill("a", "x")]).is_err());
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }
}
