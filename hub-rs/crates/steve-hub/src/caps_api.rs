//! `GET /api/capabilities/steve`: Steve itself as the built-in MCP server of 能力: the tools every
//! Bot's session has (with their descriptions), the ones a client outside Steve gets with an access
//! token, and where that client connects: `/mcp` at each address the Hub is configured to be
//! reached by (public first, then the local network, then this machine), never a host written into
//! the code. Also where Steve's own Agent Card is.

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use serde_json::{Value, json};

use crate::api::AppState;

fn first_sentence(d: &str) -> String {
    let end = d
        .char_indices()
        .find(|&(i, c)| c == '.' && d[i + 1..].starts_with([' ', '\n']) || c == '。')
        .map_or(d.len(), |(i, c)| i + c.len_utf8());
    d[..end].trim().to_string()
}

/// Every Steve tool a session can have, by name, with its first sentence, whether it reads or writes
/// and whether every session lists it (`core`); the core first, in its order.
pub fn session_tools() -> Vec<Value> {
    use crate::wire::tool_registry as reg;
    use steve_modules::caps::tools::CORE;
    let mut all = crate::wire::tool_service::builtin_catalog();
    all.push(reg::search_tool());
    all.push(reg::call_tool());
    let pos = |n: &str| CORE.iter().position(|c| *c == n).unwrap_or(CORE.len());
    all.sort_by_key(|t| pos(&t.name));
    reg::steve_entries(all)
        .into_iter()
        .map(|e| {
            let mut v = e.mcp();
            v["description"] = json!(first_sentence(&e.description));
            v["core"] = json!(CORE.contains(&e.name.as_str()));
            v["resources"] = json!(e.resources);
            v["as_you"] = json!(e.as_you);
            v
        })
        .collect()
}

pub async fn steve_h(State(st): State<AppState>, headers: HeaderMap) -> Json<Value> {
    let ep = crate::access::pair_endpoints(&st.cfg, &headers);
    let mut urls = vec![];
    if let Some(p) = &ep.public_url {
        urls.push(json!({"url": format!("{p}/mcp"), "kind": "public"}));
    }
    for l in &ep.lan_urls {
        urls.push(json!({"url": format!("{l}/mcp"), "kind": "lan"}));
    }
    urls.push(
        json!({"url": format!("http://127.0.0.1:{}/mcp", st.cfg.listen.port), "kind": "local"}),
    );
    let base = crate::a2a_server::base_url(&st.cfg, &headers);
    let outside: Vec<Value> = crate::wire::outside_mcp::tools()
        .into_iter()
        .map(|t| json!({"name": t["name"], "description": first_sentence(t["description"].as_str().unwrap_or(""))}))
        .collect();
    Json(json!({
        "tools": session_tools(),
        "core": steve_modules::caps::tools::CORE,
        "outside": outside,
        "outside_core": crate::wire::outside_mcp::CORE,
        "endpoints": urls,
        "agent_card": format!("{base}{}", crate::a2a_server::CARD_PATHS[0]),
        "a2a": format!("{base}{}", crate::a2a_server::PATH),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_description_is_cut_at_its_first_sentence() {
        assert_eq!(
            first_sentence("Read one item. Then more."),
            "Read one item."
        );
        assert_eq!(first_sentence("v1.2 is out. x"), "v1.2 is out.");
        assert_eq!(first_sentence("没有句号"), "没有句号");
        let names: Vec<String> = session_tools()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert!(
            names.contains(&"steve_deliver".to_string())
                && names.contains(&"steve_items_search".to_string())
        );
    }
}
