//! Platform-neutral connector operations. The websocket adapter supplies these from its handshake;
//! the Hub authorizes the connection and binds Hub-issued conversation/message/reference IDs before
//! calling it. Tool names and reference IDs are deterministic module output, never model inventions.
//! Reads are explicitly requested. This module never schedules a history backfill or retries a write.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_modules::accounts::connections::ConnCx;
use steve_modules::{ApiError, ApiResult, Caller};
use steve_store::base::Viewer;

use crate::{Channels, spaces};

/// The meaning of an identifier argument. Plugins receive external IDs only after the Hub has
/// resolved these against stored, visible objects of this exact connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    Conversation,
    Message,
    Topic,
    Reference,
}

impl Binding {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Message => "message",
            Self::Topic => "topic",
            Self::Reference => "reference",
        }
    }
}

/// Discovery declares capabilities, never implies permission or schedules a read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Search,
    History,
    Read,
    Write,
}

/// Standard MCP definition plus Steve's resource/as-you annotations and explicit ID bindings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub name: String,
    pub description: String,
    pub capability: Capability,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(default)]
    pub annotations: Value,
    /// Top-level argument name → required object kind. Every bound value is a Hub ID, not a
    /// platform ID. A search operation may have none; a targeted read/write must bind its target.
    #[serde(default)]
    pub bindings: BTreeMap<String, Binding>,
}

impl Operation {
    pub fn read_only(&self) -> bool {
        self.annotations["readOnlyHint"].as_bool() == Some(true)
    }
    pub fn as_you(&self) -> bool {
        self.annotations["steve"]["asYou"]
            .as_bool()
            .unwrap_or(!self.read_only())
    }
}

/// An optional `connector` in the existing ext_ws v1 hello. The extension is versioned separately
/// so a legacy messaging bridge is not mistaken for a generic connector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub id: String,
    #[serde(default)]
    pub operations: Vec<Operation>,
    #[serde(default)]
    pub events: Vec<String>,
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}

impl Manifest {
    pub fn parse(v: &Value) -> Result<Self, String> {
        if v.to_string().len() > 256 * 1024 {
            return Err("connector declaration exceeds 256 KiB".into());
        }
        let m: Self = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        if m.version != 1 || !name_ok(&m.id) || m.operations.len() > 64 || m.events.len() > 64 {
            return Err("invalid connector version/id or declaration limits".into());
        }
        let mut names = std::collections::HashSet::new();
        for op in &m.operations {
            if !name_ok(&op.name)
                || !names.insert(&op.name)
                || op.description.len() > 8192
                || !op.input_schema.is_object()
                || !op.annotations.is_object()
                || op.input_schema["type"] != "object"
                || op.input_schema["additionalProperties"] != false
                || op.read_only() == (op.capability == Capability::Write)
            {
                return Err("invalid or duplicate connector operation".into());
            }
            for arg in op.bindings.keys() {
                if !name_ok(arg)
                    || op.input_schema["properties"].get(arg).is_none()
                    || op.input_schema["properties"][arg]["type"] != "string"
                {
                    return Err(format!("{} has an undeclared binding {arg}", op.name));
                }
            }
            if op.capability != Capability::Search
                && (op.bindings.is_empty()
                    || !op.bindings.keys().any(|arg| {
                        op.input_schema["required"]
                            .as_array()
                            .is_some_and(|r| r.iter().any(|v| v == arg))
                    }))
            {
                return Err(format!(
                    "targeted operation {} must require a bound conversation, message or reference",
                    op.name
                ));
            }
        }
        if m.events.iter().any(|e| {
            e.is_empty()
                || e.len() > 64
                || !e
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        }) {
            return Err("invalid connector event type".into());
        }
        Ok(m)
    }
}

/// Registered by a channel side. The same object owns its live websocket sessions.
#[async_trait]
pub trait ConnectorTools: Send + Sync {
    fn adapter_id(&self) -> &str;
    async fn catalog(&self, cx: &ConnCx) -> Result<Option<Manifest>, String>;
    /// Arguments are schema-checked and IDs resolved by Channels first. Writes must not be
    /// transparently replayed after a timeout/disconnect; `key` is an explicit request identity.
    async fn call(
        &self,
        cx: &ConnCx,
        op: &Operation,
        args: Value,
        key: &str,
    ) -> Result<Value, String>;
}

/// One operation on one visible account, for the Hub's unified Source::Connector registry.
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    pub connector: String,
    /// A connection ID, not a credential or a caller-supplied account name.
    pub account: String,
    pub operation: Operation,
}

