//! `steve-hub --stats-snapshot --db <file> [--workspace <id>] [--from <day> --to <day>]`: the
//! statistics' workflow section, the quality figures and the throughput of one database file, printed as
//! JSON, without a Hub (#242). It is how a baseline is taken from a copy of an installation's database.
//!
//! The source is always opened read-only. Legacy layouts must first be converted
//! by starting the new Hub. A supported current layout with pending migrations is
//! copied into a scratch canonical data directory and only that copy is migrated
//! under its own storage.lock. JSON distinguishes source_state from queried_state.
//! Scratch copies are removed on success and failure. Days use the workspace's
//! stats.tz; the default range is its last 30 days.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde_json::json;
use steve_stats::metrics::{self, Query};
use steve_stats::views;
use steve_store_sqlite::{
    compat::{StorageCapabilities, StorageState},
    layout::{CORE_FILE, Layout, domain_name},
    open, pragmas,
};

#[derive(Debug, Default, PartialEq)]
struct Args {
    db: PathBuf,
    workspace: Option<String>,
    from: Option<String>,
    to: Option<String>,
    scratch: Option<PathBuf>,
}

const USAGE: &str = "usage: steve-hub --stats-snapshot --db <file> [--workspace <id>] [--from YYYY-MM-DD --to YYYY-MM-DD] [--scratch <dir>]";

fn args(a: &[String]) -> Result<Args> {
    let mut out = Args::default();
    let mut db = None;
    let mut it = a.iter();
    while let Some(k) = it.next() {
        let v = it
            .next()
            .with_context(|| format!("{k} needs a value\n{USAGE}"))?;
        match k.as_str() {
            "--db" => db = Some(PathBuf::from(v)),
            "--workspace" => out.workspace = Some(v.clone()),
            "--from" => out.from = Some(v.clone()),
            "--to" => out.to = Some(v.clone()),
            "--scratch" => out.scratch = Some(PathBuf::from(v)),
            _ => bail!("unknown option {k}\n{USAGE}"),
        }
    }
    out.db = db.with_context(|| format!("--db is required\n{USAGE}"))?;
    if out.from.is_some() != out.to.is_some() {
        bail!("--from and --to go together\n{USAGE}");
    }
    Ok(out)
}

fn read_only(path: &Path) -> Result<Connection> {
    let c = open::open_readonly(path)
        .with_context(|| format!("cannot open {} read-only", path.display()))?;
    c.pragma_update(None, "query_only", true)?;
    Ok(c)
}

struct QuerySource {
    // The connection must close before removing its scratch directory.
    connection: Connection,
    _scratch: Option<tempfile::TempDir>,
    source_state: StorageState,
    queried_state: StorageState,
}

fn current_layout(state: &StorageState) -> Result<&Layout> {
    match state {
        StorageState::Current { layout, .. } => Ok(layout),
        StorageState::Legacy { .. } => {
            bail!("legacy storage: first start the new Hub to convert this database")
        }
        StorageState::Empty => bail!("storage is not initialized: first start the Hub"),
    }
}

fn migrate_copy(dir: &Path) -> Result<()> {
    // Engine owns this scratch directory's storage.lock throughout migration.
    // It never opens or locks the source data directory.
    let engine = open::open_runtime(dir).context("cannot migrate the scratch copy")?;
    drop(engine);
    Ok(())
}

// --db can name a renamed read-only snapshot. The manifest still uses the
// canonical core name; resolve that one entry to the supplied source path.
fn verify_snapshot_files(core: &Path, layout: &Layout) -> Result<()> {
    if core.file_name().is_some_and(|name| name == CORE_FILE) {
        layout.verify_files(core.parent().context("database has no parent")?)?;
        return Ok(());
    }
    layout.validate()?;
    let dir = core.parent().context("database has no parent")?;
    for (file, id) in &layout.files {
        let path = if file == CORE_FILE {
            core.to_path_buf()
        } else {
            dir.join(file)
        };
        let c = read_only(&path)?;
        let meta = |domain, key| steve_store_sqlite::layout::meta(&c, domain, key);
        if meta("core", "file_id")? != *id
            || meta("core", "layout")? != layout.generation.to_string()
            || meta("core", "min_reader")?.parse::<u32>()? > layout.generation
            || meta("core", "store_generation")? != layout.store_generation
        {
            bail!("storage snapshot file identity differs from its manifest");
        }
        for (domain, placed) in &layout.domains {
            if placed == file
                && (meta(domain_name(*domain), "file")? != *file
                    || meta(domain_name(*domain), "file_id")? != *id)
            {
                bail!("storage snapshot domain placement differs from its manifest");
            }
        }
    }
    Ok(())
}

