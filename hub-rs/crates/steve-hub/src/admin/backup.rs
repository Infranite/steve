use crate::backup::{self, Purpose, SnapshotId};
use anyhow::{Context, Result, bail};
use std::path::Path;

pub(crate) fn reason(args: &[String]) -> Result<Purpose> {
    match args
        .iter()
        .map(String::as_str)
        .filter(|arg| !matches!(*arg, "--json" | "--offline"))
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["backup"] | ["backup", "--reason", "manual"] => Ok(Purpose::Manual),
        ["backup", "--reason", "daily"] => Ok(Purpose::Daily),
        ["backup", "--reason", "diagnostic"] => Ok(Purpose::Diagnostic),
        ["backup", "--reason", "pre-upgrade"] => Ok(Purpose::PreUpgrade),
        ["backup", "--reason", "pre-destructive"] => Ok(Purpose::PreDestructive),
        _ => bail!(
            "usage: steve-hub --admin backup [--reason daily|manual|pre-upgrade|pre-destructive|diagnostic]"
        ),
    }
}

pub(crate) fn command(home: &Path, db: &Path, args: &[String]) -> Result<String> {
    Ok(serde_json::to_string_pretty(&backup::create(
        home,
        db,
        reason(args)?,
    )?)?)
}

pub(crate) fn restore(home: &Path, db: &Path, args: &[String]) -> Result<String> {
    let id = args
        .get(1)
        .context("restore needs a full snapshot ID")?
        .clone();
    let id = SnapshotId::try_from(id).map_err(anyhow::Error::msg)?;
    match args
        .get(2..)
        .map(|args| args.iter().map(String::as_str).collect::<Vec<_>>())
        .as_deref()
    {
        Some(["--database-only"]) => {
            backup::restore_database(home, db, &id)?;
            Ok(format!("{id}\n"))
        }
        Some(["--to", parent]) => {
            let path = backup::restore_to(home, &id, Path::new(parent))?;
            Ok(format!("{}\n", path.display()))
        }
        _ => bail!(
            "usage: steve-hub --admin restore SNAPSHOT_ID (--database-only | --to PARENT_DIRECTORY)"
        ),
    }
}
