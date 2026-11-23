use super::*;

pub fn seed_seed_output_records(store: &Store, con: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO output_deliveries(id,workspace_id,requester,idempotency_key,fingerprint,intent,connection_id,space_id,message_id,replied_at,reply_message_id,created) VALUES
        ('output-delete','default','usr_dev','delete-key','original-hash','{"identity":"bot","content":{"title":"private-title","markdown":"private-body","fields":[["private-field","value"]],"actions":[["private-action","https://example.org/private"]],"references":[["private-source","https://example.org/source"]]}}',?1,'sp_a','m2',7,'m1',1),
        ('output-keep','default','usr_dev','keep-key','keep-hash','{"identity":"bot","content":{"markdown":"retained"}}',?1,'sp_b','m3',7,'m1',1);"#, params![con])?;
        c.execute_batch(r#"INSERT INTO output_channel_preferences(workspace_id,user_id,party_id,manual_space_id,learned_space_id,updated) VALUES
        ('default','usr_dev','ann','sp_a','sp_b',1),
        ('default','usr_dev','bob','sp_b','sp_a',1);"#)?;
        Ok(())
    })
}

pub fn set_output_preference_learned_space_id(store: &Store, one_space_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE output_channel_preferences SET learned_space_id=?1"#,
            params![one_space_id],
        )?;
        Ok(())
    })
}

pub fn expire_connector_reference(store: &Store, reference: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE connector_refs SET expires=0 WHERE id=?",
            [reference],
        )?;
        Ok::<_, crate::StoreError>(())
    })
}
