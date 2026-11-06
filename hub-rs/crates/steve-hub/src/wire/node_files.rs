//! Hub-owned attachment storage. Execution scope is the frozen attempt prompt, not every file in
//! the workspace. Operation access resolves a Hub-owned start_turn or start_job command to that
//! same execution. A job attempt's frozen prompt is its `JobSpec` in `runtime_job_attempts`, and
//! its files must be of its job's workspace.
use super::node_tools::{authorize, internal};
use async_trait::async_trait;
use rusqlite::OptionalExtension;
use std::path::PathBuf;
use steve_node_api::*;
use steve_store::{Store, StoreError};

pub struct NodeFiles {
    store: Store,
    node: NodeId,
    data_dir: PathBuf,
}
impl NodeFiles {
    pub fn new(store: Store, node: NodeId, data_dir: PathBuf) -> Self {
        Self {
            store,
            node,
            data_dir,
        }
    }
    fn execution(&self, access: FileAccess) -> ApiResult<ExecutionRef> {
        match access {
            FileAccess::Execution(exec) => Ok(exec),
            FileAccess::Operation { operation } => {
                let exec=self.store.reader(|c|Ok::<_,StoreError>(c.query_row(
                    "SELECT a.execution FROM node_commands c JOIN runtime_attempts a ON a.attempt_id=c.target_attempt AND a.operation_id=c.operation_id WHERE c.operation_id=?1 AND c.node_id=?2 AND a.node_id=?2 AND c.method='start_turn' AND c.state NOT IN ('finished','rejected','unknown')",
                    rusqlite::params![operation.as_str(),self.node.as_str()],|r|r.get::<_,String>(0)).optional()?)).map_err(internal)?;
                let exec = match exec {
                    Some(e) => Some(e),
                    None => self.store.reader(|c|Ok::<_,StoreError>(c.query_row(
                        "SELECT a.execution FROM node_commands c JOIN runtime_job_attempts a ON a.attempt_id=c.target_job_attempt AND a.operation_id=c.operation_id WHERE c.operation_id=?1 AND c.node_id=?2 AND a.node_id=?2 AND c.method='start_job' AND c.state NOT IN ('finished','rejected','unknown')",
                        rusqlite::params![operation.as_str(),self.node.as_str()],|r|r.get::<_,String>(0)).optional()?)).map_err(internal)?,
                };
                serde_json::from_str(&exec.ok_or_else(denied)?).map_err(internal)
            }
        }
    }

    /// The workspace and frozen prompt an execution may read from: a live turn attempt
    /// ([`authorize`]) or a live job attempt of this Node (not finished, unknown or cancelled).
    fn grant(&self, exec: &ExecutionRef) -> ApiResult<(String, Vec<Part>)> {
        let (job, attempt) = match exec {
            ExecutionRef::Turn { .. } => {
                let (session, spec) = authorize(&self.store, &self.node, exec, true)?;
                return Ok((session.workspace_id, spec.run.prompt));
            }
            ExecutionRef::Job { job, attempt } => (job, attempt),
        };
        let row = self.store.reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT a.execution,a.spec,a.state,a.cancel_requested,j.workspace_id FROM runtime_job_attempts a JOIN runtime_jobs j ON j.job_id=a.job_id WHERE a.attempt_id=?1 AND a.job_id=?2 AND a.node_id=?3 AND j.node_id=?3 AND j.state='open'",
                rusqlite::params![attempt.as_str(), job.as_str(), self.node.as_str()],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, bool>(3)?, r.get::<_, String>(4)?)),
            ).optional()?)
        }).map_err(internal)?.ok_or_else(denied)?;
        let (saved, spec, state, cancelled, workspace) = row;
        let saved: ExecutionRef = serde_json::from_str(&saved).map_err(internal)?;
        let spec: JobSpec = serde_json::from_str(&spec).map_err(internal)?;
        if saved != *exec
            || spec.job != *job
            || spec.run.attempt != *attempt
            || cancelled
            || matches!(state.as_str(), "finished" | "unknown")
        {
            return Err(denied());
        }
        Ok((workspace, spec.run.prompt))
    }
}

