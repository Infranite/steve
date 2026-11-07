//! The method table: wire name ↔ port method, with its lane, deadline class and whether it carries
//! an operation id. [`crate::RemoteNode`], [`crate::NodeServer`], [`crate::remote_hub`] and
//! [`crate::ServeHub`] all use these constants, and `every_method_is_served` checks that each entry
//! reaches its port, so the two ends cannot drift.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use steve_node_api::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Commands, cancels, acks, tool calls, their answers and control messages: written first.
    Control,
    /// Event batches (a permission card is an event) and replayed events, both ways: after
    /// control, before bulk.
    Events,
    /// Blob and file chunks: last, in frames of at most [`crate::envelope::BULK_FRAME`]. No lane
    /// starves ([`crate::conn::FAIR`]). An answer travels on its request's lane.
    Bulk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A command: 15 s.
    Command,
    /// A read: 30 s.
    Read,
    /// `events.commit`: 30 s.
    Commit,
    /// `tool.call`: the call's own deadline plus 5 s (computed by the caller).
    Tool,
    /// Blob and file chunks: 60 s.
    Blob,
}

impl Class {
    pub fn deadline(self) -> Duration {
        Duration::from_secs(match self {
            Class::Command => 15,
            Class::Read | Class::Commit => 30,
            Class::Tool => 30,
            Class::Blob => 60,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    HubToNode,
    NodeToHub,
}

#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub name: &'static str,
    pub dir: Dir,
    pub lane: Lane,
    pub class: Class,
    /// Lost after it was written: `outcome_unknown` (ask, do not redo) rather than `offline`.
    pub carries_op: bool,
}

macro_rules! rpc_table {
    ($($konst:ident = $name:literal : $dir:ident $lane:ident $class:ident $op:literal;)*) => {
        $(pub const $konst: &str = $name;)*
        pub const ALL: &[Spec] = &[$(Spec {
            name: $name,
            dir: Dir::$dir,
            lane: Lane::$lane,
            class: Class::$class,
            carries_op: $op,
        }),*];
    };
}

rpc_table! {
    // SessionExecutor
    SESSION_OPEN = "session.open": HubToNode Control Command true;
    SESSION_CONFIGURE = "session.configure": HubToNode Control Command true;
    TURN_START = "turn.start": HubToNode Control Command true;
    SESSION_INSPECT = "session.inspect": HubToNode Control Read false;
    SESSION_RELEASE = "session.release": HubToNode Control Command true;
    // JobExecutor
    JOB_START = "job.start": HubToNode Control Command true;
    // ExecutionControl
    OP_INSPECT = "op.inspect": HubToNode Control Read false;
    EXEC_CANCEL = "exec.cancel": HubToNode Control Command true;
    EXEC_ABANDON = "exec.abandon": HubToNode Control Command true;
    EXEC_RECOVER = "exec.recover": HubToNode Control Command true;
    PERMISSION_RESOLVE = "permission.resolve": HubToNode Control Command true;
    NODE_SNAPSHOT = "node.snapshot": HubToNode Control Read false;
    NODE_STREAMS = "node.streams": HubToNode Control Read false;
    EVENTS_REPLAY = "events.replay": HubToNode Events Read false;
    EVENTS_ACK = "events.ack": HubToNode Control Read false;
    // WorkspaceExecutor
    WORKSPACE_PREPARE = "workspace.prepare": HubToNode Control Command true;
    WORKSPACE_INSPECT = "workspace.inspect": HubToNode Control Read false;
    WORKSPACE_TIDY = "workspace.tidy": HubToNode Control Command true;
    GIT_INSPECT = "git.inspect": HubToNode Control Read false;
    GIT_APPLY = "git.apply": HubToNode Control Command true;
    EXEC_START = "exec.start": HubToNode Control Command true;
    FS_LIST = "fs.list": HubToNode Control Read false;
    FS_LIST_ROOT = "fs.list_root": HubToNode Control Read false;
    FS_READ = "fs.read": HubToNode Bulk Blob false;
    TREE_SCAN = "tree.scan": HubToNode Control Command true;
    SCAN_PAGE = "scan.page": HubToNode Control Read false;
    // Discovery
    HARNESS_PROBE = "harness.probe": HubToNode Control Read false;
    HISTORY_LIST = "history.list": HubToNode Control Read false;
    HISTORY_IMPORT = "history.import": HubToNode Control Command true;
    IMPORT_PAGE = "import.page": HubToNode Control Read false;
    HISTORY_LOCATE = "history.locate": HubToNode Control Read false;
    // EventSink
    EVENTS_COMMIT = "events.commit": NodeToHub Events Commit false;
    // HubTools
    TOOL_CATALOG = "tool.catalog": NodeToHub Control Read false;
    TOOL_CALL = "tool.call": NodeToHub Control Tool true;
    TOOL_INSPECT = "tool.inspect": NodeToHub Control Read false;
    TOOL_CANCEL = "tool.cancel": NodeToHub Control Command true;
    // HubFiles
    BLOB_FETCH = "blob.fetch": NodeToHub Bulk Blob false;
    BLOB_BEGIN = "blob.begin": NodeToHub Control Blob true;
    BLOB_CHUNK = "blob.chunk": NodeToHub Bulk Blob false;
    BLOB_STATUS = "blob.status": NodeToHub Control Read false;
    BLOB_FINISH = "blob.finish": NodeToHub Control Blob false;
    BLOB_ABORT = "blob.abort": NodeToHub Control Blob false;
    // HubSecrets: one-shot, so a lost answer is not asked again (the operation fails instead)
    // one-shot: lost after it was written, the secret may be used up (never `offline`)
    SECRET_TAKE = "secret.take": NodeToHub Control Read true;
}

pub fn spec(name: &str) -> Option<&'static Spec> {
    ALL.iter().find(|s| s.name == name)
}

// Arguments of the multi-argument port methods; single-argument ones travel verbatim.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayArgs {
    pub from: Watermark,
    pub max_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListDirArgs {
    pub path: PathRef,
    pub page: PageRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListRootArgs {
    pub at: RootPath,
    pub page: PageRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PagesArgs {
    pub operation: OperationId,
    pub page: PageRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeArgs {
    pub fresh: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretArgs {
    pub operation: OperationId,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchArgs {
    pub access: FileAccess,
    pub id: ArtifactId,
    pub offset: u64,
    pub max: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_found() {
        let mut names: Vec<_> = ALL.iter().map(|s| s.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), ALL.len());
        assert!(spec(TURN_START).unwrap().carries_op);
        assert!(!spec(OP_INSPECT).unwrap().carries_op);
        assert_eq!(spec(EVENTS_COMMIT).unwrap().lane, Lane::Events);
        assert_eq!(spec(EVENTS_REPLAY).unwrap().lane, Lane::Events);
        assert_eq!(spec(BLOB_FETCH).unwrap().lane, Lane::Bulk);
        assert_eq!(spec(FS_READ).unwrap().lane, Lane::Bulk);
        assert_eq!(spec(PERMISSION_RESOLVE).unwrap().lane, Lane::Control);
        assert!(spec("nope").is_none());
    }
}
