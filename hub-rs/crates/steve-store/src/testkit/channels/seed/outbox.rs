use super::*;

pub fn remove_receipt(store: &Store, ev_seq: i64) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"DELETE FROM outbox_receipts WHERE event_seq=?1"#,
            params![ev_seq],
        )?;
        Ok(())
    })
}
