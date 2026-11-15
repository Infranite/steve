use std::path::{Path, PathBuf};

use steve_node_api::{DeliveryChunk, FileAddress, FileRead, PathRef, RelPath, Revision, RootPath};
use steve_store::Session;

use super::{Delivery, MAX_FILE, Service};
use crate::wire::workspaces::{At, PlacementError, Subject, Workspaces};

const CHUNK: u32 = 128 * 1024;

#[derive(Default)]
struct Receive {
    bytes: Vec<u8>,
    version: Option<Revision>,
    size: Option<u64>,
}

impl Receive {
    fn push(&mut self, r: DeliveryChunk) -> Result<bool, String> {
        let c = r.chunk;
        if r.size > MAX_FILE as u64
            || c.bytes.len() > CHUNK as usize
            || self.size.is_some_and(|size| size != r.size)
            || self
                .version
                .as_ref()
                .is_some_and(|version| *version != r.version)
            || c.offset != self.bytes.len() as u64
            || c.offset.saturating_add(c.bytes.len() as u64) > r.size
            || c.eof != (c.offset + c.bytes.len() as u64 == r.size)
            || (!c.eof && c.bytes.is_empty())
        {
            return Err(
                "incomplete or changed Node delivery (size, revision, offset or EOF mismatch)"
                    .into(),
            );
        }
        self.version = Some(r.version);
        self.size = Some(r.size);
        self.bytes.extend(c.bytes);
        Ok(c.eof)
    }
}

enum Source {
    Local(PathBuf, RelPath),
    Node(std::sync::Arc<dyn steve_node_api::Node>, FileAddress),
}

