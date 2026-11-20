//! Messages and seen_by masks never grant personal-send authority by themselves.
use rusqlite::Connection;
use steve_store::db;

#[test]
fn messages_and_seen_masks_do_not_authorize_personal_sends_on_reopen() {
    let c = Connection::open_in_memory().unwrap();
    db::migrate(&c).unwrap();
    // Opaque legacy provider IDs are sufficient: no linked account or recipient is inferred.
    c.execute_batch(
        "INSERT INTO workspaces(id,name,created) VALUES('legacy','Legacy',1);
        INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,created,updated)
        VALUES('old-chat','legacy','slack','group','opaque-provider-chat','Legacy chat',1,1);
        INSERT INTO chan_messages(id,workspace_id,space_id,direction,text,status,created,seen_by)
        VALUES('old-in','legacy','old-chat','in','/bot old command','received',1,3),
              ('old-out','legacy','old-chat','out','old draft','draft',1,3);",
    )
    .unwrap();
    for _ in 0..2 {
        db::migrate(&c).unwrap();
        for table in ["chan_trigger_receipts"] {
            let n: i64 = c
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "no inferred authorization in {table}");
        }
        let text: String = c
            .query_row(
                "SELECT text FROM chan_messages WHERE id='old-in'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(text, "/bot old command");
    }
}
