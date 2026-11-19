//! Shared integration fixture: every normal teardown audits the source/queue/projection contract.
#[path = "../../../steve-store-sqlite/tests/digest_audit.rs"]
mod digest_audit;

pub struct TestEnv(steve_modules::testkit::TestEnv);
impl TestEnv {
    pub fn new(register: impl FnOnce(&mut steve_modules::Registry)) -> Self {
        Self(steve_modules::testkit::TestEnv::new(register))
    }
}
impl std::ops::Deref for TestEnv {
    type Target = steve_modules::testkit::TestEnv;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for TestEnv {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for TestEnv {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            digest_audit::check(self.0.store());
        }
    }
}

// Raw SQL in these integration fixtures used to inherit migration 112's triggers. Maintain
// only rows changed by the fixture write; this never repairs pre-existing production omissions.
#[allow(dead_code)]
pub fn write<T>(
    store: &steve_store::Store,
    write: impl FnOnce(&rusqlite::Connection) -> Result<T, steve_store::StoreError>,
) -> Result<T, steve_store::StoreError> {
    use std::collections::{BTreeMap, BTreeSet};
    use steve_store_sqlite::digest::{self, Scope, Source};
    type Rows = BTreeMap<(u8, String), (Scope, String, i64)>;
    fn rows(c: &rusqlite::Connection) -> rusqlite::Result<Rows> {
        c.prepare("SELECT 0,id,workspace_id,'space:' || space_id,status,created FROM chan_messages UNION ALL SELECT 1,id,workspace_id,'mail_thread:' || thread_id,status,date FROM mail_messages")?
            .query_map([],|r|Ok(((r.get(0)?,r.get(1)?),(Scope { workspace:r.get(2)?,conv:r.get(3)? },r.get(4)?,r.get(5)?))))?.collect()
    }
    fn receipts(c: &rusqlite::Connection) -> rusqlite::Result<BTreeSet<(String, String, String)>> {
        c.prepare("SELECT workspace_id,conv,message_id FROM digest_receipts")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect()
    }
    store.transaction(|tx| {
        let before = rows(tx)?;
        let old_receipts = receipts(tx)?;
        let result = write(tx)?;
        let after = rows(tx)?;
        let removed = before
            .iter()
            .filter(|(key, _)| !after.contains_key(*key))
            .map(|((_, id), (scope, _, _))| (scope.clone(), id.clone()));
        digest::remove_message(tx, removed)?;
        for ((kind, id), row) in &after {
            if before.get(&(*kind, id.clone())) != Some(row) {
                let source = if *kind == 0 {
                    Source::ChanMessage
                } else {
                    Source::MailMessage
                };
                digest::sync_message(
                    tx,
                    source,
                    id,
                    before.get(&(*kind, id.clone())).map(|r| &r.0),
                )?;
            }
        }
        for (ws, conv, id) in receipts(tx)?.difference(&old_receipts) {
            digest::dequeue_on_receipt(tx, ws, conv, id)?;
        }
        Ok(result)
    })
}
