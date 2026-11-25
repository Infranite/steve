use super::*;

pub fn seed_standalone_decisions_are_previewed_and_deleted_atomically(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, status, requested_by, created) VALUES
      ('d1','default','reply','chan_message','m1','reply?','open','usr_dev',1),
      ('d2','default','reply','chan_message','m2','reply?','resolved','usr_dev',1);"#)?;
        c.execute_batch(r#"UPDATE chan_messages SET decision_id='d1' WHERE id='m1';"#)?;
        c.execute_batch(r#"UPDATE ext_events SET decision_id='d1' WHERE message_id='m1';"#)?;
        Ok(())
    })
}

pub fn seed_shared_decision_and_changed_preview_are_not_deleted(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, status, requested_by, created) VALUES
      ('shared','default','reply','chan_message','m1','reply?','open','usr_dev',1);"#)?;
        c.execute_batch(
            r#"UPDATE chan_messages SET decision_id='shared' WHERE id IN ('m1','m3');"#,
        )?;
        Ok(())
    })
}

pub fn seed_decision_foreign(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, question, status, requested_by, created) VALUES
      ('foreign','other','reply','chan_message','m1',NULL,'reply?','open','usr_dev',1),
      ('item_decision','default','reply','space','sp_c','it_1','reply?','open','usr_dev',1);"#)?;
        Ok(())
    })
}

pub fn set_decision_status_resolved(store: &Store, id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"UPDATE decisions SET status = 'resolved', result = '{"dismissed":true}' WHERE id = ?1"#, params![id])?;
        Ok(())
    })
}

pub fn seed_decision(store: &Store, text: &str, old: i64, id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, status, requested_by, created)
                 VALUES (?1, 'default', 'ask', 'chan_message', ?2, '机器人转给你', 'open', 'steve', ?3)"#, params![text, id, old])?;
        Ok(())
    })
}

pub fn resolve_all_approvals(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE decisions SET status = 'resolved' WHERE kind = 'approve'",
            [],
        )?;
        Ok(())
    })
}
