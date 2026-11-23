use super::*;

pub fn install_fail_cleanup(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"CREATE TRIGGER fail_cleanup BEFORE DELETE ON spaces BEGIN SELECT RAISE(ABORT, 'test rollback'); END;"#)?;
        Ok(())
    })
}

pub fn remove_fail_cleanup(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"DROP TRIGGER fail_cleanup;"#)?;
        Ok(())
    })
}

pub fn install_fail_yolo_cleanup(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"CREATE TRIGGER fail_yolo_cleanup BEFORE DELETE ON decisions BEGIN SELECT RAISE(ABORT, 'rollback'); END;"#)?;
        Ok(())
    })
}

pub fn remove_fail_yolo_cleanup(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"DROP TRIGGER fail_yolo_cleanup;"#)?;
        Ok(())
    })
}

pub fn install_fail_output_cleanup(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"CREATE TRIGGER fail_output_cleanup BEFORE DELETE ON spaces BEGIN SELECT RAISE(ABORT,'rollback'); END;"#)?;
        Ok(())
    })
}

pub fn remove_fail_output_cleanup(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"DROP TRIGGER fail_output_cleanup;"#)?;
        Ok(())
    })
}

pub fn copy_m0_templates(store: &Store, m: &str, d: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"CREATE TEMP TABLE m0 AS SELECT * FROM chan_messages WHERE id = ?1;"#,
            params![m],
        )?;
        c.execute(
            r#"CREATE TEMP TABLE d0 AS SELECT * FROM decisions WHERE id = ?1;"#,
            params![d],
        )?;
        Ok(())
    })
}

pub fn seed_messages_still_inside_their_delay_do_not_starve_a_due_one(
    store: &Store,
    text: &str,
    text2: &str,
    i: i64,
    later: i64,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE m0 SET id = ?1, decision_id = ?2, created = created - 1000 - ?3;"#,
            params![text, text2, i],
        )?;
        c.execute_batch(r#"INSERT INTO chan_messages SELECT * FROM m0;"#)?;
        c.execute(
            r#"UPDATE d0 SET id = ?1, subject_id = ?2,
                   result = json_set(result, '$.send_at', ?3);"#,
            params![text2, text, later],
        )?;
        c.execute_batch(r#"INSERT INTO decisions SELECT * FROM d0;"#)?;
        Ok(())
    })
}

pub fn install_reject_ledger(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"CREATE TRIGGER reject_ledger BEFORE INSERT ON output_deliveries BEGIN SELECT RAISE(ABORT,'injected ledger failure'); END;"#)?;
        Ok(())
    })
}

pub fn install_reject_notification(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"CREATE TRIGGER reject_notification BEFORE INSERT ON personal_notifications BEGIN SELECT RAISE(ABORT,'intake failure'); END;"#)?;
        Ok(())
    })
}

pub fn restore_reject_notification(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"DROP TRIGGER reject_notification;"#)?;
        c.execute_batch(r#"UPDATE outbox_leases SET next_at=0;"#)?;
        Ok(())
    })
}

pub fn install_reject_notification_link(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"CREATE TRIGGER reject_notification_link BEFORE UPDATE OF delivery_id ON personal_notifications BEGIN SELECT RAISE(ABORT,'link failure'); END;"#)?;
        Ok(())
    })
}

pub fn remove_reject_notification_link(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"DROP TRIGGER reject_notification_link;"#)?;
        Ok(())
    })
}

pub fn copy_contract_message_templates(store: &Store, m: &str, d: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"CREATE TEMP TABLE contract_message AS SELECT * FROM chan_messages WHERE id=?1;"#,
            params![m],
        )?;
        c.execute(
            r#"CREATE TEMP TABLE contract_decision AS SELECT * FROM decisions WHERE id=?1;"#,
            params![d],
        )?;
        c.execute(r#"CREATE TEMP TABLE contract_space AS SELECT * FROM spaces WHERE id=(SELECT space_id FROM chan_messages WHERE id=?1);"#, params![m])?;
        c.execute_batch(r#"UPDATE contract_space SET id='contract-orphan',external_id='contract-orphan',status='left';"#)?;
        c.execute_batch(r#"INSERT INTO spaces SELECT * FROM contract_space;"#)?;
        c.execute(
            r#"UPDATE chan_messages SET status='discarded' WHERE id=?1;"#,
            params![m],
        )?;
        Ok(())
    })
}

pub fn seed_contract_message(
    store: &Store,
    id: &str,
    space: &str,
    decision: &str,
    at: i64,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE contract_message SET id=?1,space_id=?2, decision_id=?3;"#,
            params![id, space, decision],
        )?;
        c.execute_batch(r#"INSERT INTO chan_messages SELECT * FROM contract_message;"#)?;
        c.execute(r#"UPDATE contract_decision SET id=?1,subject_id=?2,result=json_set(result,'$.send_at',?3);"#, params![decision, id, at])?;
        c.execute_batch(r#"INSERT INTO decisions SELECT * FROM contract_decision;"#)?;
        Ok(())
    })
}

pub fn seed_contract_message_for_durable_send_deadlines_keep_direct(
    store: &Store,
    now: i64,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"UPDATE contract_message SET id='contract-direct',decision_id=NULL,created=?1,sent=NULL;"#, params![now])?;
        c.execute_batch(r#"INSERT INTO chan_messages SELECT * FROM contract_message;"#)?;
        Ok(())
    })
}

pub fn seed_route_log_fixture_route(
    store: &Store,
    ws: &str,
    turn: &str,
    reply: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO route_log (id,workspace_id,message_id,reply_id,model_kind,kind,ms,created) VALUES ('fixture-route',?1,?2,?3,'fixture','ask',0,1)"#, params![ws, turn, reply])?;
        Ok(())
    })
}

pub fn seed_chan_message_seen(store: &Store, con: &str, msg: &str) -> Result<()> {
    fixture_write(store, |c| {
        c.execute(r#"INSERT INTO chan_message_seen (connection_id, source_id, message_id) VALUES (?1, 'dlv_m1', ?2)"#, params![con, msg])?;
        Ok(())
    })
}
