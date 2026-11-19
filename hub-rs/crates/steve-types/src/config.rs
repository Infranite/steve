//! Group 6 owns events and reasons for bots, projects, tags, workspaces, settings, changesets and memory.
//! Associated ID labels (BotId, ProjectId, TagId, WorkspaceId, MemoryId, ChangesetId) are all defined centrally in `id.rs`.

use crate::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(tag = "type")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ConfigEvent {
    #[serde(rename = "bot")]
    Bot { id: BotId, ws: WorkspaceId },
    #[serde(rename = "project")]
    Project { id: ProjectId, ws: WorkspaceId },
    #[serde(rename = "tag")]
    Tag { id: TagId, ws: WorkspaceId },
    #[serde(rename = "workspace")]
    Workspace { id: WorkspaceId, ws: WorkspaceId },
    #[serde(rename = "settings")]
    Settings { id: WorkspaceId, ws: WorkspaceId },
    #[serde(rename = "mm")]
    Mm { id: MemoryId, ws: WorkspaceId },
    #[serde(rename = "changeset")]
    Changeset { id: ChangesetId, ws: WorkspaceId },
}

impl Scoped for ConfigEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Bot { ws, .. } => Scope::Ws(ws.clone()),
            Self::Project { ws, .. } => Scope::Ws(ws.clone()),
            Self::Tag { ws, .. } => Scope::Ws(ws.clone()),
            Self::Workspace { ws, .. } => Scope::Ws(ws.clone()),
            Self::Settings { ws, .. } => Scope::Ws(ws.clone()),
            Self::Mm { ws, .. } => Scope::Ws(ws.clone()),
            Self::Changeset { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}

impl ConfigEvent {
    /// The public SSE tag for this domain event.
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::Bot { .. } => "bot",
            Self::Project { .. } => "project",
            Self::Tag { .. } => "tag",
            Self::Workspace { .. } => "workspace",
            Self::Settings { .. } => "settings",
            Self::Mm { .. } => "mm",
            Self::Changeset { .. } => "changeset",
        }
    }
}