/// Keeps an image that has no file of its own (a module's or a channel's base64 block) as a
/// job-input artifact of `workspace`, so a job's prompt can name it: content-addressed (the same
/// bytes in the same workspace are one file) and stored like any attachment, under the same
/// retention.
pub fn job_input(
    store: &Store,
    data_dir: &std::path::Path,
    workspace: &str,
    mime: &str,
    bytes: &[u8],
) -> ApiResult<ArtifactRef> {
    use sha2::Digest as _;
    if !component(workspace) {
        return Err(ApiError::new(ErrorCode::Invalid, "bad workspace id"));
    }
    let sha = hex::encode(sha2::Sha256::digest(bytes));
    let id = format!(
        "fil_{}",
        &hex::encode(sha2::Sha256::digest(format!("{workspace}\0{sha}")))[..16]
    );
    let artifact = |mime: String| -> ApiResult<ArtifactRef> {
        Ok(ArtifactRef {
            id: ArtifactId::new(id.clone()).map_err(internal)?,
            sha256: Digest::new(format!("sha256:{sha}")).map_err(internal)?,
            size: bytes.len() as u64,
            mime,
        })
    };
    let path = data_dir
        .join("files")
        .join(workspace)
        .join(&sha[..2])
        .join(&id);
    let row = store.files().get(&id).map_err(internal)?;
    if let Some(row) = &row
        && (row.workspace_id != workspace || row.sha256 != sha)
    {
        return Err(internal("job input id taken by another file"));
    }
    if std::fs::metadata(&path).map(|m| m.len()).ok() != Some(bytes.len() as u64) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let dir = path.parent().expect("a file under data/files");
        std::fs::create_dir_all(dir).map_err(internal)?;
        let tmp = dir.join(format!(".{id}.{}.tmp", uuid::Uuid::new_v4().simple()));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(internal)?;
        f.write_all(bytes).map_err(internal)?;
        f.sync_all().map_err(internal)?;
        std::fs::rename(&tmp, &path).map_err(internal)?;
    }
    match row {
        Some(row) => artifact(row.mime),
        None => {
            let ext = mime.strip_prefix("image/").unwrap_or("bin");
            let row = store
                .files()
                .create(&steve_store::NewFile {
                    id: id.clone(),
                    workspace_id: workspace.into(),
                    sha256: sha.clone(),
                    name: format!("job-input-{}.{ext}", &sha[..12]),
                    mime: mime.into(),
                    size: bytes.len() as i64,
                    created_by: "job".into(),
                })
                .or_else(|e| match store.files().get(&id) {
                    // the same input, kept by a concurrent call
                    Ok(Some(row)) if row.workspace_id == workspace && row.sha256 == sha => Ok(row),
                    _ => Err(internal(e)),
                })?;
            artifact(row.mime)
        }
    }
}
fn denied() -> ApiError {
    ApiError::new(ErrorCode::Denied, "artifact not granted to this execution")
}
fn unsupported<T>() -> ApiResult<T> {
    Err(ApiError::new(
        ErrorCode::Unsupported,
        "Node artifact upload is not enabled",
    ))
}
fn component(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', '\0'])
}
#[async_trait]
impl HubFiles for NodeFiles {
    async fn fetch(
        &self,
        access: FileAccess,
        id: ArtifactId,
        offset: u64,
        max: u32,
    ) -> ApiResult<ReadChunk> {
        if max == 0 {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "nonzero read size required",
            ));
        }
        let exec = self.execution(access)?;
        let (workspace, prompt) = self.grant(&exec)?;
        let artifact = prompt
            .iter()
            .find_map(|part| match part {
                Part::Image { artifact } | Part::File { artifact, .. } if artifact.id == id => {
                    Some(artifact)
                }
                _ => None,
            })
            .ok_or_else(denied)?;
        let row = self
            .store
            .files()
            .get(id.as_str())
            .map_err(internal)?
            // granted, but no longer on the Hub: asking again will not bring it back
            .ok_or_else(|| ApiError::new(ErrorCode::Conflict, "artifact gone from the hub"))?;
        if row.workspace_id != workspace
            || row.size < 0
            || row.size as u64 != artifact.size
            || format!("sha256:{}", row.sha256) != artifact.sha256.as_str()
            || row.mime != artifact.mime
            || row.sha256.len() != 64
            || !row.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || !component(&row.workspace_id)
            || !component(&row.id)
        {
            return Err(denied());
        }
        if offset > artifact.size {
            return Err(ApiError::new(ErrorCode::Invalid, "offset past EOF"));
        }
        let root = self.data_dir.join("files");
        let components = [
            row.workspace_id.clone(),
            row.sha256[..2].to_string(),
            row.id.clone(),
        ];
        let size = artifact.size;
        // Blocking IO stays outside core/store locks. A local blob must remain under data/files;
        // O_NOFOLLOW prevents a final-component symlink swap between checking and opening.
        tokio::task::spawn_blocking(move || {
            use std::io::{Read, Seek, SeekFrom};
            use std::os::{
                fd::{AsRawFd, FromRawFd},
                unix::fs::OpenOptionsExt,
            };
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
                .open(root)
                .map_err(internal)?;
            // Walk with directory handles: no ancestor symlink can escape after validation.
            for (i, part) in components.iter().enumerate() {
                let part = std::ffi::CString::new(part.as_str()).map_err(internal)?;
                let flags = libc::O_RDONLY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK
                    | if i < 2 { libc::O_DIRECTORY } else { 0 };
                let fd = unsafe { libc::openat(file.as_raw_fd(), part.as_ptr(), flags) };
                if fd < 0 {
                    let e = std::io::Error::last_os_error();
                    // the blob is gone from the Hub: it will not come back by asking again
                    if e.kind() == std::io::ErrorKind::NotFound {
                        return Err(ApiError::new(
                            ErrorCode::Conflict,
                            "artifact missing on the hub",
                        ));
                    }
                    return Err(internal(e));
                }
                file = unsafe { std::fs::File::from_raw_fd(fd) };
            }
            let meta = file.metadata().map_err(internal)?;
            if !meta.is_file() || meta.len() != size {
                return Err(ApiError::new(ErrorCode::Conflict, "artifact size changed"));
            }
            file.seek(SeekFrom::Start(offset)).map_err(internal)?;
            let len = (max.min(256 * 1024) as u64).min(size - offset) as usize;
            let mut bytes = vec![0; len];
            file.read_exact(&mut bytes).map_err(internal)?;
            Ok(ReadChunk {
                offset,
                eof: offset + len as u64 == size,
                bytes,
            })
        })
        .await
        .map_err(internal)?
    }
    async fn begin_upload(&self, _: BeginUpload) -> ApiResult<Transfer> {
        unsupported()
    }
    async fn upload(&self, _: BlobChunk) -> ApiResult<Transfer> {
        unsupported()
    }
    async fn upload_status(&self, _: TransferId) -> ApiResult<TransferState> {
        unsupported()
    }
    async fn finish_upload(&self, _: TransferId) -> ApiResult<ArtifactRef> {
        unsupported()
    }
    async fn abort_upload(&self, _: TransferId) -> ApiResult<TransferState> {
        unsupported()
    }
}