pub fn tool_name(connection: &str, operation: &str) -> String {
    let digest = hex::encode(Sha256::digest(
        format!("{connection}\0{operation}").as_bytes(),
    ));
    format!(
        "connector_{}__{}",
        &digest[..12],
        &operation[..operation.len().min(40)]
    )
}

/// Stable identity for a reference stored with a message. Opaque keys remain in the module/store;
/// callers use the result, looked up against the connection and visible message before reading.
pub fn reference_id(connection: &str, chat: &str, message: &str, key: &str) -> String {
    let digest = hex::encode(Sha256::digest(
        json!([connection, chat, message, key])
            .to_string()
            .as_bytes(),
    ));
    format!("ref_{}", &digest[..32])
}

impl Channels {
    pub fn connector(&self, adapter: &str) -> Option<Arc<dyn ConnectorTools>> {
        self.connector_views().get(adapter).cloned()
    }

    /// Lists only connections visible under the ordinary account visibility rules. Catalog reads
    /// do not subscribe, search history, identify people or import any remote content.
    pub async fn connector_tools(&self, who: &Caller) -> ApiResult<Vec<Tool>> {
        let viewer = self.modules().viewer(who)?;
        let connections = self
            .connections()
            .list(who)
            .map_err(|e| ApiError::invalid(e.to_string()))?;
        let mut out = Vec::new();
        for row in connections
            .into_iter()
            .filter(|r| matches!(r.status.as_str(), "ok" | "error"))
        {
            let id = row.id.clone();
            let Some(provider) = self.connector(&row.adapter) else {
                continue;
            };
            let cx = self
                .connections()
                .cx_for(&viewer.ws, &id, None)
                .map_err(|e| ApiError::invalid(e.to_string()))?;
            match provider.catalog(&cx).await {
                Ok(Some(m)) => {
                    for operation in m.operations {
                        if !operation.read_only()
                            && !row.caps.iter().any(|c| c == "msg.send_as_user")
                        {
                            continue;
                        }
                        out.push(Tool {
                            name: tool_name(&id, &operation.name),
                            connector: m.id.clone(),
                            account: id.clone(),
                            operation,
                        });
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(connection=%id, "connector catalog unavailable: {e}"),
            }
        }
        Ok(out)
    }

    /// Re-discovers the current definition; a cached descriptor never grants account access.
    pub async fn connector_call(
        &self,
        who: &Caller,
        name: &str,
        args: Value,
        key: &str,
    ) -> Result<Value, String> {
        let viewer = self.modules().viewer(who).map_err(|e| e.to_string())?;
        let tool = self
            .connector_tools(who)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|t| t.name == name)
            .ok_or("connector tool not available")?;
        let validator = jsonschema::validator_for(&tool.operation.input_schema)
            .map_err(|e| format!("invalid inputSchema: {e}"))?;
        let errors: Vec<_> = validator
            .iter_errors(&args)
            .map(|e| e.to_string())
            .collect();
        if !errors.is_empty() {
            return Err(format!(
                "invalid connector arguments: {}",
                errors.join("; ")
            ));
        }
        let args = self
            .bind_connector_args(&viewer, &tool.account, &tool.operation, args)
            .map_err(|e| e.to_string())?;
        // Re-check status/capabilities immediately before dispatch, not only during catalog lookup.
        let row = self
            .connections()
            .get(who, &tool.account)
            .map_err(|e| e.to_string())?;
        let cap = (!tool.operation.read_only()).then_some("msg.send_as_user");
        let cx = self
            .connections()
            .cx_for(&viewer.ws, &tool.account, cap)
            .map_err(|e| e.to_string())?;
        let provider = self
            .connector(&row.adapter)
            .ok_or("connector provider unavailable")?;
        let result = provider.call(&cx, &tool.operation, args, key).await?;
        self.register_connector_result(&viewer, &tool.account, &row.account_id, result)
            .map_err(|e| e.to_string())
    }

    fn bind_connector_args(
        &self,
        viewer: &Viewer,
        conn: &str,
        op: &Operation,
        mut args: Value,
    ) -> ApiResult<Value> {
        self.store().reader(|c| {
            let account: String = c.query_row("SELECT account_id FROM connections WHERE workspace_id=? AND id=?", params![viewer.ws,conn], |r| r.get(0))?;
            for (arg, kind) in &op.bindings {
                let Some(value) = args.get(arg).filter(|v| !v.is_null()) else { continue };
                let id = value.as_str().ok_or_else(|| ApiError::invalid("bound ID must be a string"))?;
                // A result reference was issued by this Hub on this exact account, not by the
                // plugin or model. Resolve it without interpreting any external ID as a Hub ID.
                let reference: Option<(String,String,String,i64)> = c.query_row(
                    "SELECT external_id,CASE WHEN kind='conversation' THEN external_id ELSE conversation_id END,kind,expires FROM connector_refs WHERE id=? AND workspace_id=? AND connection_id=? AND account_id=?",
                    params![id,viewer.ws,conn,account], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
                if let Some((external, conversation, stored_kind, expires)) = reference {
                    if expires <= self.now() { return Err(ApiError::invalid("connector reference expired; search or read again")); }
                    if stored_kind != kind.code() { return Err(ApiError::invalid("connector reference type mismatch")); }
                    self.check_connector_conversation(c, viewer, conn, &conversation)?;
                    args[arg] = external.into();
                    continue;
                }
                let (space, external) = match kind {
                    Binding::Conversation => (id.to_string(), None),
                    Binding::Message | Binding::Topic => {
                        let row: Option<(String, Option<String>, Option<String>, Option<String>)> = c.query_row(
                            "SELECT m.space_id, m.external_id, m.thread_key, x.source_id FROM chan_messages m JOIN chan_message_seen x ON x.message_id=m.id AND x.connection_id=?2 WHERE m.id=?1 AND m.purged IS NULL", params![id,conn],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
                        let (space, message, topic, source) = row.ok_or_else(|| ApiError::not_found("message"))?;
                        // `external_id` is not NULL for messages without a provider ID: it is a
                        // Hub-only fp:… key. Use the common filter before this connection's source ID.
                        let message = crate::fallback::platform(message).or_else(|| crate::fallback::platform(source));
                        (space, Some(if *kind == Binding::Topic { topic } else { message }.filter(|s| !s.is_empty()).ok_or_else(|| ApiError::invalid("message has no external identifier"))?))
                    },
                    Binding::Reference => {
                        // Search only messages supplied through this connection. Opaque IDs cannot
                        // select another account's reference even when the spaces are shared.
                        let mut st = c.prepare("SELECT m.space_id,m.attachments FROM chan_messages m JOIN chan_message_seen d ON d.message_id=m.id JOIN json_each(m.attachments) a WHERE d.connection_id=? AND m.purged IS NULL AND json_extract(a.value,'$.reference_id')=? LIMIT 1")?;
                        let rows = st.query_map(params![conn, id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                        let found = rows.into_iter().find_map(|(space, a)| {
                            let list: Value = serde_json::from_str(&a).ok()?;
                            let key = list.as_array()?.iter().find(|a| a["reference_id"] == id)?["key"].as_str()?.to_string();
                            Some((space, Some(key)))
                        });
                        found.ok_or_else(|| ApiError::not_found("reference"))?
                    }
                };
                let visible = spaces::own(c, viewer, &space)?;
                let linked: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM space_identities WHERE space_id=? AND connection_id=? AND left_at IS NULL)", params![space,conn], |r| r.get(0))?;
                if !linked && visible.via != conn { return Err(ApiError::not_found("conversation")); }
                let settings = spaces::settings(c, &space)?;
                if settings.disclosure == "never" || settings.attention == spaces::Attention::Blocked { return Err(ApiError::not_found("conversation")); }
                self.check_connector_conversation(c, viewer, conn, &visible.external_id)?;
                args[arg] = json!(external.unwrap_or(visible.external_id));
            }
            Ok(args)
        })
    }
    fn check_connector_conversation(
        &self,
        c: &rusqlite::Connection,
        viewer: &Viewer,
        conn: &str,
        external: &str,
    ) -> ApiResult<()> {
        if external.is_empty() {
            return Ok(());
        }
        if let Some(space) = spaces::find(c, &viewer.ws, conn, external)? {
            spaces::own(c, viewer, &space.id)?;
            let settings = spaces::settings(c, &space.id)?;
            if settings.disclosure == "never" || settings.attention == spaces::Attention::Blocked {
                return Err(ApiError::not_found("conversation"));
            }
        }
        Ok(())
    }

    /// Result references are registered only after an explicit operation. They are not messages,
    /// members or history, and produce no flows. A plugin cannot choose a Hub reference_id.
    fn register_connector_result(
        &self,
        viewer: &Viewer,
        conn: &str,
        account: &str,
        mut result: Value,
    ) -> ApiResult<Value> {
        if result.to_string().len() > 1024 * 1024 {
            return Err(ApiError::invalid("connector result too large"));
        }
        let Some(refs) = result.get("references") else {
            return Ok(result);
        };
        let refs = refs
            .as_array()
            .filter(|a| a.len() <= 200)
            .ok_or_else(|| ApiError::invalid("invalid connector references"))?
            .clone();
        let now = self.now();
        let public = self.store().transaction(|tx| {
            // The call may have taken time: a disconnect/account change revokes its results too.
            let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM connections WHERE workspace_id=? AND id=? AND account_id=? AND status IN ('ok','error'))",params![viewer.ws,conn,account], |r| r.get(0))?;
            if !active { return Err(ApiError::not_found("connector connection")); }
            let mut out = Vec::new();
            for r in &refs {
                let kind: Binding = serde_json::from_value(r["kind"].clone()).map_err(|_| ApiError::invalid("invalid reference kind"))?;
                let external = external_field(r,"external_id",true)?;
                let conversation = external_field(r,"conversation_id",false)?;
                let source = external_field(r,"source_message_id",false)?;
                let original = external_field(r,"original_type",true)?;
                self.check_connector_conversation(tx, viewer, conn, if kind == Binding::Conversation { &external } else { &conversation })?;
                let expires = r["expires_ms"].as_i64().unwrap_or(now + 24*3_600_000).min(now + 30*24*3_600_000);
                if expires <= now { return Err(ApiError::invalid("connector reference already expired")); }
                let id = reference_id(conn, account, &conversation, &json!([kind.code(),external]).to_string());
                let mut metadata = json!({"reference_id": id, "kind": kind, "original_type": original,
                    "source": {"connector_account": conn, "conversation_external_id": conversation, "message_external_id": source}, "expires_ms": expires});
                for key in ["name","title","mime","size"] {
                    if let Some(v) = r.get(key).filter(|v| v.is_u64() || v.as_str().is_some_and(|s| s.len() <= 1024)) { metadata[key] = v.clone(); }
                }
                if let Some(url) = r["url"].as_str().filter(|u| u.len() <= 2048 && url::Url::parse(u).is_ok_and(|u| matches!(u.scheme(),"http"|"https") && u.username().is_empty() && u.password().is_none())) { metadata["url"] = url.into(); }
                tx.execute("INSERT INTO connector_refs (id,workspace_id,connection_id,account_id,kind,external_id,conversation_id,source_message_id,original_type,metadata,expires,created,updated) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET metadata=excluded.metadata,expires=excluded.expires,updated=excluded.updated",
                    params![id,viewer.ws,conn,account,kind.code(),external,conversation,source,original,metadata.to_string(),expires,now,now])?;
                out.push(metadata);
            }
            Ok(out)
        })?;
        result["references"] = json!(public);
        Ok(result)
    }
}

fn external_field(v: &Value, key: &str, required: bool) -> ApiResult<String> {
    let s = match v.get(key) {
        Some(Value::String(s)) => s.as_str(),
        None | Some(Value::Null) if !required => "",
        _ => return Err(ApiError::invalid(format!("invalid {key}"))),
    };
    if (required && s.is_empty()) || s.len() > 512 || s.chars().any(char::is_control) {
        return Err(ApiError::invalid(format!("invalid {key}")));
    }
    Ok(s.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deterministic_names_and_scoped_references() {
        assert_eq!(
            tool_name("account-a", "read"),
            tool_name("account-a", "read")
        );
        assert_ne!(
            tool_name("account-a", "read"),
            tool_name("account-b", "read")
        );
        assert_ne!(
            reference_id("a", "chat", "message", "key"),
            reference_id("b", "chat", "message", "key")
        );
        assert!(tool_name("a", &"x".repeat(64)).len() <= 64);
    }
    #[test]
    fn declarations_cannot_hide_unbound_writes() {
        let mut m = json!({"version":1,"id":"example","operations":[{"name":"write","description":"write","capability":"write","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"annotations":{"readOnlyHint":false}}]});
        assert!(Manifest::parse(&m).is_err());
        m["operations"][0]["capability"] = "search".into();
        m["operations"][0]["annotations"]["readOnlyHint"] = true.into();
        assert!(Manifest::parse(&m).is_ok());
        m["operations"][0]["inputSchema"]["additionalProperties"] = true.into();
        assert!(Manifest::parse(&m).is_err());
    }
}
