//! Backup observations made outside the Hub survive until its recorder can ingest them.
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    fs,
    io::ErrorKind,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    time::Duration,
};
use steve_types::backup::BackupPurpose;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Operation {
    Initialize,
    Backup,
    Cleanup,
    Restore,
}

impl Operation {
    fn code(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::Backup => "backup",
            Self::Cleanup => "cleanup",
            Self::Restore => "restore",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    SqliteSnapshot,
    FileStage,
    RepoBackup,
    RepoVerify,
    RepoPublish,
    RepoRestore,
    SqliteVerify,
    RepoForget,
    RepoPrune,
}

impl Phase {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::SqliteSnapshot => "sqlite_snapshot",
            Self::FileStage => "file_stage",
            Self::RepoBackup => "repo_backup",
            Self::RepoVerify => "repo_verify",
            Self::RepoPublish => "repo_publish",
            Self::RepoRestore => "repo_restore",
            Self::SqliteVerify => "sqlite_verify",
            Self::RepoForget => "repo_forget",
            Self::RepoPrune => "repo_prune",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EngineCommand {
    Init,
    Cat,
    Unlock,
    Snapshots,
    Backup,
    Ls,
    Dump,
    Tag,
    Restore,
    Forget,
    Prune,
}

impl EngineCommand {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Cat => "cat",
            Self::Unlock => "unlock",
            Self::Snapshots => "snapshots",
            Self::Backup => "backup",
            Self::Ls => "ls",
            Self::Dump => "dump",
            Self::Tag => "tag",
            Self::Restore => "restore",
            Self::Forget => "forget",
            Self::Prune => "prune",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Event {
    Operation(Observation),
    Phase {
        phase: Phase,
        purpose: Option<BackupPurpose>,
        elapsed: Duration,
    },
    Engine {
        command: EngineCommand,
        elapsed: Duration,
        failure: Option<Failure>,
    },
    Staged {
        purpose: BackupPurpose,
        files: u64,
        bytes: u64,
        skipped_caches: u64,
    },
    Deleted {
        purpose: Option<BackupPurpose>,
        count: u64,
    },
    Reclaimed {
        bytes: u64,
    },
}

#[derive(Serialize, Deserialize)]
struct Batch {
    events: Vec<Event>,
}

impl Observation {
    fn record(self) {
        let operation = self.operation.code();
        let purpose = self.purpose.map_or(
            match self.operation {
                Operation::Initialize | Operation::Cleanup => "repository",
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

impl Event {
    fn record(self) {
        match self {
            Self::Operation(observation) => observation.record(),
            Self::Phase {
                phase,
                purpose,
                elapsed,
            } => {
                let purpose = purpose.map_or("repository", BackupPurpose::code);
                metrics::histogram!(
                    "steve_backup_phase_seconds",
                    "phase" => phase.code(),
                    "purpose" => purpose
                )
                .record(elapsed.as_secs_f64());
            }
            Self::Engine {
                command,
                elapsed,
                failure,
            } => {
                let cmd = command.code();
                let result = failure.map_or("ok", Failure::code);
                metrics::counter!(
                    "steve_backup_engine_commands_total",
                    "command" => cmd,
                    "result" => result
                )
                .increment(1);
                metrics::histogram!("steve_backup_engine_command_seconds", "command" => cmd)
                    .record(elapsed.as_secs_f64());
            }
            Self::Staged {
                purpose,
                files,
                bytes,
                skipped_caches,
            } => {
                let purpose = purpose.code();
                if files > 0 {
                    metrics::counter!("steve_backup_staged_files_total", "purpose" => purpose)
                        .increment(files);
                }
                if bytes > 0 {
                    metrics::counter!("steve_backup_staged_bytes_total", "purpose" => purpose)
                        .increment(bytes);
                }
                if skipped_caches > 0 {
                    metrics::counter!("steve_backup_skipped_caches_total", "purpose" => purpose)
                        .increment(skipped_caches);
                }
            }
            Self::Deleted { purpose, count } => {
                if count > 0 {
                    let purpose = purpose.map_or("pending", BackupPurpose::code);
                    metrics::counter!("steve_backup_snapshots_deleted_total", "purpose" => purpose)
                        .increment(count);
                }
            }
            Self::Reclaimed { bytes } => {
                if bytes > 0 {
                    metrics::counter!("steve_backup_reclaimed_bytes_total").increment(bytes);
                }
            }
        }
    }
}

const DIRECTORY: &str = ".backup-observations";

thread_local! {
    static PENDING: RefCell<Option<(PathBuf, Vec<Event>)>> = const { RefCell::new(None) };
}

/// Groups offline observations from one top-level backup, restore or cleanup invocation into one
/// atomic file so subprocess boundaries do not incur extra directory syncs.
pub(crate) struct Scope {
    active: bool,
}

pub(crate) fn scope(home: &Path) -> Scope {
    if super::INSTALLED.get().is_some() {
        return Scope { active: false };
    }
    let active = PENDING.with(|pending| {
        let mut slot = pending.borrow_mut();
        if slot.is_none() {
            *slot = Some((home.to_path_buf(), Vec::new()));
            true
        } else {
            false
        }
    });
    Scope { active }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let batch = PENDING.with(|pending| pending.borrow_mut().take());
        if let Some((home, events)) = batch
            && !events.is_empty()
        {
            persist(&home, &Batch { events });
        }
    }
}

fn persist(home: &Path, batch: &Batch) {
    let result = (|| -> anyhow::Result<()> {
        let directory = home.join(DIRECTORY);
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&directory)?;
        crate::backup::private_directory(&directory)?;
        crate::upgrades::atomic(
            &directory.join(format!("{}.json", uuid::Uuid::new_v4())),
            batch,
        )
    })();
    if let Err(error) = result {
        tracing::warn!(%error, "could not retain offline backup observation");
    }
}

pub(crate) fn emit(home: &Path, event: Event) {
    if super::INSTALLED.get().is_some() {
        event.record();
        return;
    }
    let buffered = PENDING.with(|pending| {
        let mut slot = pending.borrow_mut();
        if let Some((scope_home, events)) = slot.as_mut()
            && scope_home == home
        {
            events.push(event);
            None
        } else {
            Some(event)
        }
    });
    if let Some(event) = buffered {
        persist(
            home,
            &Batch {
                events: vec![event],
            },
        );
    }
}

pub(crate) fn report(home: &Path, observation: Observation) {
    emit(home, Event::Operation(observation));
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
            let batch: Batch = match crate::upgrades::read(&path) {
                Ok(batch) => batch,
                Err(error) => {
                    tracing::warn!(%error, "could not read offline backup observation");
                    continue;
                }
            };
            fs::remove_file(path)?;
            for event in batch.events {
                event.record();
            }
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
        {
            let _batch = scope(home.path());
            emit(
                home.path(),
                Event::Engine {
                    command: EngineCommand::Restore,
                    elapsed: Duration::from_secs(2),
                    failure: None,
                },
            );
            emit(
                home.path(),
                Event::Phase {
                    phase: Phase::SqliteVerify,
                    purpose: Some(BackupPurpose::PreUpgrade),
                    elapsed: Duration::from_secs(1),
                },
            );
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
        }
        assert_eq!(
            fs::read_dir(home.path().join(DIRECTORY)).unwrap().count(),
            1,
            "scoped offline operation writes one atomic observation file"
        );
        let recorder = super::super::recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            drain(home.path()).unwrap();
            drain(home.path()).unwrap();
        });
        handle.run_upkeep();
        let text = handle.render();
        let has = |metric: &str, labels: &[&str], value: &str| {
            text.lines().any(|line| {
                line.starts_with(metric)
                    && labels.iter().all(|label| line.contains(label))
                    && line.ends_with(value)
            })
        };
        assert!(
            has(
                "steve_backup_operation_seconds_count{",
                &["operation=\"restore\"", "purpose=\"pre-upgrade\""],
                " 1"
            ),
            "{text}"
        );
        assert!(
            has(
                "steve_backup_written_bytes_total{",
                &["operation=\"restore\"", "purpose=\"pre-upgrade\""],
                " 123"
            ),
            "{text}"
        );
        assert!(
            has(
                "steve_backup_phase_seconds_count{",
                &["phase=\"sqlite_verify\"", "purpose=\"pre-upgrade\""],
                " 1"
            ),
            "{text}"
        );
        assert!(
            has(
                "steve_backup_engine_commands_total{",
                &["command=\"restore\"", "result=\"ok\""],
                " 1"
            ),
            "{text}"
        );
        assert!(
            has(
                "steve_backup_engine_command_seconds_count{",
                &["command=\"restore\""],
                " 1"
            ),
            "{text}"
        );
    }
}
