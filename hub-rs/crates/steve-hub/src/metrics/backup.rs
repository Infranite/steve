//! Backup observations made outside the Hub survive until its recorder can ingest them.
use serde::{Deserialize, Serialize};
use std::{fs, io::ErrorKind, os::unix::fs::DirBuilderExt, path::Path, time::Duration};
use steve_types::backup::BackupPurpose;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Operation {
    Backup,
    Cleanup,
    Restore,
}

impl Operation {
    fn code(self) -> &'static str {
        match self {
            Self::Backup => "backup",
            Self::Cleanup => "cleanup",
            Self::Restore => "restore",
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Failure {
    Locked,
    SourceIncomplete,
    Deadline,
    Failed,
}

impl Failure {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Locked => "locked",
            Self::SourceIncomplete => "source_incomplete",
            Self::Deadline => "deadline",
            Self::Failed => "failed",
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Observation {
    pub operation: Operation,
    pub purpose: Option<BackupPurpose>,
    pub elapsed: Duration,
    pub bytes: Option<u64>,
    pub failure: Option<Failure>,
}

impl Observation {
    fn record(self) {
        let operation = self.operation.code();
        let purpose = self.purpose.map_or(
            match self.operation {
                Operation::Cleanup => "repository",
                _ => "unknown",
            },
            BackupPurpose::code,
        );
        metrics::histogram!("steve_backup_operation_seconds", "operation" => operation, "purpose" => purpose)
            .record(self.elapsed.as_secs_f64());
        if let Some(bytes) = self.bytes {
            metrics::counter!("steve_backup_written_bytes_total", "operation" => operation, "purpose" => purpose)
                .increment(bytes);
        }
        if let Some(failure) = self.failure {
            metrics::counter!("steve_backup_failures_total", "operation" => operation, "purpose" => purpose, "code" => failure.code())
                .increment(1);
        }
    }
}

const DIRECTORY: &str = ".backup-observations";

pub(crate) fn report(home: &Path, observation: Observation) {
    if super::INSTALLED.get().is_some() {
        observation.record();
        return;
    }
    let result = (|| -> anyhow::Result<()> {
        let directory = home.join(DIRECTORY);
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&directory)?;
        crate::backup::private_directory(&directory)?;
        crate::upgrades::atomic(
            &directory.join(format!("{}.json", uuid::Uuid::new_v4())),
            &observation,
        )
    })();
    if let Err(error) = result {
        tracing::warn!(%error, "could not retain offline backup observation");
    }
}

pub(super) async fn collect(home: &Path, database: &Path) {
    let home = home.to_owned();
    let database = database.to_owned();
    let result = tokio::task::spawn_blocking(move || {
        crate::backup::observe_destination(&home, &database);
        drain(&home)
    })
    .await;
    if !matches!(result, Ok(Ok(()))) {
        tracing::warn!(?result, "could not collect offline backup observations");
        metrics::counter!("steve_metrics_collection_errors_total", "source" => "backup")
            .increment(1);
    }
}

fn drain(home: &Path) -> anyhow::Result<()> {
    let directory = home.join(DIRECTORY);
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        result => result?,
    };
    crate::backup::private_directory(&directory)?;
    let mut capacity_changed = false;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") && entry.file_type()?.is_file() {
            let observation: Observation = match crate::upgrades::read(&path) {
                Ok(observation) => observation,
                Err(error) => {
                    tracing::warn!(%error, "could not read offline backup observation");
                    continue;
                }
            };
            fs::remove_file(path)?;
            observation.record();
            capacity_changed = true;
        }
    }
    if capacity_changed {
        crate::backup::observe_capacity(&crate::backup::root(home));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offline_restore_reaches_the_hub_recorder_once() {
        let home = tempfile::tempdir_in("/tmp").unwrap();
        report(
            home.path(),
            Observation {
                operation: Operation::Restore,
                purpose: Some(BackupPurpose::PreUpgrade),
                elapsed: Duration::from_secs(3),
                bytes: Some(123),
                failure: None,
            },
        );
        let recorder = super::super::recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            drain(home.path()).unwrap();
            drain(home.path()).unwrap();
        });
        handle.run_upkeep();
        let text = handle.render();
        let has = |metric: &str, value: &str| {
            text.lines().any(|line| {
                line.starts_with(metric)
                    && line.contains("operation=\"restore\"")
                    && line.contains("purpose=\"pre-upgrade\"")
                    && line.ends_with(value)
            })
        };
        assert!(has("steve_backup_operation_seconds_count{", " 1"), "{text}");
        assert!(has("steve_backup_written_bytes_total{", " 123"), "{text}");
    }
}
