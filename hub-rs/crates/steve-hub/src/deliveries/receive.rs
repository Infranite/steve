use std::path::{Path, PathBuf};

use steve_node_api::{
    DeliveryChunk, ExecutionDeliveryRead, FileAddress, FileRead, RelPath, Revision, RootPath,
};
use steve_store::Session;

use super::{Delivery, MAX_FILE, Service};
use crate::wire::workspaces::{At, Subject, Workspaces};

pub(super) const CHUNK: u32 = 128 * 1024;

#[derive(Default)]
pub(super) struct Receive {
    pub(super) bytes: Vec<u8>,
    version: Option<Revision>,
    size: Option<u64>,
}

impl Receive {
    pub(super) fn push(&mut self, r: DeliveryChunk) -> Result<bool, String> {
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
    Node(std::sync::Arc<dyn steve_node_api::Node>, FileAddress),
    Execution(
        std::sync::Arc<dyn steve_node_api::Node>,
        ExecutionDeliveryRead,
    ),
}

impl Service {
    /// A static delivery (see [`Self::receive_as`]).
    pub async fn receive(
        &self,
        session: &Session,
        path: &str,
        title: Option<&str>,
        entrypoint: Option<&str>,
        workspaces: &Workspaces,
    ) -> Result<Delivery, String> {
        self.receive_as(session, path, title, entrypoint, false, workspaces)
            .await
    }

