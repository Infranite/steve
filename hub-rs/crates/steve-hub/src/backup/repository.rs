use super::{
    Purpose,
    engine::{Engine, private, private_tempdir},
};
use crate::metrics::backup::{
    self as telemetry, EngineCommand, Event, Observation, Operation, Phase,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
    time::Instant,
};
use steve_modules::vault::{keystore, secret::Kek};
use steve_types::backup::SnapshotId;

const PENDING: &str = "steve:pending";

pub(super) struct Repository {
    engine: Engine,
    key: Kek,
}

#[derive(Deserialize, Serialize)]
struct Publication {
    pending: SnapshotId,
    purpose: Purpose,
    operation: String,
}

#[derive(Deserialize)]
pub(super) struct Snapshot {
    pub id: SnapshotId,
    time: jiff::Timestamp,
    #[serde(default)]
    tags: Vec<String>,
}

impl Snapshot {
    pub fn created_ms(&self) -> u64 {
        self.time.as_millisecond().max(0) as u64
    }

    pub fn purpose(&self) -> Option<Purpose> {
        if self.pending()
            || !self
                .tags
                .iter()
                .any(|tag| tag.starts_with("steve:operation:"))
        {
            return None;
        }
        let mut purposes = Purpose::ALL
            .into_iter()
            .filter(|p| self.tags.contains(&purpose_tag(*p)));
        let purpose = purposes.next()?;
        purposes.next().is_none().then_some(purpose)
    }

    pub fn pending(&self) -> bool {
        self.tags.iter().any(|tag| tag == PENDING)
            && self
                .tags
                .iter()
                .any(|tag| tag.starts_with("steve:operation:"))
    }
}

fn purpose_tag(purpose: Purpose) -> String {
    format!("purpose:{}", purpose.code())
}

#[derive(Deserialize)]
#[serde(tag = "message_type", rename_all = "snake_case")]
enum Message {
    Summary {
        snapshot_id: Option<SnapshotId>,
        data_added_packed: Option<u64>,
        bytes_restored: Option<u64>,
    },
    Node {
        path: String,
        #[serde(rename = "type")]
        kind: String,
        size: Option<u64>,
    },
    #[serde(other)]
    Other,
}

fn messages(bytes: &[u8]) -> impl Iterator<Item = Result<Message>> {
    bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| Ok(serde_json::from_slice(line)?))
}

