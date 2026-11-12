//! Steve's tools, independent of any tool protocol: the Steve tools
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
    super::diagnostics::TOOL,
    "steve_items_search",
    "steve_item_read",
    steve_mcp::tools::LANE_STATUS,
    "steve_event_get",
    "steve_knowledge_search",
    "steve_knowledge_get",
    "steve_contacts_search",
    "steve_contact_get",
    "steve_im_read",
];

/// The most a tool's text may take once JSON-escaped: its answer to a Node goes in one frame
/// ([`steve_node_wire::envelope::MAX_FRAME`]), and the envelope around it needs the rest.
pub const RESULT_CAP: usize = steve_node_wire::envelope::BULK_CAP;

/// `text` as it fits in one answer: unchanged when it does, else its beginning and a note of what
/// was cut and how to get the rest. Any tool's result, so that none fails only for its size.
pub fn bound_result(tool: &str, text: String) -> String {
    bound_to(tool, text, RESULT_CAP)
}

fn bound_to(tool: &str, text: String, cap: usize) -> String {
    let escaped = |c: char| match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    };
    let size: usize = text.chars().map(escaped).sum();
    if size <= cap {
        return text;
    }
    let note = |kept: usize| {
        format!(
            "\n\n[Steve cut this {tool} result: it was {} bytes, more than the {cap} one tool \
             result may carry; only the first {kept} bytes are above. To read the rest, call {tool} \
             again for less at a time: a narrower query, a smaller limit or page, or one id or \
             field at a time.]",
            text.len()
        )
    };
    // the note's own size, with room for the numbers in it
    let room = cap.saturating_sub(note(text.len()).chars().map(escaped).sum::<usize>() + 16);
    let (mut used, mut end) = (0, 0);
    for (i, c) in text.char_indices() {
        used += escaped(c);
        if used > room {
            break;
        }
        end = i + c.len_utf8();
    }
    let mut out = text[..end].to_string();
    out.push_str(&note(end));
    out
}

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
        let observation = super::diagnostics::ToolObservation::new(&self.mcp.store, &s, name);
        observation.run(self.call_inner(s, name, args)).await
    }

    async fn call_inner(
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

    #[test]
    fn a_result_over_the_cap_is_cut_and_says_so() {
        assert_eq!(bound_to("steve_x", "short".into(), 100), "short");
        let big = "ab\"\n".repeat(400_000);
        let out = bound_result("steve_event_get", big.clone());
        let json = serde_json::to_string(&out).unwrap();
        assert!(json.len() <= RESULT_CAP, "{} escaped bytes", json.len());
        assert!(big.starts_with(out.split("\n\n[Steve cut").next().unwrap()));
        assert!(
            out.contains(&format!("it was {} bytes", big.len())),
            "{out}"
        );
        assert!(out.contains("call steve_event_get again for less"), "{out}");
        // multi-byte text is cut on a character boundary
        let wide = "工作图".repeat(200_000);
        let out = bound_result("steve_workflow", wide);
        assert!(serde_json::to_string(&out).unwrap().len() <= RESULT_CAP);
        assert!(out.starts_with("工作图"));
    }
}
