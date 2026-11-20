#![allow(dead_code)]
pub struct Audit(pub steve_store::Store);
pub fn check(store: &steve_store::Store) {
    store
        .reader(|c| {
            let tx = c.unchecked_transaction()?;
            let workspaces = tx
                .prepare(&format!("SELECT workspace_id FROM {} UNION SELECT workspace_id FROM {} UNION SELECT workspace_id FROM digest_pending UNION SELECT workspace_id FROM digest_pending_convs", steve_store_sqlite::digest::Source::ChanMessage.shape().table, steve_store_sqlite::digest::Source::MailMessage.shape().table))?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for ws in workspaces {
                let mismatches = steve_store_sqlite::digest::verify_pending(&tx, &ws)?;
                assert!(
                    mismatches.is_empty(),
                    "digest pending mismatch in workspace {ws}: {mismatches:?}"
                );
            }
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
}
impl Drop for Audit {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            check(&self.0);
        }
    }
}