    /// Bot input is scoped to its current execution workspace or, in an
    /// item session, its item's Hub-owned outputs. Any session with a known working directory may
    /// deliver: chats, conversations and scheduled runs as well as items. The Node placement is
    /// taken from the current binding, never from a Bot-supplied node id.
    pub async fn receive_as(
        &self,
        session: &Session,
        path: &str,
        title: Option<&str>,
        entrypoint: Option<&str>,
        interactive: bool,
        workspaces: &Workspaces,
    ) -> Result<Delivery, String> {
        if session.cwd.trim().is_empty() {
            return Err("file delivery needs a session with a working directory".into());
        }
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
        let src = self.source_for_delivery(session, path, &source, workspaces)?;
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(300), read_source(&src))
            .await
            .map_err(|_| "delivery transfer timed out before complete receipt")??;
        if let Source::Execution(_, q) = &src {
            crate::wire::node_tools::authorize(&self.store, &q.workspace.node, &q.execution, true)
                .map_err(|e| e.safe_detail)?;
            let current = workspaces
                .placement_of(Subject::Session(&session.id))
                .map_err(|e| e.to_string())?;
            if current.at != At::Workspace(q.workspace.clone()) {
                return Err("delivery workspace binding changed during transfer".into());
            }
        }
        let (service, session, title, entrypoint) = (
            self.clone(),
            session.clone(),
            title.unwrap_or(&name).to_string(),
            entrypoint.map(str::to_string),
        );
        tokio::task::spawn_blocking(move || {
            service.publish(
                super::Publisher::Session(&session),
                &title,
                &name,
                &bytes,
                entrypoint.as_deref(),
                interactive,
            )
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Select the authenticated Node source; Hub-owned outputs retain their own root boundary.
    fn source_for_delivery(
        &self,
        session: &Session,
        path: &str,
        source: &Path,
        workspaces: &Workspaces,
    ) -> Result<Source, String> {
        let durable = match session.item_id.as_deref() {
            Some(item) => Some(
                crate::wire::output_files::directory(&self.home, item)
                    .map_err(|e| e.to_string())?,
            ),
            None => None,
        };
        Ok(if durable.as_ref().is_some_and(|d| source.starts_with(d)) {
            // Hub-owned outputs always use this machine's Node, even for a remote session.
            // Anchor above item/output directories so the Node rejects links in those components.
            let root = self.home.join("workspaces");
            workspaces.refresh_roots().map_err(|e| e.to_string())?;
            let placement = workspaces
                .hub_path(&root)
                .map_err(|e| format!("delivery Node unavailable: {e}"))?;
            let rel = session_relative(&root, &source)?;
            let At::Root(base) = placement.at else {
                return Err("Hub outputs require a Node root".into());
            };
            root_source(placement.node, base, rel)?
        } else {
            // Bound workspace paths belong to the Node, not sessions.cwd (which may be a
            // Hub scratch placeholder). Only the authenticated current execution can name them.
            workspaces.refresh_roots().map_err(|e| e.to_string())?;
            match workspaces.placement_of(Subject::Session(&session.id)) {
                Ok(p) => match p.at {
                    At::Workspace(workspace) => {
                        let (node, execution) = crate::wire::node_tools::delivery_execution()
                            .ok_or("workspace delivery requires a scoped Node execution")?;
                        if node != p.node_id || workspace.node != node {
                            return Err("delivery execution belongs to another Node".into());
                        }
                        let (current, _) = crate::wire::node_tools::authorize(
                            &self.store,
                            &node,
                            &execution,
                            true,
                        )
                        .map_err(|e| e.safe_detail)?;
                        if current.id != session.id {
                            return Err("delivery execution belongs to another session".into());
                        }
                        Source::Execution(
                            p.node,
                            ExecutionDeliveryRead {
                                execution,
                                workspace,
                                location: path.to_string(),
                                reply: false,
                                expected: None,
                                offset: 0,
                                max: CHUNK,
                            },
                        )
                    }
                    At::Root(_) => {
                        return Err("delivery has no valid Node workspace".into());
                    }
                },
                Err(e) => return Err(format!("delivery Node unavailable: {e}")),
            }
        })
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
        if d.reply {
            return Err("reply resources belong to their original session".into());
        }
        // Only the exact page URL. Do not infer an association from a path on an arbitrary
        // origin, or swallow a wrong selector.
        if location != self.url(&d.id, &d.workspace_id) {
            return Err("invalid delivery URL or workspace selector".into());
        }
        // A coordinator may include an upstream child's delivery in its own outputs. Reuse
        // the exact immutable version in the same workspace; its original item_id stays intact.
        // It remains an ordinary link: artifacts attach delivery_id only within the SAME item.
        Ok(Some(d))
    }
}

fn session_relative(cwd: &Path, source: &Path) -> Result<RelPath, String> {
    let rel = source
        .strip_prefix(cwd)
        .map_err(|_| "delivery must be inside the session directory; package folders as ZIP")?;
    RelPath::new(rel.to_str().ok_or("delivery path must be UTF-8")?).map_err(|e| e.to_string())
}

fn root_source(
    node: std::sync::Arc<dyn steve_node_api::Node>,
    base: RootPath,
    rel: RelPath,
) -> Result<Source, String> {
    let rel = RelPath::new(if base.rel.as_str().is_empty() {
        rel.as_str().into()
    } else {
        format!("{}/{}", base.rel.as_str(), rel.as_str())
    })
    .map_err(|e| e.to_string())?;
    Ok(Source::Node(
        node,
        FileAddress::Root(RootPath {
            root: base.root,
            rel,
        }),
    ))
}

async fn read_source(src: &Source) -> Result<Vec<u8>, String> {
    let mut recv = Receive::default();
    loop {
        let offset = recv.bytes.len() as u64;
        let expected = recv.version.clone();
        let chunk = match src {
            Source::Execution(node, query) => {
                let mut q = query.clone();
                q.expected = expected;
                q.offset = offset;
                node.read_execution_delivery(q)
                    .await
                    .map_err(|e| format!("Node execution delivery refused: {}", e.safe_detail))?
            }
            Source::Node(node, path) => node
                .read_delivery(FileRead {
                    path: path.clone(),
                    expected,
                    offset,
                    max: CHUNK,
                })
                .await
                .map_err(|e| format!("Node delivery refused: {}", e.safe_detail))?,
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
            if self.mode == 5 {
                assert_eq!(
                    method,
                    steve_node_wire::methods::EXECUTION_DELIVERY_READ,
                    "an execution delivery never falls back to workspace or general fs.read"
                );
                return Err(Error::new(Code::Unsupported, "execution delivery refused"));
            }
            assert_eq!(
                method,
                steve_node_wire::methods::DELIVERY_READ,
                "no fallback to general fs.read"
            );
            if self.mode == 1 {
                return Err(Error::new(Code::Unsupported, "delivery read refused"));
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
                return Err(Error::new(Code::Unavailable, "link cut"));
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
        let source = if mode == 5 {
            Source::Execution(
                remote.clone(),
                ExecutionDeliveryRead {
                    execution: ExecutionRef::Turn {
                        session: SessionId::new("s").unwrap(),
                        turn: TurnId::new("t").unwrap(),
                        attempt: AttemptId::new("a").unwrap(),
                    },
                    workspace: WorkspaceRef {
                        node: remote.id().clone(),
                        id: WorkspaceId::new("w").unwrap(),
                    },
                    location: "/actual/node/workspace/report.md".into(),
                    reply: false,
                    expected: None,
                    offset: 0,
                    max: CHUNK,
                },
            )
        } else {
            Source::Node(
                remote.clone(),
                FileAddress::Root(RootPath {
                    root: RootId::new("outputs").unwrap(),
                    rel: RelPath::new("report.md").unwrap(),
                }),
            )
        };
        let result = read_source(&source).await;
        node.close(None, 1000, "test complete");
        remote.detach(1);
        result
    }

    #[tokio::test]
    async fn missing_node_never_falls_back_to_reading_local_files() {
        let (_home, svc, session) = super::super::tests::fixture();
        std::fs::write(Path::new(&session.cwd).join("report.md"), b"local bytes").unwrap();
        let ws = Workspaces::new(svc.store.clone(), None, None, vec![]);
        let error = svc
            .receive(&session, "report.md", None, None, &ws)
            .await
            .unwrap_err();
        assert!(error.starts_with("delivery Node unavailable:"), "{error}");
        let count: i64 = svc
            .store
            .read(steve_store::testkit::hub::deliveries::count)
            .unwrap();
        assert_eq!(count, 0);
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
        for mode in 1..=5 {
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
