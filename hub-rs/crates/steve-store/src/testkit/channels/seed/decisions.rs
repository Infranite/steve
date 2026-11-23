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

pub fn seed_completed_yolo_history_does_not_pin_channel_decisions(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, status, requested_by, created, resolved) VALUES
      ('d1','default','approve','chan_message','m1','send?','resolved','steve',1,2),
      ('d2','default','fail','chan_message','m2','retry?','resolved','steve',1,2);"#)?;
        c.execute_batch(r#"UPDATE chan_messages SET decision_id='d1' WHERE id='m1';"#)?;
        c.execute_batch(r#"INSERT INTO yolo_actions (id,workspace_id,decision_id,kind,subject_type,rule,outcome,reason,undo,created) VALUES
      ('ya1','default','d1','approve','chan_message','channel.send','decided','sent','{"kind":"recall"}',1),
      ('ya2','default','d2','fail','chan_message','channel.retry','cancelled','cancelled','{"kind":"decision"}',1),
      ('ya3','default','d2','fail','chan_message','channel.retry','decided','done',NULL,1);"#)?;
        Ok(())
    })
}

pub fn seed_scheduled_yolo_action_and_new_execution_after_preview_are(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, status, requested_by, created) VALUES
      ('d1','default','approve','chan_message','m1','send?','open','steve',1);"#)?;
        c.execute_batch(r#"INSERT INTO yolo_actions (id,workspace_id,decision_id,kind,subject_type,rule,outcome,reason,created) VALUES
      ('ya1','default','d1','approve','chan_message','channel.send','scheduled','waiting',1);"#)?;
        Ok(())
    })
}

pub fn set_yolo_action_outcome_cancelled(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE yolo_actions SET outcome='cancelled' WHERE id='ya1';"#)?;
        Ok(())
    })
}

pub fn set_yolo_action_outcome_running(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE yolo_actions SET outcome='running' WHERE id='ya1';"#)?;
        Ok(())
    })
}

pub fn seed_recovery_review_fences_legacy_auto_approval_and_preserves(
    store: &Store,
    result: &str,
    review: &str,
    message: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE decisions SET status='resolved',result=?1 WHERE id=?2;"#,
            params![result, review],
        )?;
        c.execute(
            r#"UPDATE chan_messages SET status='pending',sent=NULL WHERE id=?1"#,
            params![message],
        )?;
        Ok(())
    })
}

pub fn set_decision_status_resolved(store: &Store, id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"UPDATE decisions SET status = 'resolved', result = '{"dismissed":true}' WHERE id = ?1"#, params![id])?;
        Ok(())
    })
}

pub fn set_decision_options(store: &Store, id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"UPDATE decisions SET options = '["draft_reply","to_item","done"]', draft = '' WHERE id = ?1"#, params![id])?;
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
    store.with_conn(|c| {
        c.execute(
            "UPDATE decisions SET status = 'resolved' WHERE kind = 'approve'",
            [],
        )?;
        Ok(())
    })
}

pub fn reopen_fixture_decision(w: &mut crate::W<'_>, id: &str) -> Result<()> {
    w.fixture_connection()
        .execute("UPDATE decisions SET status = 'open' WHERE id = ?", [id])?;
    Ok(())
}