impl Repository {
    pub fn open(home: &Path, deadline: Instant, create: bool) -> Result<Option<Self>> {
        let started = Instant::now();
        if !create && !super::root(home).try_exists()? {
            return Ok(None);
        }
        let engine = Engine::lock(home, deadline)?;
        let path = engine.repository();
        let exists = match fs::symlink_metadata(&path) {
            Ok(_) => {
                private(&path)?;
                true
            }
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if !exists && !create {
            return Ok(None);
        }
        let store = keystore::from_env(home, |k| match (cfg!(test), k) {
            (true, "STEVE_VAULT_KEYSTORE") => Some("file".into()),
            (true, "HOME") => Some(home.to_string_lossy().into_owned()),
            _ => std::env::var(k).ok(),
        })?;
        // Keychain and Secret Service are shared by installations under the same OS user.
        // A repository's key account stays stable for the life of its installation path.
        use sha2::{Digest, Sha256};
        let identity = Sha256::digest(home.canonicalize()?.as_os_str().as_encoded_bytes());
        let account = format!("backup/restic-v2/{}", hex::encode(identity));
        let key = match store.get(&account)? {
            Some(key) => key,
            None if exists => anyhow::bail!("backup_repository_key_missing"),
            None => {
                let key = Kek::generate()?;
                store.set(&account, &key)?;
                let saved = store
                    .get(&account)?
                    .context("backup repository key was not persisted")?;
                ensure!(
                    keystore::encode(&saved).as_str() == keystore::encode(&key).as_str(),
                    "backup repository key readback mismatch"
                );
                saved
            }
        };
        let repo = Self { engine, key };
        if exists && repo.work().join("snapshot").try_exists()? {
            // A process can die after restic has written packs and before it creates a snapshot.
            repo.require_prune()?;
        }
        repo.engine.reset_work()?;
        if !exists {
            let init_result = (|| -> Result<Option<u64>> {
                let init = private_tempdir(&repo.work(), "init-")?;
                repo.engine.run_at(
                    init.path(),
                    &repo.key,
                    EngineCommand::Init,
                    &["--repository-version".as_ref(), "2".as_ref()],
                    init.path(),
                )?;
                private(init.path())?;
                fs::rename(init.path(), &path)?;
                File::open(path.parent().context("missing repository parent")?)?.sync_all()?;
                Ok(super::capacity(&path).ok().map(|capacity| capacity.bytes))
            })();
            let bytes = init_result.as_ref().ok().copied().flatten();
            telemetry::report(
                home,
                Observation {
                    operation: Operation::Initialize,
                    purpose: None,
                    elapsed: started.elapsed(),
                    bytes,
                    failure: init_result.as_ref().err().map(super::error_code),
                },
            );
            init_result?;
        }
        #[derive(Deserialize)]
        struct Config {
            version: u32,
        }
        let config: Config =
            serde_json::from_slice(&repo.command(EngineCommand::Cat, &["config".as_ref()])?)?;
        ensure!(config.version == 2, "backup repository must use format v2");
        // The repository's own liveness check removes stale locks left by an interrupted
        // restic process. It preserves locks belonging to active clients.
        repo.command(EngineCommand::Unlock, &[])?;
        Ok(Some(repo))
    }

    pub fn path(&self) -> PathBuf {
        self.engine.repository()
    }
    pub fn work(&self) -> PathBuf {
        self.engine.work()
    }
    pub fn remaining(&self) -> Result<std::time::Duration> {
        self.engine.remaining()
    }

    pub fn command(&self, operation: EngineCommand, args: &[&OsStr]) -> Result<Vec<u8>> {
        self.engine.run(&self.key, operation, args, &self.work())
    }

    pub fn snapshots(&self) -> Result<Vec<Snapshot>> {
        let bytes = self.command(EngineCommand::Snapshots, &[])?;
        Ok(serde_json::from_slice::<Option<Vec<Snapshot>>>(&bytes)?.unwrap_or_default())
    }

    pub fn backup(
        &self,
        cwd: &Path,
        purpose: Purpose,
        required: &[String],
        database: &steve_types::backup::VerifiedDatabase,
    ) -> Result<(Snapshot, u64)> {
        let operation = format!("steve:operation:{}", uuid::Uuid::new_v4());
        let tags = format!("{PENDING},{operation}");
        let mut args = vec![
            "--force".as_ref(),
            "--tag".as_ref(),
            tags.as_ref(),
            super::DATABASE_ENTRY[1..].as_ref(),
            super::CHECKSUM_ENTRY[1..].as_ref(),
        ];
        if required.len() > 2 {
            args.push("files".as_ref());
        }
        let backup_started = Instant::now();
        let bytes = match self
            .engine
            .run(&self.key, EngineCommand::Backup, &args, cwd)
        {
            Ok(bytes) => {
                telemetry::emit(
                    self.engine.home(),
                    Event::Phase {
                        phase: Phase::RepoBackup,
                        purpose: Some(purpose),
                        elapsed: backup_started.elapsed(),
                    },
                );
                bytes
            }
            Err(error) => {
                telemetry::emit(
                    self.engine.home(),
                    Event::Phase {
                        phase: Phase::RepoBackup,
                        purpose: Some(purpose),
                        elapsed: backup_started.elapsed(),
                    },
                );
                return match self.require_prune() {
                    Ok(()) => Err(error),
                    Err(mark) => {
                        Err(error.context(format!("could not record prune intent: {mark}")))
                    }
                };
            }
        };
        let mut summary = None;
        for message in messages(&bytes) {
            if let Message::Summary {
                snapshot_id: Some(id),
                data_added_packed: Some(written),
                ..
            } = message?
            {
                summary = Some((id, written));
            }
        }
        let (pending, written) =
            summary.context("backup summary omitted snapshot ID or written bytes")?;
        let verify_started = Instant::now();
        let verify_result = (|| -> Result<()> {
            let mut args: Vec<&OsStr> = vec![pending.as_str().as_ref()];
            args.extend(required.iter().map(OsStr::new));
            let listing = self.command(EngineCommand::Ls, &args)?;
            let mut present = std::collections::HashSet::new();
            for message in messages(&listing) {
                if let Message::Node { path, kind, size } = message?
                    && match path.as_str() {
                        super::DATABASE_ENTRY => kind == "file" && size == Some(database.bytes),
                        super::CHECKSUM_ENTRY => kind == "file" && size == Some(64),
                        _ => true,
                    }
                {
                    present.insert(path);
                }
            }
            ensure!(
                required.iter().all(|path| present.contains(path)),
                "backup_required_file_missing"
            );
            let checksum = self.command(
                EngineCommand::Dump,
                &[pending.as_str().as_ref(), super::CHECKSUM_ENTRY.as_ref()],
            )?;
            ensure!(
                checksum == database.sha256.as_bytes(),
                "backup database checksum metadata mismatch"
            );
            Ok(())
        })();
        telemetry::emit(
            self.engine.home(),
            Event::Phase {
                phase: Phase::RepoVerify,
                purpose: Some(purpose),
                elapsed: verify_started.elapsed(),
            },
        );
        verify_result?;
        // Only a successful, verified pending snapshot gets a durable publication intent.
        let publish_started = Instant::now();
        let intent = Publication {
            pending,
            purpose,
            operation,
        };
        let published = (|| -> Result<Snapshot> {
            crate::upgrades::atomic(&self.publication_path(), &intent)?;
            self.finish_publication(&intent)
        })();
        telemetry::emit(
            self.engine.home(),
            Event::Phase {
                phase: Phase::RepoPublish,
                purpose: Some(purpose),
                elapsed: publish_started.elapsed(),
            },
        );
        Ok((published?, written))
    }

    fn publication_path(&self) -> PathBuf {
        self.path().with_file_name(".restic-publication.json")
    }

    pub fn recover_publication(&self) -> Result<()> {
        let path = self.publication_path();
        if path.try_exists()? {
            let intent: Publication = crate::upgrades::read(&path)?;
            self.finish_publication(&intent)?;
        }
        Ok(())
    }

    fn finish_publication(&self, intent: &Publication) -> Result<Snapshot> {
        let identities = |repo: &Self| -> Result<Vec<Snapshot>> {
            let bytes = repo.command(
                EngineCommand::Snapshots,
                &["--tag".as_ref(), intent.operation.as_ref()],
            )?;
            Ok(serde_json::from_slice::<Option<Vec<Snapshot>>>(&bytes)?.unwrap_or_default())
        };
        let mut identities_before = identities(self)?;
        if !identities_before
            .iter()
            .any(|snapshot| snapshot.purpose() == Some(intent.purpose))
        {
            ensure!(
                identities_before
                    .iter()
                    .any(|s| s.id == intent.pending && s.pending()),
                "verified pending backup missing"
            );
            let tag = purpose_tag(intent.purpose);
            self.command(
                EngineCommand::Tag,
                &[
                    "--add".as_ref(),
                    tag.as_ref(),
                    "--remove".as_ref(),
                    PENDING.as_ref(),
                    intent.pending.as_str().as_ref(),
                ],
            )?;
            identities_before = identities(self)?;
        }
        let mut published = identities_before
            .into_iter()
            .filter(|snapshot| snapshot.purpose() == Some(intent.purpose));
        let snapshot = published
            .next()
            .context("backup finalized snapshot missing")?;
        ensure!(
            published.next().is_none(),
            "multiple finalized snapshots for one operation"
        );
        let path = self.publication_path();
        fs::remove_file(&path)?;
        File::open(path.parent().context("missing publication parent")?)?.sync_all()?;
        Ok(snapshot)
    }

    pub fn restore(
        &self,
        id: &SnapshotId,
        purpose: Purpose,
        target: &Path,
        db_only: bool,
    ) -> Result<u64> {
        let started = Instant::now();
        let result = (|| -> Result<u64> {
            let mut args = vec![
                id.as_str().as_ref(),
                "--target".as_ref(),
                target.as_os_str(),
                "--verify".as_ref(),
            ];
            if db_only {
                args.extend(
                    [
                        "--include",
                        super::DATABASE_ENTRY,
                        "--include",
                        super::CHECKSUM_ENTRY,
                    ]
                    .map(OsStr::new),
                );
            }
            let output = self.command(EngineCommand::Restore, &args)?;
            for message in messages(&output) {
                if let Message::Summary {
                    bytes_restored: Some(bytes),
                    ..
                } = message?
                {
                    return Ok(bytes);
                }
            }
            anyhow::bail!("restore summary omitted bytes restored")
        })();
        telemetry::emit(
            self.engine.home(),
            Event::Phase {
                phase: Phase::RepoRestore,
                purpose: Some(purpose),
                elapsed: started.elapsed(),
            },
        );
        result
    }

    pub fn require_prune(&self) -> Result<()> {
        let mark = self.path().with_file_name(".restic-needs-prune");
        File::create_new(&mark)
            .or_else(|error| {
                if error.kind() == ErrorKind::AlreadyExists {
                    File::open(&mark)
                } else {
                    Err(error)
                }
            })?
            .sync_all()?;
        File::open(mark.parent().context("missing prune marker parent")?)?.sync_all()?;
        Ok(())
    }

    pub fn prune_if_needed(&self) -> Result<bool> {
        let mark = self.path().with_file_name(".restic-needs-prune");
        if mark.try_exists()? {
            let started = Instant::now();
            let result = (|| -> Result<()> {
                self.command(
                    EngineCommand::Prune,
                    &["--max-unused".as_ref(), "0".as_ref()],
                )?;
                fs::remove_file(&mark)?;
                File::open(mark.parent().context("missing prune marker parent")?)?.sync_all()?;
                Ok(())
            })();
            telemetry::emit(
                self.engine.home(),
                Event::Phase {
                    phase: Phase::RepoPrune,
                    purpose: None,
                    elapsed: started.elapsed(),
                },
            );
            result?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn forget(&self, purpose: Option<Purpose>, ids: &[SnapshotId]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let started = Instant::now();
        let result = (|| -> Result<()> {
            for chunk in ids.chunks(256) {
                let args: Vec<_> = chunk.iter().map(|id| OsStr::new(id.as_str())).collect();
                self.command(EngineCommand::Forget, &args)?;
            }
            Ok(())
        })();
        telemetry::emit(
            self.engine.home(),
            Event::Phase {
                phase: Phase::RepoForget,
                purpose,
                elapsed: started.elapsed(),
            },
        );
        result
    }
}
