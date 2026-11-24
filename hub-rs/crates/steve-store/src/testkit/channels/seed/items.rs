use super::*;

pub fn seed_item(store: &Store, item: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO items(id, workspace_id, no, title, phase, bot_id, source, created, updated, outcome) VALUES (?1, 'default', (SELECT coalesce(max(no), 0) + 1 FROM items), '修告警脚本', 'closed', 'ops', '["运维群","chan"]', 1, 1, 'completed');"#, params![item])?;
        Ok(())
    })
}

pub fn seed_item_st_2(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO items(id, workspace_id, no, title, phase, source, created, updated, outcome) VALUES ('ST-2', 'default', 99, 'x', 'closed', '[]', 1, 1, 'completed');"#)?;
        Ok(())
    })
}

pub fn seed_item_st_7(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO items(id, workspace_id, no, title, phase, source, created, updated) VALUES ('ST-7', 'default', 7, '回滚', 'active', '[]', 1, 1)"#)?;
        Ok(())
    })
}

pub fn seed_item_st_9(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO items(id, workspace_id, no, title, phase, source, created, updated) VALUES ('ST-9','default',9,'渠道事项','active','[]',1,1)"#)?;
        Ok(())
    })
}

pub fn seed_prompt_has_seven_layers_in_order(store: &Store, ws: &str, party: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO items(id, workspace_id, no, kind, title, phase, created, updated) VALUES ('ST-9', ?1, 9, 'task', '监控迁移', 'active', 1, 1);"#, params![ws])?;
        c.execute("INSERT INTO timeline(item_id,ts,actor,kind,text) VALUES('ST-9',1,'dev','progress','迁移一半')", [])?;
        c.execute(r#"INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, source, created) VALUES (?1, 'item', 'ST-9', 'raised_by', 'party', ?2, 'me', 1);"#, params![ws, party])?;
        Ok(())
    })
}

pub fn seed_thread_reply_by_the_raiser_goes_to_its_item(store: &Store, item: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT OR IGNORE INTO items(id, workspace_id, no, title, phase, created, updated) VALUES (?1, 'default', 901, 't', 'active', 0, 0);"#, params![item])?;
        c.execute(
            r#"UPDATE items SET phase = 'active' WHERE id = ?1"#,
            params![item],
        )?;
        Ok(())
    })
}

pub fn set_item_phase_closed(store: &Store, item: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE items SET phase = 'closed' WHERE id = ?1"#,
            params![item],
        )?;
        Ok(())
    })
}