fn query_source(
    a: &Args,
    capabilities: &StorageCapabilities,
    migrate: impl FnOnce(&Path) -> Result<()>,
) -> Result<QuerySource> {
    let source = read_only(&a.db)?;
    let source_state = open::inspect(&source)?;
    let layout = current_layout(&source_state)?;
    capabilities.can_open(&source_state, false)?;
    let source_dir = a.db.parent().context("database has no parent directory")?;
    verify_snapshot_files(&a.db, layout)?;
    let (connection, scratch) = if capabilities.can_open(&source_state, true).is_ok() {
        (source, None)
    } else {
        let dir = match &a.scratch {
            Some(d) => tempfile::tempdir_in(d),
            None => tempfile::tempdir(),
        }
        .context("cannot make a scratch directory for the migrated copy")?;
        for file in layout.files.keys() {
            let source_path = if file == CORE_FILE {
                a.db.clone()
            } else {
                source_dir.join(file)
            };
            let copied = steve_store_sqlite::backup::offline::backup_file(
                &source_path,
                dir.path(),
                &steve_store_sqlite::backup::offline::CopyOptions::default(),
                &|_| {},
                &steve_store_sqlite::backup::CancelFlag::default(),
            )?;
            if file == CORE_FILE && copied.path.file_name().is_some_and(|n| n != CORE_FILE) {
                // This verified copy is owned by our TempDir. TempPath preserves
                // cleanup on rename failure and refuses to clobber a peer file.
                tempfile::TempPath::try_from_path(copied.path)?
                    .persist_noclobber(dir.path().join(CORE_FILE))?;
            }
        }
        // A concurrent schema change must not relabel a different copy as the
        // state inspected above. Ordinary data changes do not affect this check.
        if open::inspect(&read_only(&dir.path().join(CORE_FILE))?)? != source_state {
            bail!("source storage changed while copying; retry the snapshot");
        }
        migrate(dir.path())?;
        (read_only(&dir.path().join(CORE_FILE))?, Some(dir))
    };
    let queried_state = open::inspect(&connection)?;
    capabilities.can_open(&queried_state, true)?;
    let query_dir = scratch.as_ref().map_or(source_dir, |d| d.path());
    let queried_layout = current_layout(&queried_state)?;
    let query_core = scratch
        .as_ref()
        .map_or_else(|| a.db.clone(), |d| d.path().join(CORE_FILE));
    verify_snapshot_files(&query_core, queried_layout)?;
    pragmas::attach_readonly(&connection, query_dir, queried_layout, CORE_FILE)?;
    Ok(QuerySource {
        connection,
        _scratch: scratch,
        source_state,
        queried_state,
    })
}

fn state_json(state: &StorageState) -> serde_json::Value {
    match state {
        StorageState::Current {
            layout,
            domains,
            contract_schema,
        } => {
            let files: std::collections::BTreeMap<_, _> = layout
                .domains
                .iter()
                .map(|(domain, file)| (domain_name(*domain), file))
                .collect();
            json!({"kind": "current", "layout": layout.generation,
                "min_reader": layout.min_reader, "contract_schema": contract_schema,
                "domains": domains, "domain_files": files,
                "store_generation": layout.store_generation, "file_ids": layout.files})
        }
        StorageState::Legacy { schema_version } => {
            json!({"kind":"legacy", "schema_version":schema_version})
        }
        StorageState::Empty => json!({"kind":"empty"}),
    }
}

