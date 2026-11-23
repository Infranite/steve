use super::*;

pub fn seed_turn_msg_t1(store: &Store, text: &str, text2: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO messages (id, workspace_id, space, author, text, created, meta) VALUES ('msg_t1', 'default', ?1, 'me', '告警脚本坏了', 1, ?2)"#, params![text, text2])?;
        Ok(())
    })
}

pub fn set_turn_meta(store: &Store, id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE messages SET meta=json_remove(meta, '$.chan_gen') WHERE id=?1"#,
            params![id],
        )?;
        Ok(())
    })
}

pub fn set_turn_meta_for_private_outcomes_and_unknown_snapsh(
    store: &Store,
    id: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE messages SET meta=json_remove(meta, '$.chan_gen.hits') WHERE id=?1"#,
            params![id],
        )?;
        Ok(())
    })
}

pub fn set_turn_meta_for_private_outcomes_and_unknown_snapshots(
    store: &Store,
    id: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE messages SET meta=json_set(meta, '$.chan_gen', 'corrupt') WHERE id=?1"#,
            params![id],
        )?;
        Ok(())
    })
}

pub fn set_turn_space_chan_another_chat(store: &Store, id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE messages SET space='chan:another-chat' WHERE id=?1"#,
            params![id],
        )?;
        Ok(())
    })
}

pub fn clear_conversation_generation(store: &Store, id: &str) -> Result<()> {
    fixture_write(store, |c| {
        c.execute(
            "UPDATE messages SET meta=json_remove(meta,'$.chan_gen') WHERE id=?",
            [id],
        )?;
        Ok(())
    })
}
