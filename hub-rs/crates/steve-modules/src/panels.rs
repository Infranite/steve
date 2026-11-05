//! Item panels: declarative cards a module shows in the item detail sidebar
//! (embedding point `item.sidebar`). steve-hub serves them (integrator):
//! `GET /api/items/:id/panels` → [`panels`], `POST /api/items/:id/panels/:panel/actions/:action` →
//! [`action`]. Panel ids start with their provider's id (`github.pr:ghp_…` belongs to `github`), which
//! is how an action finds its provider.

use async_trait::async_trait;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::{ApiError, ApiResult, Caller, Modules};

/// One panel, the JSON the web's generic `ItemPanels` component renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Panel {
    /// `<provider>.<kind>:<object id>`, e.g. `github.pr:ghp_…`.
    pub id: String,
    /// The provider ([`ItemPanel::id`]).
    pub plugin: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<PanelHead>,
    #[serde(default)]
    pub rows: Vec<PanelRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foot: Option<String>,
    #[serde(default)]
    pub actions: Vec<PanelAction>,
    /// Unix ms of the data shown.
    pub updated: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelHead {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// Short Chinese labels (“草稿”“已合并”“有冲突”“要求修改”).
    #[serde(default)]
    pub badges: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowState {
    Ok,
    Fail,
    Pending,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelRow {
    pub state: RowState,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

/// A button. With `href` it is a plain link the web opens itself; without, it posts to the action route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelAction {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

/// The answer of an action route: what to tell the user; the web reloads the panels afterwards.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ActionOutcome {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One provider of panels.
#[async_trait]
pub trait ItemPanel: Send + Sync {
    /// The provider id, also the prefix of its panel ids (`"github"`).
    fn id(&self) -> &'static str;

    /// The panels for an item the caller can see (ownership is checked by the route first).
    fn panels(&self, c: &Connection, ws: &str, me: &str, item_id: &str) -> ApiResult<Vec<Panel>>;

    /// Runs `action` of `panel`; unknown ones answer `404 没有这个操作`.
    async fn action(
        &self,
        m: &Modules,
        caller: &Caller,
        item_id: &str,
        panel: &str,
        action: &str,
    ) -> ApiResult<ActionOutcome>;
}

/// The provider id a panel id belongs to: the text before the first `.` or `:`.
pub fn provider_of(panel_id: &str) -> &str {
    panel_id.split(['.', ':']).next().unwrap_or(panel_id)
}

/// All providers' panels for an item, in registration order. Empty when nothing is registered.
pub fn panels(
    m: &Modules,
    c: &Connection,
    ws: &str,
    me: &str,
    item_id: &str,
) -> ApiResult<Vec<Panel>> {
    let mut out = Vec::new();
    for p in &m.registry.panels {
        out.extend(p.panels(c, ws, me, item_id)?);
    }
    Ok(out)
}

/// Dispatches an action to the provider owning `panel`; `404 没有这个面板` when none does.
pub async fn action(
    m: &Modules,
    caller: &Caller,
    item_id: &str,
    panel: &str,
    action: &str,
) -> ApiResult<ActionOutcome> {
    let id = provider_of(panel);
    let p = m
        .registry
        .panels
        .iter()
        .find(|p| p.id() == id)
        .ok_or_else(|| ApiError::not_found("面板"))?;
    p.action(m, caller, item_id, panel, action).await
}
