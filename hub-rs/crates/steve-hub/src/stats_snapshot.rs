//! `steve-hub --stats-snapshot --db <file> [--workspace <id>] [--from <day> --to <day>]`: the
//! statistics' workflow section, the quality figures and the throughput of one database file, printed as
//! JSON, without a Hub (#242). It is how a baseline is taken from a copy of an installation's database.
//!
//! The file is only read (opened read-only, `query_only`). One with an older schema than this build is
//! first copied (SQLite's online backup) into a scratch directory (`--scratch`, else the system's
//! temporary directory) and the copy migrated, so it reads the same as an upgraded Hub's; the copy is
//! removed afterwards. Days are the workspace's (`stats.tz`); the default range is its last 30 days.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use steve_stats::metrics::{self, Query};
use steve_stats::views;
use steve_store::db;

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
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("cannot open {} read-only", path.display()))?;
    c.pragma_update(None, "query_only", true)?;
    Ok(c)
}

/// How often, and how far apart, a copy is tried again while a running Hub holds the database busy.
const COPY_TRIES: u32 = 100;
const COPY_PAUSE: std::time::Duration = std::time::Duration::from_millis(100);

/// Every page of `src` into `dst` in one step, from one snapshot: a copy taken in pieces starts over
/// whenever another connection (a running Hub) commits between two of them.
fn copy_all(src: &Connection, dst: &mut Connection) -> Result<()> {
    let backup = rusqlite::backup::Backup::new(src, dst)?;
    for _ in 0..COPY_TRIES {
        match backup.step(-1)? {
            rusqlite::backup::StepResult::Done => return Ok(()),
            _ => std::thread::sleep(COPY_PAUSE),
        }
    }
    bail!("the database stayed busy; no copy was made")
}

/// The snapshot of `a`, as JSON.
fn snapshot(a: &Args) -> Result<serde_json::Value> {
    if !a.db.is_file() {
        bail!("no database at {}", a.db.display());
    }
    let src = read_only(&a.db)?;
    let schema = db::compat(&src)?.schema;
    if schema > db::SCHEMA_VERSION {
        bail!(
            "the database has schema {schema}, newer than this build's {}: use the Hub release that wrote it",
            db::SCHEMA_VERSION
        );
    }
    // an older schema reads as an upgraded Hub's would: from a migrated copy, never the file itself
    let mut scratch = None;
    let c = if schema < db::SCHEMA_VERSION {
        let dir = match &a.scratch {
            Some(d) => tempfile::tempdir_in(d),
            None => tempfile::tempdir(),
        }
        .context("cannot make a scratch directory for the migrated copy")?;
        let copy = dir.path().join("snapshot.sqlite3");
        let mut dst = Connection::open(&copy)?;
        copy_all(&src, &mut dst)?;
        db::migrate(&dst).context("cannot migrate the copy")?;
        scratch = Some(dir);
        dst
    } else {
        src
    };
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
    let today = views::today(&c, &ws, steve_store::now())?;
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
    let s = metrics::summary(&c, &q)?.context("bad range")?;
    drop(c);
    drop(scratch);
    Ok(json!({
        "workspace": ws,
        "schema": schema,
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

    #[test]
    fn an_older_database_is_read_from_a_migrated_copy_and_left_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.sqlite3");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(88);",
            )
            .unwrap();
            for m in &db::MIGRATIONS[..88] {
                c.execute_batch(m).unwrap();
            }
            c.execute_batch(
                "INSERT INTO workspaces(id,name,created) VALUES ('w','w',1);
                 INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','w',1,'t','run','执行',1,1);",
            )
            .unwrap();
            // a closing invalidated today, known only from its timeline line before migration 89
            c.execute(
                "INSERT INTO timeline(item_id,ts,actor,kind,text) VALUES ('i1',?,'steve','sys','收尾作废：计划里又有没做完的步骤')",
                [steve_store::now()],
            )
            .unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let v = snapshot(&Args {
            db: path.clone(),
            scratch: Some(dir.path().into()),
            ..Args::default()
        })
        .unwrap();
        assert_eq!(v["workspace"], "w");
        assert_eq!(v["schema"], 88);
        assert_eq!(v["workflow"]["invalidations"]["plan"], 1);
        assert_eq!(v["quality"]["unit_retries"], 0);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "the file is only read"
        );
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "the scratch copy is removed");
    }
}