impl Service {
    /// Bot input is scoped to its own session directory or its item's Hub-owned outputs. The
    /// Node placement is taken from the current binding, never from a Bot-supplied node id.
    pub async fn receive(
        &self,
        session: &Session,
        path: &str,
        title: Option<&str>,
        entrypoint: Option<&str>,
        workspaces: &Workspaces,
    ) -> Result<Delivery, String> {
        let item = session
            .item_id
            .as_deref()
            .ok_or("file delivery requires an item session")?;
        let cwd = Path::new(&session.cwd);
        let source = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            cwd.join(path)
        };
        let check_part = source.strip_prefix(cwd).unwrap_or(Path::new(path));
        if crate::wire::output_files::polluted_output_path(check_part) {
            return Err("delivery must not contain repository metadata, dependencies or build outputs; deliver a standalone file or ZIP".into());
        }
        let name = source
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("invalid delivery filename")?
            .to_string();
        let durable =
            crate::wire::output_files::directory(&self.home, item).map_err(|e| e.to_string())?;
        let src = if source.starts_with(&durable) {
            // This is explicitly Hub-owned, not a similarly named path on another Node.
            // Anchor at the trusted installation home: workspaces/item/outputs are ALL walked
            // without following links. Canonicalizing the outputs directory itself would erase
            // a malicious symlink (or race with its replacement) before the strict reader saw it.
            let rel = source.strip_prefix(&self.home).expect("outputs below home");
            let rel = RelPath::new(rel.to_str().ok_or("delivery path must be UTF-8")?)
                .map_err(|e| e.to_string())?;
            Source::Local(self.home.canonicalize().map_err(|e| e.to_string())?, rel)
        } else {
            let rel = source.strip_prefix(cwd).map_err(
                |_| "delivery must be inside the session directory; package folders as ZIP",
            )?;
            let rel = RelPath::new(rel.to_str().ok_or("delivery path must be UTF-8")?)
                .map_err(|e| e.to_string())?;
            // A newly created session can add a more specific root before the asynchronous
            // project/session observer reaches the Node. Refresh before choosing its root id.
            workspaces.refresh_roots().map_err(|e| e.to_string())?;
            match workspaces.placement_of(Subject::Session(&session.id)) {
                Ok(p) => {
                    let address = match p.at {
                        At::Workspace(workspace) => {
                            FileAddress::Workspace(PathRef { workspace, rel })
                        }
                        At::Root(base) => FileAddress::Root(RootPath {
                            root: base.root,
                            rel: RelPath::new(if base.rel.as_str().is_empty() {
                                rel.as_str().into()
                            } else {
                                format!("{}/{}", base.rel.as_str(), rel.as_str())
                            })
                            .map_err(|e| e.to_string())?,
                        }),
                    };
                    Source::Node(p.node, address)
                }
                Err(PlacementError::LocalOff) => {
                    Source::Local(cwd.canonicalize().map_err(|e| e.to_string())?, rel)
                }
                Err(e) => return Err(format!("delivery Node unavailable: {e}")),
            }
        };
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(300), read_source(src))
            .await
            .map_err(|_| "delivery transfer timed out before complete receipt")??;
        let (service, session, title, entrypoint) = (
            self.clone(),
            session.clone(),
            title.unwrap_or(&name).to_string(),
            entrypoint.map(str::to_string),
        );
        tokio::task::spawn_blocking(move || {
            service.accept(&session, &title, &name, &bytes, entrypoint.as_deref())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// `steve_deliver` references the exact version returned by `steve_deliver_file`. Validate it
    /// in this session's workspace; never interpret a foreign or malformed delivery URL as a path.
    pub fn reference(&self, session: &Session, location: &str) -> Result<Option<Delivery>, String> {
        let prefix = format!("{}/deliveries/", self.base);
        let Some(tail) = location.strip_prefix(&prefix) else {
            return Ok(None);
        };
        let id = tail.split('?').next().unwrap_or_default();
        let d = self
            .get(id, Some(&session.workspace_id))
            .map_err(|e| e.to_string())?
            .ok_or("delivery is not in this workspace")?;
        // Accept old receipts without a selector, and the exact new page URL. Do not infer
        // an association from a path on an arbitrary origin, or swallow a wrong selector.
        if location != format!("{prefix}{}", d.id) && location != self.url(&d.id, &d.workspace_id) {
            return Err("invalid delivery URL or workspace selector".into());
        }
        // A coordinator may include an upstream child's delivery in its own outputs. Reuse
        // the exact immutable version in the same workspace; its original item_id stays intact.
        // It remains an ordinary link: artifacts attach delivery_id only within the SAME item.
        Ok(Some(d))
    }
}

async fn read_source(src: Source) -> Result<Vec<u8>, String> {
    let mut recv = Receive::default();
    loop {
        let offset = recv.bytes.len() as u64;
        let expected = recv.version.clone();
        let chunk = match &src {
            Source::Node(node, path) => node
                .read_delivery(FileRead {
                    path: path.clone(),
                    expected,
                    offset,
                    max: CHUNK,
                })
                .await
                .map_err(|e| format!("Node delivery refused: {}", e.safe_detail))?,
            Source::Local(root, rel) => {
                let (root, rel) = (root.clone(), rel.clone());
                tokio::task::spawn_blocking(move || {
                    steve_node::read_delivery_file(&root, &rel, expected.as_ref(), offset, CHUNK)
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.safe_detail)?
            }
        };
        if recv.push(chunk)? {
            break;
        }
    }

    Ok(recv.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use steve_node_api::*;
    use steve_node_wire::{Conn, ConnOptions, Handler, RemoteNode};

    struct Peer {
        root: PathBuf,
        mode: u8,
    }
    #[async_trait::async_trait]
    impl Handler for Peer {
        async fn handle(&self, method: &str, p: serde_json::Value) -> ApiResult<serde_json::Value> {
            assert_eq!(
                method,
                steve_node_wire::methods::DELIVERY_READ,
                "no fallback to general fs.read"
            );
            if self.mode == 1 {
                return Err(ApiError::new(ErrorCode::Unsupported, "old Node"));
            }
            let q: FileRead = steve_node_wire::conn::from_value(p)?;
            let FileAddress::Root(path) = q.path else {
                panic!("fixture root")
            };
            assert!(q.max <= steve_node_wire::envelope::CHUNK_BYTES);
            if q.offset > 0 {
                assert!(q.expected.is_some(), "revision is pinned after first chunk");
            }
            let mut r = steve_node::read_delivery_file(
                &self.root,
                &path.rel,
                q.expected.as_ref(),
                q.offset,
                q.max,
            )?;
            if self.mode == 2 {
                r.chunk.eof = true;
            } // truncated, falsely final
            if self.mode == 3 && q.offset > 0 {
                r.version = Revision::new("changed").unwrap();
            }
            if self.mode == 4 && q.offset > 0 {
                return Err(ApiError::new(ErrorCode::Offline, "link cut"));
            }
            Ok(serde_json::to_value(r).unwrap())
        }
    }

    async fn transfer(root: PathBuf, mode: u8) -> Result<Vec<u8>, String> {
        let (h, n) = steve_node_wire::testkit::pair();
        let opts = |side| ConnOptions {
            side,
            ..Default::default()
        };
        let hub = Conn::spawn(
            h.sink,
            h.stream,
            1,
            Arc::new(Peer {
                root: root.clone(),
                mode,
            }),
            opts("h"),
        );
        let node = Conn::spawn(
            n.sink,
            n.stream,
            1,
            Arc::new(Peer { root, mode }),
            opts("n"),
        );
        let remote = RemoteNode::new(NodeId::new("remote").unwrap());
        remote.attach(hub, false);
        let result = read_source(Source::Node(
            remote.clone(),
            FileAddress::Root(RootPath {
                root: RootId::new("outputs").unwrap(),
                rel: RelPath::new("report.md").unwrap(),
            }),
        ))
        .await;
        node.close(None, 1000, "test complete");
        remote.detach(1);
        result
    }

    #[tokio::test]
    async fn remote_node_transfer_is_complete_pins_versions_and_fails_closed() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        let data = vec![b'x'; CHUNK as usize * 3 + 17];
        std::fs::write(root.join("report.md"), &data).unwrap();
        let (_hub, svc, session) = super::super::tests::fixture();
        let received = transfer(root.clone(), 0).await.unwrap();
        assert_eq!(received, data);
        let delivery = svc
            .accept(&session, "Remote report", "report.md", &received, None)
            .unwrap();
        for mode in 1..=4 {
            assert!(transfer(root.clone(), mode).await.is_err(), "mode={mode}");
        }
        std::fs::remove_file(root.join("report.md")).unwrap();
        // The wire peer is detached and its file is gone; this read is exclusively Hub-owned.
        let (mut file, _, _, _) = svc.open(&delivery, None).unwrap();
        let mut saved = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut saved).unwrap();
        assert_eq!(saved, data);
    }

    #[test]
    fn rejects_false_offsets_empty_progress_early_eof_and_oversized_payloads() {
        let chunk = |size, offset, bytes: Vec<u8>, eof| DeliveryChunk {
            size,
            version: Revision::new("v1").unwrap(),
            chunk: ReadChunk { offset, bytes, eof },
        };
        for c in [
            chunk(10, 0, vec![1; 2], true),
            chunk(10, 1, vec![1; 9], true),
            chunk(10, 0, vec![], false),
            chunk(1, 0, vec![1; 2], true),
            chunk(MAX_FILE as u64 + 1, 0, vec![], false),
            chunk(CHUNK as u64 + 1, 0, vec![1; CHUNK as usize + 1], true),
        ] {
            assert!(Receive::default().push(c).is_err());
        }
        assert!(Receive::default().push(chunk(0, 0, vec![], true)).unwrap());
    }
}
