use super::*;

pub fn seed_chunk_default(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO kb_chunks (workspace_id, obj_type, obj_id, seq, heading, text, visibility, level, created_by, source_kind, label, updated)
         VALUES ('default', 'chunk', 'kd_acme', 2, '', 'Acme 的预算是 30万，下周签', 'space:spc_acme', 'reasoning', 'usr_dev', 'note', '笔记 · Acme', 1);"#)?;
        Ok(())
    })
}

pub fn remove_chunk(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"DELETE FROM kb_chunks WHERE obj_id = 'kd_acme'"#)?;
        Ok(())
    })
}