#[cfg(test)]
mod tests {
    use super::super::node_tools::tests::fixture;
    use super::*;

    /// Every `.rs` file under `dir`, with its text.
    fn sources(dir: &std::path::Path) -> Vec<(PathBuf, String)> {
        let mut out = vec![];
        for e in std::fs::read_dir(dir).expect("source dir").flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(sources(&p));
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push((p.clone(), std::fs::read_to_string(&p).expect("source")));
            }
        }
        out
    }

    /// N0e-E9: the Hub never copies attachments into a working directory. The legacy runner's copy
    /// (`steve_acp::attachments`) is reachable from the legacy runner only: neither the Hub (its
    /// RoutedRuntime, the Node coordinator, the Node files port) nor the Node calls it; the Node
    /// fetches through HubFiles and places attachments itself (steve-node attachments.rs).
    #[test]
    fn no_attachment_copy_outside_the_legacy_runner() {
        let needle = concat!("for", "_agent");
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for krate in [
            "steve-hub",
            "steve-node",
            "steve-node-api",
            "steve-node-wire",
        ] {
            for (p, text) in sources(&crates.join(krate).join("src")) {
                assert!(
                    !text.contains(needle),
                    "{} reaches the legacy attachment copy",
                    p.display()
                );
            }
        }
        let acp = crates.join("steve-acp/src");
        let users: Vec<String> = sources(&acp)
            .into_iter()
            .filter(|(_, t)| t.contains(needle))
            .filter_map(|(p, _)| p.strip_prefix(&acp).ok().map(|r| r.display().to_string()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(users, ["attachments.rs", "lib.rs", "runner.rs"]);
        // the Node's prompt path places them itself
        let node = std::fs::read_to_string(crates.join("steve-node/src/lib.rs")).expect("lib.rs");
        assert!(node.contains("attachments::place("));
    }
    #[tokio::test]
    async fn granted_chunks_and_workspace_isolation() {
        let bytes = b"attachment bytes";
        let digest = Digest::sha256(bytes);
        let id = ArtifactId::new("fil_test").unwrap();
        // Fixture's coordinator needs the file row before prompt planning: begin without attachments,
        // then freeze the granted descriptor in the stored spec, exactly what plan_in persists.
        let h = fixture(vec![]).await;
        let f = steve_store::NewFile {
            id: id.to_string(),
            workspace_id: "default".into(),
            sha256: digest.as_str()[7..].into(),
            name: "file.txt".into(),
            mime: "text/plain".into(),
            size: bytes.len() as i64,
            created_by: "test".into(),
        };
        h.store.files().create(&f).unwrap();
        let (_, mut spec) =
            authorize(&h.store, &NodeId::new("local").unwrap(), &h.exec, true).unwrap();
        spec.run.prompt.push(Part::File {
            artifact: ArtifactRef {
                id: id.clone(),
                sha256: digest.clone(),
                size: bytes.len() as u64,
                mime: f.mime.clone(),
            },
            name: f.name.clone(),
        });
        h.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "UPDATE runtime_attempts SET spec=?1",
                    [serde_json::to_string(&spec).unwrap()],
                )?;
                Ok(())
            })
            .unwrap();
        let path = h.dir.path().join("files/default").join(&f.sha256[..2]);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join(&f.id), bytes).unwrap();
        let files = NodeFiles::new(
            h.store.clone(),
            NodeId::new("local").unwrap(),
            h.dir.path().into(),
        );
        let access = FileAccess::Execution(h.exec.clone());
        let first = files.fetch(access.clone(), id.clone(), 0, 4).await.unwrap();
        assert_eq!(first.bytes, b"atta");
        assert!(!first.eof);
        let last = files
            .fetch(access.clone(), id.clone(), 4, u32::MAX)
            .await
            .unwrap();
        assert!(last.eof);
        assert_eq!(last.bytes, &bytes[4..]);
        assert!(
            files
                .fetch(access.clone(), id.clone(), bytes.len() as u64, 1)
                .await
                .unwrap()
                .bytes
                .is_empty()
        );
        assert_eq!(
            files
                .fetch(access.clone(), id.clone(), 999, 1)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Invalid
        );
        let op = h
            .store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT operation_id FROM runtime_attempts",
                    [],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .unwrap();
        files
            .fetch(
                FileAccess::Operation {
                    operation: OperationId::new(op).unwrap(),
                },
                id.clone(),
                0,
                1,
            )
            .await
            .unwrap();
        assert_eq!(
            files
                .fetch(access.clone(), ArtifactId::new("fil_other").unwrap(), 0, 1)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Denied
        );
        h.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "UPDATE files SET workspace_id='foreign' WHERE id='fil_test'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            files
                .fetch(access.clone(), id.clone(), 0, 1)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Denied
        );
        h.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "UPDATE files SET workspace_id='default' WHERE id='fil_test'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        std::fs::remove_file(path.join(&f.id)).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", path.join(&f.id)).unwrap();
        assert!(files.fetch(access.clone(), id.clone(), 0, 1).await.is_err());
        // gone from the Hub (its blob, then its row): a conflict, which the Node does not retry
        std::fs::remove_file(path.join(&f.id)).unwrap();
        let e = files
            .fetch(access.clone(), id.clone(), 0, 1)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Conflict, "{e:?}");
        h.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute("DELETE FROM files WHERE id='fil_test'", [])?;
                Ok(())
            })
            .unwrap();
        let e = files.fetch(access, id, 0, 1).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Conflict, "{e:?}");
    }

    /// A job attempt reads the inputs its frozen prompt names, while it is live, and nothing else.
    #[tokio::test]
    async fn a_job_attempt_reads_its_own_inputs_only() {
        use steve_store::jobs::*;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            dir.path().join("hub.sqlite3"),
            steve_store::FnPublisher(|_| {}),
        )
        .unwrap();
        store.ensure_workspace().unwrap();
        let data = dir.path();
        let png = job_input(&store, data, "default", "image/png", b"png-bytes").unwrap();
        assert_eq!(
            job_input(&store, data, "default", "image/png", b"png-bytes").unwrap(),
            png,
            "the same bytes are one input"
        );
        assert_eq!(png.sha256, Digest::sha256(b"png-bytes"));
        let other = job_input(&store, data, "default", "image/png", b"other").unwrap();
        assert_ne!(other.id, png.id);
        let start = |job: &str, attempt: &str, op: &str, prompt: Vec<Part>| {
            let exec = ExecutionRef::Job {
                job: JobId::new(job).unwrap(),
                attempt: AttemptId::new(attempt).unwrap(),
            };
            let mut spec = steve_node_api::testkit::build::job(job, attempt, "看图");
            spec.run.prompt.extend(prompt);
            let (e, s) = (
                serde_json::to_value(&exec).unwrap(),
                serde_json::to_value(&spec).unwrap(),
            );
            store
                .transaction::<_, StoreError>(|tx| {
                    create_job_in(
                        tx,
                        &NewJob {
                            id: job,
                            kind: JobKind::Think,
                            purpose: "answer",
                            workspace: "default",
                            node: "local",
                            thread_run: None,
                        },
                        &NewJobAttempt {
                            id: attempt,
                            ordinal: 0,
                            model: "m",
                            operation: op,
                            execution: &e,
                            spec: &s,
                            digest: "d",
                        },
                        &StartJob {
                            request: &serde_json::json!({}),
                            digest: "d",
                        },
                        1,
                    )
                })
                .unwrap();
            exec
        };
        let mine = start(
            "j_1",
            "a_1",
            "op_1",
            vec![Part::Image {
                artifact: png.clone(),
            }],
        );
        let theirs = start("j_2", "a_2", "op_2", vec![]);
        let files = NodeFiles::new(store.clone(), NodeId::new("local").unwrap(), data.into());
        let read = |access: FileAccess, id: &ArtifactId| {
            let (files, id) = (&files, id.clone());
            async move { files.fetch(access, id, 0, 1 << 20).await }
        };
        let c = read(FileAccess::Execution(mine.clone()), &png.id)
            .await
            .unwrap();
        assert_eq!((c.bytes.as_slice(), c.eof), (&b"png-bytes"[..], true));
        let op = FileAccess::Operation {
            operation: OperationId::new("op_1").unwrap(),
        };
        assert!(read(op.clone(), &png.id).await.is_ok());
        for (access, id) in [
            (FileAccess::Execution(mine.clone()), &other.id),
            (FileAccess::Execution(theirs), &png.id),
        ] {
            assert_eq!(read(access, id).await.unwrap_err().code, ErrorCode::Denied);
        }
        // a finished attempt reads nothing anymore
        store
            .transaction::<_, StoreError>(|tx| {
                apply_attempt_in(tx, "a_1", AttemptState::Finished, None, None, 2)
            })
            .unwrap();
        assert_eq!(
            read(FileAccess::Execution(mine), &png.id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Denied
        );
    }
}