/// The snapshot of `a`, as JSON.
fn snapshot(a: &Args) -> Result<serde_json::Value> {
    if !a.db.is_file() {
        bail!("no database at {}", a.db.display());
    }
    let prepared = query_source(a, &StorageCapabilities::compiled(), migrate_copy)?;
    let c = &prepared.connection;
    let ws = match &a.workspace {
        Some(w) => w.clone(),
        None => {
            let all: Vec<String> = c
                .prepare("SELECT id FROM workspaces ORDER BY created, id")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            match all.as_slice() {
                [one] => one.clone(),
                [] => bail!("the database has no workspace"),
                many => bail!(
                    "several workspaces, pick one with --workspace: {}",
                    many.join(", ")
                ),
            }
        }
    };
    let today = views::today(c, &ws, steve_store::now())?;
    let (from, to) = match (&a.from, &a.to) {
        (Some(f), Some(t)) => views::range("custom", Some(f), Some(t), &today),
        _ => views::range("30d", None, None, &today),
    }
    .map_err(|bad| anyhow::anyhow!("bad {bad}: days are YYYY-MM-DD, at most a year apart"))?;
    let q = Query {
        workspace_id: ws.clone(),
        from,
        to,
        ..Query::default()
    };
    let s = metrics::summary(c, &q)?.context("bad range")?;
    Ok(json!({
        "workspace": ws,
        "source_state": state_json(&prepared.source_state),
        "queried_state": state_json(&prepared.queried_state),
        "range": {"from": s.from, "to": s.to, "tz": s.tz},
        "workflow": s.workflow,
        "quality": s.quality,
        "throughput": s.throughput,
    }))
}

