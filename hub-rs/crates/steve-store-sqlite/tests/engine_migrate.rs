use rusqlite::Connection;
use steve_store_sqlite::migrate::{self, Migration, MigrationSet};
use steve_types::Code;

#[test]
fn embedded_locks_match_and_changed_sql_or_lock_is_rejected_before_sql() {
    migrate::verify_locks().unwrap();
    for set in migrate::CATALOG {
        let mut changed = set.migrations.to_vec();
        changed[0].sql = "CREATE TABLE unexpected(id);";
        assert_eq!(
            migrate::verify_lock(&changed, set.lock).unwrap_err().code,
            Code::Incompatible
        );
        assert!(migrate::verify_lock(set.migrations, "wrong LOCK").is_err());
    }
    let mut c = Connection::open_in_memory().unwrap();
    let changed = MigrationSet {
        domain: "core",
        migrations: &[Migration {
            version: 1,
            name: "0001.sql",
            sql: "CREATE TABLE unexpected(id);",
        }],
        lock: "bad",
    };
    assert_eq!(
        migrate::apply_set(&mut c, &changed).unwrap_err().code,
        Code::Incompatible
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert!(c.is_autocommit());
}

#[test]
fn pending_domain_migrations_commit_together_and_failure_rolls_everything_back() {
    let mut c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE domain_schema(domain TEXT PRIMARY KEY,version INTEGER); INSERT INTO domain_schema VALUES('core',0);").unwrap();
    let broken = MigrationSet {
        domain: "core",
        migrations: &[
            Migration {
                version: 1,
                name: "0001_step.sql",
                sql: "CREATE TABLE engine_step1(id INTEGER);",
            },
            Migration {
                version: 2,
                name: "0002_step.sql",
                sql: "INSERT INTO no_such_table VALUES(1);",
            },
        ],
        lock: "0001_step.sql 3a4d1171c8b54b293fec770772baf862518d2941307cd38a1c1eed3dca5b3627\n0002_step.sql 028d9caf359e6724b8f4b941495bf13cd34c0ce6d416e9c2f4251b4b93919686\n",
    };
    assert!(migrate::apply_set(&mut c, &broken).is_err());
    assert!(c.is_autocommit());
    assert_eq!(
        c.query_row(
            "SELECT version FROM domain_schema WHERE domain='core'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='engine_step1'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    let valid = MigrationSet {
        domain: "core",
        migrations: &[
            Migration {
                version: 1,
                name: "0001_step.sql",
                sql: "CREATE TABLE engine_step1(id INTEGER);",
            },
            Migration {
                version: 2,
                name: "0002_step.sql",
                sql: "CREATE TABLE engine_step2(id INTEGER);",
            },
        ],
        lock: "0001_step.sql 3a4d1171c8b54b293fec770772baf862518d2941307cd38a1c1eed3dca5b3627\n0002_step.sql 359000a0a6789b61c67f353d77134ffc518af3071bd5ff3a3d84c44eac7dfa84\n",
    };
    migrate::apply_set(&mut c, &valid).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT version FROM domain_schema WHERE domain='core'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        2
    );
    c.execute_batch("CREATE TRIGGER engine_version_guard BEFORE UPDATE ON domain_schema BEGIN SELECT RAISE(ABORT,'version rewritten'); END;").unwrap();
    migrate::apply_set(&mut c, &valid).unwrap();
}
