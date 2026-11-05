//! Steve's tools, independent of any tool protocol (docs/hub-node-client.md §4): the five Steve tools
//! (steve-mcp) and the modules' tools (`mcp_extra`), listed and run for one session. The Hub's MCP
//! endpoint is one way to reach them (for harnesses the in-process runner starts); a Node's
//! `tool.call` is the other. Both get the same definitions, checks and texts.

use std::sync::Arc;

use serde_json::Value;
use steve_node_api::ToolDescriptor;

use super::core::CoreMcp;
use super::mcp_extra::{self, Extra};

pub struct ToolService {
    pub mcp: Arc<CoreMcp>,
    pub extra: Extra,
}

/// Tools the agent only reads with (they change nothing).
const READ_ONLY: &[&str] = &[
    "steve_event_get",
    "steve_knowledge_search",
    "steve_knowledge_get",
    "steve_contacts_search",
    "steve_contact_get",
];

impl ToolService {
    /// Every tool, in the order the MCP endpoint lists them.
    pub fn catalog(&self) -> Vec<ToolDescriptor> {
        let mut out: Vec<ToolDescriptor> = steve_mcp::tools::list()
            .into_iter()
            .map(|t| ToolDescriptor {
                read_only: READ_ONLY.contains(&t.name.as_ref()),
                name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema: Value::Object((*t.input_schema).clone()),
            })
            .collect();
        out.extend(mcp_extra::module_tools().into_iter().map(|t| {
            let name = t["name"].as_str().unwrap_or_default().to_string();
            ToolDescriptor {
                read_only: READ_ONLY.contains(&name.as_str()),
                name,
                description: t["description"].as_str().unwrap_or_default().to_string(),
                input_schema: t["inputSchema"].clone(),
            }
        }));
        out
    }

    /// Runs `name` for session `s`: the success text, or the error text the agent reads.
    pub async fn call(
        &self,
        s: steve_store::Session,
        name: &str,
        args: Value,
    ) -> Result<String, String> {
        if let Some(r) = mcp_extra::call_module(&self.extra, s.clone(), name, args.clone()).await {
            return r;
        }
        if steve_mcp::handles(name) {
            let args = match args {
                Value::Object(m) => m,
                _ => serde_json::Map::new(),
            };
            return steve_mcp::run(&*self.mcp, &s, name, &args).await;
        }
        Err(format!("MCP error -32602: Tool {name} not found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names `tools/list` of the Hub's MCP endpoint gives: the five, then the modules'.
    fn mcp_names() -> Vec<String> {
        let mut v: Vec<String> = steve_mcp::tools::list()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        v.extend(
            mcp_extra::module_tools()
                .into_iter()
                .map(|t| t["name"].as_str().unwrap_or_default().to_string()),
        );
        v
    }

    #[test]
    fn the_catalog_is_what_the_mcp_endpoint_lists() {
        let names = mcp_names();
        assert!(names.contains(&"steve_update".to_string()));
        assert!(names.contains(&mcp_extra::REPLY_SOURCE.to_string()));
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "no tool twice");
        // every definition has a schema object
        for t in mcp_extra::module_tools() {
            assert!(t["inputSchema"].is_object(), "{t}");
        }
    }
}
