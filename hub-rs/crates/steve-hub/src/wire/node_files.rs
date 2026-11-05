//! Hub-owned attachment storage. Execution scope is the frozen attempt prompt, not every file in
//! the workspace. Operation access resolves a Hub-owned start_turn command to that same execution.
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
                serde_json::from_str(&exec.ok_or_else(denied)?).map_err(internal)
            }
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
        let (session, spec) = authorize(&self.store, &self.node, &exec, true)?;
        let artifact = spec
            .run
            .prompt
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
            .ok_or_else(denied)?;
        if row.workspace_id != session.workspace_id
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
                    return Err(internal(std::io::Error::last_os_error()));
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
        assert!(files.fetch(access, id, 0, 1).await.is_err());
    }
}
