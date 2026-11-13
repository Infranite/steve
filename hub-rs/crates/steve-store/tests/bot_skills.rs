//! Migration 63: a Bot's skills, and the ones a session's harness gets.

mod common;

use common::*;
use steve_store::bot_skills::{self, Skill};

fn skill(id: &str, bot: &str, name: &str) -> Skill {
    Skill {
        id: id.into(),
        workspace_id: "default".into(),
        bot_id: bot.into(),
        name: name.into(),
        description: format!("Use for {name}"),
        content: "# Steps\n1. Do it.".into(),
        enabled: true,
        created: 1,
        updated: 1,
    }
}

#[test]
fn a_session_gets_its_bots_enabled_skills_by_name() {
    let t = fresh();
    t.store
        .with_conn(|c| {
            c.execute_batch(
                "INSERT OR IGNORE INTO bots (id, workspace_id, name, harness, updated) VALUES ('dev', 'default', '开发', 'claude-code', 1), ('doc', 'default', '文档', 'codex', 1);
                 INSERT INTO sessions (id, workspace_id, bot_id, harness, cwd, status, created, updated) VALUES ('s_dev', 'default', 'dev', 'claude-code', '.', 'active', 1, 1), ('s_none', 'default', NULL, 'claude-code', '.', 'active', 1, 1);",
            )?;
            Ok(())
        })
        .unwrap();
    t.store
        .transaction(|tx| {
            bot_skills::insert(tx, &skill("bskill_a", "dev", "review"))?;
            bot_skills::insert(tx, &skill("bskill_b", "dev", "a-release"))?;
            bot_skills::insert(tx, &skill("bskill_c", "doc", "other"))?;
            let mut off = skill("bskill_d", "dev", "drafts");
            off.enabled = false;
            bot_skills::insert(tx, &off)
        })
        .unwrap();
    let names = |s: &str| {
        t.store
            .reader(|c| bot_skills::enabled_for_session(c, s))
            .unwrap()
            .into_iter()
            .map(|x| x.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(names("s_dev"), vec!["a-release", "review"]);
    assert!(names("s_none").is_empty());
    assert!(names("missing").is_empty());
    assert!(
        t.store
            .reader(|c| bot_skills::name_taken(c, "dev", "review", None))
            .unwrap()
    );
    assert!(
        !t.store
            .reader(|c| bot_skills::name_taken(c, "dev", "review", Some("bskill_a")))
            .unwrap()
    );
    let mut s = skill("bskill_a", "dev", "review");
    s.name = "code-review".into();
    s.content = "changed".into();
    s.updated = 7;
    t.store
        .transaction(|tx| bot_skills::update(tx, &s))
        .unwrap();
    let got = t
        .store
        .reader(|c| bot_skills::get(c, "bskill_a"))
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            got.name.as_str(),
            got.content.as_str(),
            got.updated,
            got.created
        ),
        ("code-review", "changed", 7, 1)
    );
    assert!(
        t.store
            .transaction(|tx| bot_skills::delete(tx, "bskill_b"))
            .unwrap()
    );
    assert_eq!(names("s_dev"), vec!["code-review"]);
    assert_eq!(
        t.store
            .reader(|c| bot_skills::list(c, "dev"))
            .unwrap()
            .len(),
        2
    );
}
