//! A normal Store open must execute the migration, not merely leave a SQL file on disk.
use steve_store::testkit::SqliteFixtures;
#[test]
fn current_store_has_no_implicit_digest_membership_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        steve_store::Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher)
            .unwrap();
    store
        .fixture_write(|c| {
            let n: i64 = c.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND
                 (name LIKE 'digest_pending_%' OR name='digest_receipt_ack'
                  OR name LIKE 'digest_chan_messages_%' OR name LIKE 'digest_mail_messages_%')",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(
                n, 0,
                "migration 114 must be included in the old runner before explicit maintenance"
            );
            Ok(())
        })
        .unwrap();
}
