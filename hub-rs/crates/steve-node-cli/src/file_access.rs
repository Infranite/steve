//! Local setup reads the same observation the running Node sends over discovery.

use anyhow::{Context, Result, ensure};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use steve_node::LocalNode;
use steve_node_api::{Discovery, FileAccessReport, NODE_BOOT_SERVICE_ENV, NODE_FILE_ACCESS_SOCKET};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;

pub(crate) struct SetupProbe {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl SetupProbe {
    pub(crate) fn start(home: &Path, node: LocalNode) -> Result<Option<Self>> {
        if std::env::var_os(NODE_BOOT_SERVICE_ENV).as_deref() != Some("1".as_ref()) {
            return Ok(None);
        }
        // The Node already holds its exclusive home lock. Only that Node can
        // have left a stale socket behind from a crash.
        let path = home.join(NODE_FILE_ACCESS_SOCKET);
        if let Ok(meta) = path.symlink_metadata() {
            ensure!(
                meta.file_type().is_socket() && meta.uid() == unsafe { libc::getuid() },
                "file-access endpoint is not an owned socket"
            );
            std::fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path).context("bind file-access setup socket")?;
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let report = node
                    .host()
                    .await
                    .ok()
                    .and_then(|host| host.file_access)
                    .unwrap_or_else(FileAccessReport::unknown);
                if let Ok(bytes) = serde_json::to_vec(&report) {
                    let _ = socket.write_all(&bytes).await;
                }
            }
        });
        Ok(Some(Self { path, task }))
    }
}

impl Drop for SetupProbe {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}
