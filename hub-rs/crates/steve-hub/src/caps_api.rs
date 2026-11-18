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
            v["aliases"] = json!(crate::wire::tool_families::aliases(&e.name));
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

/// The skill center's workspace-scoped desired state and authenticated receipts.
pub async fn skill_distribution(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<crate::api::Ctx>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> crate::api::ApiResult {
    use crate::wire::bot_skills as skills;
    let modules = st
        .modules
        .clone()
        .ok_or_else(crate::error::ApiError::no_route)?;
    let links = st
        .nodes
        .as_ref()
        .map(|svc| {
            svc.registry
                .linked()
                .into_iter()
                .filter_map(|id| {
                    let link = skills::remote_link(svc.registry.current(&id)?);
                    Some((id, link))
                })
                .collect::<std::collections::HashMap<_, _>>()
        })
        .unwrap_or_default();
    let local = st.node.is_some();
    crate::api::blocking(move || {
        let mut links = links;
        let record = modules.store.reader::<_, steve_store::StoreError>(|conn| {
            let local_link = skills::local_link();
            if local && !links.contains_key("local") && conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM node_skill_inventory WHERE node_id='local' AND link=?)",
                [&local_link], |r| r.get::<_,bool>(0),
            )? {
                links.insert("local".into(), local_link);
            }
            skills::distribution(conn, &c.ws, &id, &c.me, &links, local)
        }).map_err(|e| {
            tracing::warn!(%e, "skill distribution read failed");
            crate::error::ApiError::internal(steve_i18n::t!("hub.skills.read_failed"))
        })?;
        crate::api::ok(record.ok_or_else(|| crate::error::ApiError::not_found(
            steve_i18n::t!("hub.skills.not_found"),
        ))?)
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_model_descriptions_are_english_and_locale_independent() {
        fn definitions() -> Value {
            json!({
                "session": crate::wire::tool_service::builtin_catalog().iter().map(|t| json!({"description":t.description,"schema":t.input_schema})).collect::<Vec<_>>(),
                "research": crate::wire::research_mcp::tools(),
                "outside": crate::wire::outside_mcp::tools(),
                "onboard": crate::wire::onboard_mcp::tools(),
            })
        }
        let zh = steve_i18n::with(steve_i18n::ZH_CN, definitions);
        let en = steve_i18n::with(steve_i18n::EN, definitions);
        assert_eq!(zh, en, "model contracts never follow the UI locale");
        fn walk(v: &Value) {
            match v {
                Value::Object(obj) => {
                    for (k, v) in obj {
                        if k == "description" {
                            let text = v.as_str().unwrap();
                            assert!(!text.is_empty());
                            assert!(
                                !text.chars().any(|c| ('\u{3400}'..='\u{9fff}').contains(&c)),
                                "non-English model description: {text}"
                            );
                        }
                        walk(v);
                    }
                }
                Value::Array(xs) => {
                    for x in xs {
                        walk(x);
                    }
                }
                _ => {}
            }
        }
        walk(&zh);
        let tools = session_tools();
        assert_eq!(tools.len(), 48);
        assert_eq!(tools.iter().filter(|t| t["core"] == true).count(), 8);
    }

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
