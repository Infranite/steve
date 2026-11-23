//! `steve-hub --stats-snapshot --db <file> [--workspace <id>] [--from <day> --to <day>]`: the
//! statistics' workflow section, the quality figures and the throughput of one database file, printed as
//! JSON, without a Hub (#242). It is how a baseline is taken from a copy of an installation's database.
//!
//! The file is only read (opened read-only, `query_only`). One with an older schema than this build is
//! first copied (SQLite's online backup) into a scratch directory (`--scratch`, else the system's
//! temporary directory) and the copy migrated, so it reads the same as an upgraded Hub's; the copy is
//! removed afterwards. Days are the workspace's (`stats.tz`); the default range is its last 30 days.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::json;
use steve_stats::metrics::{self, Query};
use steve_stats::views;
use steve_store::{R, Store, Workspaces, db};

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

/// The snapshot of `a`, as JSON.
fn snapshot(a: &Args) -> Result<serde_json::Value> {
    if !a.db.is_file() {
        bail!("no database at {}", a.db.display());
    }
    let schema = db::inspect_schema(&a.db)?.schema;
    if schema > db::SCHEMA_VERSION {
        bail!(
            "the database has schema {schema}, newer than this build's {}: use the Hub release that wrote it",
            db::SCHEMA_VERSION
        );
    }
    // an older schema reads as an upgraded Hub's would: from a migrated copy, never the file itself
    let mut scratch = None;
    let path = if schema < db::SCHEMA_VERSION {
        let dir = match &a.scratch {
            Some(d) => tempfile::tempdir_in(d),
            None => tempfile::tempdir(),
        }
        .context("cannot make a scratch directory for the migrated copy")?;
        let copy = steve_store_sqlite::backup::offline::backup_file(
            &a.db,
            dir.path(),
            &steve_store_sqlite::backup::offline::CopyOptions::default(),
            &|_| {},
            &steve_store_sqlite::backup::CancelFlag::default(),
        )?;
        db::migrate_file(&copy.path).context("cannot migrate the copy")?;
        scratch = Some(dir);
        copy.path
    } else {
        a.db.clone()
    };
    let result = Store::read_file(&path, |r| Ok(report(r, a, schema)))?;
    drop(scratch);
    result
}

fn report(r: &mut R<'_>, a: &Args, schema: i64) -> Result<serde_json::Value> {
    let ws = match &a.workspace {
        Some(w) => w.clone(),
        None => {
            let all = Workspaces(r).ids()?;
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
    let today = views::today(r, &ws, steve_store::now())?;
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
    let s = metrics::summary(r, &q)?.context("bad range")?;
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
        steve_store::testkit::hub::snapshot_before_invalidations(&path, steve_store::now())
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let v = snapshot(&Args {
            db: path.clone(),
            scratch: Some(dir.path().into()),
            ..Args::default()
        })
        .unwrap();
        assert_eq!(v["workspace"], "w");
        assert_eq!(v["schema"], 91);
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
