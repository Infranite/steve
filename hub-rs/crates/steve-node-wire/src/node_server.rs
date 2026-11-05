//! The Node's side of the Hub→Node methods: each request decoded into its port argument and handed
//! to the Node (a `LocalNode`, or a fake in tests). The generation check happened in [`Conn`]:
//! a request of a stale connection never gets here.
//!
//! [`Conn`]: crate::Conn

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use steve_node_api::*;

use crate::conn::{Handler, from_value as de, to_value as en, unsupported};
use crate::methods::*;

pub struct NodeServer<N: Node + ?Sized> {
    node: Arc<N>,
}

impl<N: Node + ?Sized> NodeServer<N> {
    pub fn new(node: Arc<N>) -> Arc<Self> {
        Arc::new(NodeServer { node })
    }
}

#[async_trait]
impl<N: Node + ?Sized + 'static> Handler for NodeServer<N> {
    async fn handle(&self, method: &str, p: Value) -> ApiResult<Value> {
        let n = &self.node;
        match method {
            SESSION_OPEN => en(&n.open_session(de(p)?).await?),
            SESSION_CONFIGURE => en(&n.configure_session(de(p)?).await?),
            TURN_START => en(&n.start_turn(de(p)?).await?),
            SESSION_INSPECT => en(&n.session(de(p)?).await?),
            SESSION_RELEASE => en(&n.release_session(de(p)?).await?),
            JOB_START => en(&n.start_job(de(p)?).await?),
            OP_INSPECT => en(&n.operation(de(p)?).await?),
            EXEC_CANCEL => en(&n.cancel(de(p)?).await?),
            EXEC_ABANDON => en(&n.abandon(de(p)?).await?),
            EXEC_RECOVER => en(&n.recover(de(p)?).await?),
            PERMISSION_RESOLVE => en(&n.resolve_permission(de(p)?).await?),
            NODE_SNAPSHOT => en(&n.snapshot(de(p)?).await?),
            NODE_STREAMS => en(&n.streams().await?),
            EVENTS_REPLAY => {
                let a: ReplayArgs = de(p)?;
                en(&n.replay(a.from, a.max_bytes).await?)
            }
            EVENTS_ACK => en(&n.acknowledge(de(p)?).await?),
            WORKSPACE_PREPARE => en(&n.prepare_workspace(de(p)?).await?),
            WORKSPACE_INSPECT => en(&n.workspace(de(p)?).await?),
            WORKSPACE_TIDY => en(&n.tidy_workspace(de(p)?).await?),
            GIT_INSPECT => en(&n.git_facts(de(p)?).await?),
            GIT_APPLY => en(&n.git_apply(de(p)?).await?),
            EXEC_START => en(&n.exec(de(p)?).await?),
            FS_LIST => {
                let a: ListDirArgs = de(p)?;
                en(&n.list_dir(a.path, a.page).await?)
            }
            FS_LIST_ROOT => {
                let a: ListRootArgs = de(p)?;
                en(&n.list_root(a.at, a.page).await?)
            }
            FS_READ => en(&n.read_file(de(p)?).await?),
            TREE_SCAN => en(&n.scan(de(p)?).await?),
            SCAN_PAGE => {
                let a: PagesArgs = de(p)?;
                en(&n.scan_page(a.operation, a.page).await?)
            }
            HARNESS_PROBE => {
                let a: ProbeArgs = de(p)?;
                en(&n.probe(a.fresh).await?)
            }
            HISTORY_LIST => en(&n.history(de(p)?).await?),
            HISTORY_IMPORT => en(&n.import_history(de(p)?).await?),
            IMPORT_PAGE => {
                let a: PagesArgs = de(p)?;
                en(&n.import_page(a.operation, a.page).await?)
            }
            HISTORY_LOCATE => en(&n.locate(de(p)?).await?),
            other => Err(unsupported(other)),
        }
    }
}
