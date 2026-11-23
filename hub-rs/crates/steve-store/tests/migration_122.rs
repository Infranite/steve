use rusqlite::Connection;
use steve_store::db;

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn preserves_owner_finish_and_discards_requests_without_a_delivery_manifest() {
    let c = Connection::open_in_memory().unwrap();
    for sql in &db::MIGRATIONS[..121] {
        c.execute_batch(sql).unwrap();
    }
    c.execute_batch(db::ITEM_LIFECYCLE).unwrap();
    c.execute_batch(
        r#"CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(121);
         CREATE TABLE schema_compat(min_reader INTEGER NOT NULL); INSERT INTO schema_compat VALUES(121);
         INSERT OR IGNORE INTO workspaces(id,name,created) VALUES('default','Test',0);
         INSERT INTO items(id,workspace_id,no,title,status,stage,phase,cycle,revision,closing,created,updated)
         VALUES
          ('owner','default',1,'owner','pause','work','closing',1,2,
            '{"kind":"complete","request":"owner","reason":"你标记了完成","policy":"human","accepted":true,"at":10}',1,2),
          ('automatic','default',2,'automatic','run','work','closing',1,3,
            '{"kind":"complete","policy":"automatic","accepted":true}',1,2),
          ('cancel','default',3,'cancel','pause','work','closing',2,4,
            '{"kind":"cancel"}',1,2);
         INSERT INTO items(id,workspace_id,no,title,status,stage,phase,outcome,created,updated)
         VALUES ('past','default',4,'past','done','work','closed','completed',1,2);
         INSERT INTO item_cycles (item_id,cycle,revision,outcome,closed)
         VALUES ('past',1,1,'completed',2);
         INSERT INTO item_operations(id,item_id,cycle,revision,kind,status,required,created,updated)
         VALUES ('pending','automatic',1,3,'release','pending',1,1,2),
                ('unknown','automatic',1,3,'release','unknown',1,1,2);"#
    ).unwrap();

    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(count(&c, "SELECT COUNT(*) FROM items"), 4);
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM items WHERE phase='closed' AND outcome='completed'"
        ),
        2
    );
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM item_cycles WHERE item_id='owner' AND acceptance='owner'"
        ),
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM items WHERE id='automatic' AND phase='active' AND closing IS NULL"
        ),
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM items WHERE id='cancel' AND phase='closing' AND cycle=2"
        ),
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM item_operations WHERE (id='pending' AND status='waived') OR (id='unknown' AND status='unknown')"
        ),
        2
    );
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('item_would_do','item_judgements','item_judge_candidates')"
        ),
        0
    );
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM pragma_foreign_key_check"),
        0
    );
}