/// The command; its exit code.
pub fn main(a: &[String]) -> i32 {
    match args(a).and_then(|a| snapshot(&a)) {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            0
        }
        Err(e) => {
            eprintln!("steve-hub --stats-snapshot: {e:#}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn options() {
        assert_eq!(
            args(&s(&[
                "--db",
                "/d",
                "--from",
                "2026-11-01",
                "--to",
                "2026-11-17"
            ]))
            .unwrap(),
            Args {
                db: "/d".into(),
                from: Some("2026-11-01".into()),
                to: Some("2026-11-17".into()),
                ..Args::default()
            }
        );
        assert!(args(&s(&[])).is_err(), "--db is required");
        assert!(args(&s(&["--db", "/d", "--from", "2026-11-01"])).is_err());
        assert!(args(&s(&["--db", "/d", "--nope", "x"])).is_err());
        assert!(args(&s(&["--db"])).is_err());
    }

    fn current_fixture(dir: &Path) -> PathBuf {
        let path = dir.join(CORE_FILE);
        let mut c = Connection::open(&path).unwrap();
        steve_store_sqlite::migrate::initialize(&mut c, &Layout::single_file()).unwrap();
        c.execute_batch(
            "INSERT INTO workspaces(id,name,created) VALUES ('w','w',1);
             INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','w',1,'t','run','执行',1,1);",
        ).unwrap();
        c.execute("INSERT INTO item_marks(workspace_id,item_id,ts,kind,to_v,actor) VALUES ('w','i1',?,'invalidate','plan','steve')", [steve_store::now()]).unwrap();
        path
    }

    #[test]
    fn current_snapshot_queries_readonly_without_source_storage_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = current_fixture(dir.path());
        let before = std::fs::read(&path).unwrap();
        // A running Hub may own the source lock; a read-only snapshot needs none.
        let lock = std::fs::File::create(dir.path().join("storage.lock")).unwrap();
        lock.try_lock().unwrap();
        let v = snapshot(&Args {
            db: path.clone(),
            ..Args::default()
        })
        .unwrap();
        assert_eq!(v["workspace"], "w");
        assert_eq!(v["source_state"]["kind"], "current");
        assert_eq!(v["source_state"], v["queried_state"]);
        assert_eq!(v["workflow"]["invalidations"]["plan"], 1);
        assert_eq!(v["quality"]["unit_retries"], 0);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!path.with_extension("sqlite3-wal").exists());
    }

    #[test]
    fn readonly_snapshot_preserves_live_source_database_and_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = current_fixture(dir.path());
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        writer
            .execute("UPDATE workspaces SET name='in WAL' WHERE id='w'", [])
            .unwrap();
        let wal = path.with_extension("sqlite3-wal");
        let before = (std::fs::read(&path).unwrap(), std::fs::read(&wal).unwrap());
        let args = Args {
            db: path.clone(),
            ..Args::default()
        };
        let prepared = query_source(&args, &StorageCapabilities::compiled(), |_| {
            bail!("exact current must not migrate")
        })
        .unwrap();
        assert_eq!(
            prepared
                .connection
                .query_row("SELECT name FROM workspaces WHERE id='w'", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "in WAL"
        );
        assert!(
            prepared
                .connection
                .execute("UPDATE workspaces SET name='forbidden'", [])
                .is_err()
        );
        drop(prepared);
        assert_eq!(
            (std::fs::read(&path).unwrap(), std::fs::read(&wal).unwrap()),
            before
        );
        assert!(!dir.path().join("storage.lock").exists());
    }

    #[test]
    fn legacy_and_future_storage_are_refused_without_source_changes() {
        for legacy in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = if legacy {
                let path = dir.path().join(CORE_FILE);
                let c = Connection::open(&path).unwrap();
                c.execute_batch("CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(113);").unwrap();
                path
            } else {
                let path = current_fixture(dir.path());
                let c = Connection::open(&path).unwrap();
                c.execute(
                    "UPDATE domain_schema SET version=999 WHERE domain='comms'",
                    [],
                )
                .unwrap();
                path
            };
            let before = std::fs::read(&path).unwrap();
            let error = snapshot(&Args {
                db: path.clone(),
                scratch: Some(dir.path().into()),
                ..Args::default()
            })
            .unwrap_err();
            if legacy {
                assert!(error.to_string().contains("first start the new Hub"));
            }
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn pending_current_is_migrated_only_in_copy_and_reports_both_states() {
        use sha2::{Digest, Sha256};
        use steve_store_sqlite::migrate::{Migration, MigrationSet};
        let dir = tempfile::tempdir().unwrap();
        let path = current_fixture(dir.path());
        let before = std::fs::read(&path).unwrap();
        let mut capabilities = StorageCapabilities::compiled();
        // A renamed source must still become canonical only inside scratch.
        let renamed = dir.path().join("source-copy.sqlite3");
        std::fs::rename(&path, &renamed).unwrap();
        let path = renamed;
        // The compiled catalog currently has only v1. Extend the test candidate
        // with a real, hash-locked v2 migration to exercise the pending branch.
        capabilities.core.target = 2;
        let args = Args {
            db: path.clone(),
            scratch: Some(dir.path().into()),
            ..Args::default()
        };
        let prepared = query_source(&args, &capabilities, |copy| {
            assert_ne!(copy, dir.path());
            assert!(copy.join(CORE_FILE).is_file());
            // Exercise the production Engine lock/open path first, then use the
            // public migration runner with this fixture's extra catalog entry.
            migrate_copy(copy)?;
            let lock = std::fs::File::open(copy.join("storage.lock"))?;
            lock.try_lock()?;
            let mut c = Connection::open(copy.join(CORE_FILE))?;
            const MIGRATIONS: &[Migration] = &[
                Migration {
                    version: 1,
                    name: "0001_baseline.sql",
                    sql: steve_store_sqlite::schema::CORE,
                },
                Migration {
                    version: 2,
                    name: "0002.sql",
                    sql: "CREATE TABLE snapshot_copy_probe(id INTEGER);",
                },
            ];
            let hashes = MIGRATIONS
                .iter()
                .map(|m| {
                    format!(
                        "{} {}",
                        m.name,
                        Sha256::digest(m.sql.as_bytes())
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            steve_store_sqlite::migrate::apply_set(
                &mut c,
                &MigrationSet {
                    domain: "core",
                    migrations: MIGRATIONS,
                    lock: Box::leak(hashes.into_boxed_str()),
                },
            )?;
            Ok(())
        })
        .unwrap();
        assert_eq!(state_json(&prepared.source_state)["domains"]["core"], 1);
        assert_eq!(state_json(&prepared.queried_state)["domains"]["core"], 2);
        assert!(
            prepared
                .connection
                .execute("INSERT INTO snapshot_copy_probe VALUES(1)", [])
                .is_err()
        );
        let copy = prepared._scratch.as_ref().unwrap().path().to_path_buf();
        drop(prepared);
        assert!(!copy.exists());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // Failure must remove its partial scratch directory too.
        assert!(query_source(&args, &capabilities, |_| bail!("migration failed")).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
