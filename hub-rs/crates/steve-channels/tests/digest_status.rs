#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use steve_store_sqlite::digest::{self, Source};

#[test]
fn conditional_status_updates_only_synchronize_matched_rows() {
    let env = TestEnv::new(|_| {});
    env.store().transaction(|tx| {
        tx.execute("INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,created,updated) VALUES('s','default','test','group','s','S',1,1)",[])?;
        tx.execute("INSERT INTO chan_messages(id,workspace_id,space_id,direction,status,created) VALUES('a','default','s','in','received',1)",[])?;
        digest::sync_message(tx,Source::ChanMessage,"a",None)?;
        let position=tx.query_row("SELECT position FROM digest_pending WHERE message_id='a'",[],|r|r.get::<_,i64>(0))?;
        assert_eq!(steve_channels::messages::set_status(tx,"a","UPDATE chan_messages SET status='pending' WHERE id='a' AND status='draft'",[])?,0);
        assert_eq!(tx.query_row("SELECT position FROM digest_pending WHERE message_id='a'",[],|r|r.get::<_,i64>(0))?,position);
        assert_eq!(steve_channels::messages::set_status(tx,"a","UPDATE chan_messages SET status='sent' WHERE id='a'",[])?,1);
        assert_eq!(tx.query_row("SELECT position FROM digest_pending WHERE message_id='a'",[],|r|r.get::<_,i64>(0))?,position);
        assert_eq!(steve_channels::messages::set_status(tx,"a","UPDATE chan_messages SET status='pending' WHERE id='a'",[])?,1);
        assert_eq!(tx.query_row("SELECT count(*) FROM digest_pending WHERE message_id='a'",[],|r|r.get::<_,i64>(0))?,0);
        Ok::<_,steve_store::StoreError>(())
    }).unwrap();
}

#[test]
fn fixture_sql_does_not_repair_an_earlier_missing_production_call() {
    let env = TestEnv::new(|_| {});
    env.store().transaction(|tx| {
        tx.execute("INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,created,updated) VALUES('s','default','test','group','s','S',1,1)",[])?;
        tx.execute("INSERT INTO chan_messages(id,workspace_id,space_id,direction,status,created) VALUES('omitted','default','s','in','received',1)",[])?;
        Ok::<_,steve_store::StoreError>(())
    }).unwrap();
    digest_env::write(env.store(), |c| {
        c.execute(
            "UPDATE chan_messages SET text='fixture body change' WHERE id='omitted'",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    env.store()
        .transaction(|tx| {
            assert_eq!(
                digest::verify_pending(tx, "default")?[0].kind,
                digest::MismatchKind::Missing
            );
            digest::sync_message(tx, Source::ChanMessage, "omitted", None)?;
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
}
