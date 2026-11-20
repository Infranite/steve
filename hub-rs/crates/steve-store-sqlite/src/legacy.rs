//! Frozen legacy schema replay, for conversion and baseline verification only.
//! It deliberately excludes connection pragmas, pools, diagnostics and background workers.

use rusqlite::{Connection, OptionalExtension, Result, Transaction, TransactionBehavior};

/// The supported archive records real migration numbers, not array offsets.
pub(crate) const MIGRATIONS: &[(i64, &str)] = &[
    (113, include_str!("../legacy/migrations/113.sql")),
    (114, include_str!("../legacy/migrations/114.sql")),
];
pub(crate) const SCHEMA_VERSION: i64 = MIGRATIONS[MIGRATIONS.len() - 1].0;

/// Conversion's sole runner. Validation, current metadata and credential cleanup
/// execute in its final hook before the same transaction commits.
pub(crate) fn migrate_to_head_with<E: From<rusqlite::Error>>(
    conn: &Connection,
    check: impl FnOnce(&Connection) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    let cur = version(conn)?;
    if !(crate::compat::LEGACY_MIN..=SCHEMA_VERSION).contains(&cur) {
        return Err(rusqlite::Error::InvalidParameterName(
            "unsupported legacy conversion input".into(),
        )
        .into());
    }
    migrate(conn, cur, check)
}

/// Read and validate the legacy version and minimum reader without any writes.
pub fn version(conn: &Connection) -> Result<i64> {
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='schema_version')",
        [],
        |r| r.get(0),
    )?;
    let (count, cur): (i64, i64) = if present {
        conn.query_row(
            "SELECT count(*), coalesce(min(v),0) FROM schema_version",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?
    } else {
        (0, 0)
    };
    if count > 1
        || !(0..=SCHEMA_VERSION).contains(&cur)
        || minimum_reader(conn, cur)? > SCHEMA_VERSION
    {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "cannot migrate legacy schema {cur} with head {SCHEMA_VERSION}"
        )));
    }
    Ok(cur)
}

fn minimum_reader(c: &Connection, fallback: i64) -> Result<i64> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='schema_compat')",
        [],
        |r| r.get(0),
    )?;
    if present {
        Ok(
            c.query_row("SELECT min_reader FROM schema_compat", [], |r| r.get(0))
                .optional()?
                .unwrap_or(fallback),
        )
    } else {
        Ok(fallback)
    }
}

fn migrate<E: From<rusqlite::Error>>(
    c: &Connection,
    cur: i64,
    check: impl FnOnce(&Connection) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    if !(0..=SCHEMA_VERSION).contains(&cur) {
        return Err(rusqlite::Error::InvalidParameterName(
            "unsupported legacy conversion input".into(),
        )
        .into());
    }
    // The caller must not already own a transaction: foreign_keys cannot change within one.
    if !c.is_autocommit() {
        return Err(rusqlite::Error::InvalidParameterName(
            "legacy migration requires autocommit".into(),
        )
        .into());
    }
    let fks: i64 = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if cur != SCHEMA_VERSION {
        c.pragma_update(None, "foreign_keys", "OFF")?;
    }
    let ran = run_pending(c, cur, check);
    let restored = c.pragma_update(None, "foreign_keys", fks);
    ran?;
    restored.map_err(Into::into)
}

fn run_pending<E: From<rusqlite::Error>>(
    c: &Connection,
    cur: i64,
    check: impl FnOnce(&Connection) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    let tx = Transaction::new_unchecked(c, TransactionBehavior::Immediate)?;
    // Metadata belongs to the same transaction as migrations, including on an empty DB.
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (v INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS schema_compat (min_reader INTEGER NOT NULL);",
    )?;
    for (version, sql) in MIGRATIONS.iter().filter(|(version, _)| *version > cur) {
        debug_assert!(*version <= SCHEMA_VERSION);
        tx.execute_batch(sql)?;
    }
    let broken: Vec<String> = if cur == SCHEMA_VERSION {
        Vec::new()
    } else {
        tx.prepare("SELECT \"table\" || ' -> ' || parent FROM pragma_foreign_key_check")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    if !broken.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "foreign keys broken after migrating to v{SCHEMA_VERSION}: {}",
            broken.join(", ")
        ))
        .into());
    }
    tx.execute("UPDATE schema_version SET v = ?", [SCHEMA_VERSION])?;
    // every migration needs a build that knows it (letting older builds use one would also need a record
    // of what they wrote since, which nothing keeps); the stamp only ever rises
    let stamp = minimum_reader(&tx, cur)?.max(SCHEMA_VERSION);
    tx.execute("DELETE FROM schema_compat", [])?;
    tx.execute("INSERT INTO schema_compat (min_reader) VALUES (?)", [stamp])?;
    // Final hook sees the stamped head and may replace legacy metadata with
    // the current contract/fence. Never touch legacy tables after this hook.
    // A hook error still rolls back replay and stamps in this same transaction.
    check(&tx)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod final_hook_tests {
    use super::*;
    #[test]
    fn supported_archive_matches_its_frozen_lock() {
        use sha2::{Digest, Sha256};
        let locked: Vec<_> = include_str!("../legacy/migrations/LOCK").lines().collect();
        assert_eq!(locked.len(), MIGRATIONS.len());
        assert_eq!(MIGRATIONS.first().unwrap().0, crate::compat::LEGACY_MIN);
        for ((version, sql), line) in MIGRATIONS.iter().zip(locked) {
            let hash: String = Sha256::digest(sql.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(line, format!("{version:03}.sql {hash}"));
        }
        assert_eq!(SCHEMA_VERSION, 114);
    }
    #[test]
    fn final_hook_sees_replay_stamps_and_rolls_back_trigger_drop_with_metadata() {
        for reject in [false, true] {
            let c = Connection::open_in_memory().unwrap();
            c.execute_batch(include_str!("../legacy/113_fixture.sql"))
                .unwrap();
            let result = migrate_to_head_with(&c, |tx| {
                assert_eq!(version(tx).unwrap(), SCHEMA_VERSION);
                assert_eq!(minimum_reader(tx, 0).unwrap(), SCHEMA_VERSION);
                crate::compat::stamp_reader_fence(tx).unwrap();
                if reject {
                    Err(rusqlite::Error::InvalidQuery)
                } else {
                    Ok(())
                }
            });
            assert_eq!(result.is_err(), reject);
            assert!(c.is_autocommit());
            let triggers: u32 = c
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name='digest_pending_insert'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(triggers, u32::from(reject));
            if reject {
                assert_eq!(version(&c).unwrap(), 113);
                assert_eq!(minimum_reader(&c, 0).unwrap(), 113);
            } else {
                assert_eq!(
                    minimum_reader(&c, 0).unwrap(),
                    crate::compat::LEGACY_READER_FENCE
                );
                assert!(c.prepare("SELECT v FROM schema_version").is_err());
            }
        }
    }
}
