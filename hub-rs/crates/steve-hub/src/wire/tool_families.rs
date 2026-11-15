//! A few cohesive read operations share one public tool. The old operation names remain callable
//! for existing sessions and pins; authorization and side effects stay in their original handlers.
use serde_json::{Value, json};
use steve_node_api::ToolDescriptor;

struct Family {
    name: &'static str,
    description: &'static str,
    operations: &'static [(&'static str, &'static str)],
}

const FAMILIES: &[Family] = &[
    Family {
        name: "steve_contacts",
        description: "Look up people, organizations, teams and responsibility. Use find to search by name, alias, email or organization; whois to identify who owns or maintains an account, Bot, device, persona, connection or group; holdings to list a person's accounts and responsibilities. Results respect the caller's visibility. This tool does not save contacts or change ownership.",
        operations: &[
            ("find", "steve_contacts_find"),
            ("whois", "steve_whois"),
            ("holdings", "steve_person_holdings"),
        ],
    },
    Family {
        name: "steve_github_read",
        description: "Inspect the pull requests and CI checks linked to this item. Use status first, then ci_log with a check name or job ID to read a failed job's log. This tool does not post comments or open pull requests.",
        operations: &[
            ("status", "steve_github_status"),
            ("ci_log", "steve_github_ci_log"),
        ],
    },
    Family {
        name: "steve_mail_read",
        description: "Read this item's email context. Use thread to read its messages, senders, recipients, dates and attachment references; optionally specify a visible thread ID. Use attachment with an attachment ID to copy it into this session's working directory for reading. This tool does not create drafts or send email.",
        operations: &[
            ("thread", "steve_mail_thread"),
            ("attachment", "steve_mail_attachment"),
        ],
    },
    Family {
        name: "steve_matters",
        description: "Find or read cross-conversation matters before associating a digest segment. Use search with query to find existing matters, then read with an ID returned by Steve to inspect its timeline and visible memories. Never invent IDs; use memory or knowledge search for memories.",
        operations: &[
            ("search", "steve_matters_search"),
            ("read", "steve_matter_read"),
        ],
    },
];

pub fn canonical(name: &str) -> &str {
    FAMILIES
        .iter()
        .find(|f| f.operations.iter().any(|(_, old)| *old == name))
        .map_or(name, |f| f.name)
}

pub fn aliases(name: &str) -> Vec<&'static str> {
    FAMILIES
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.operations.iter().map(|(_, old)| *old).collect())
        .unwrap_or_default()
}

fn descriptor(f: &Family, raw: &[ToolDescriptor]) -> ToolDescriptor {
    let mut properties = serde_json::Map::new();
    let actions: Vec<_> = f.operations.iter().map(|(action, _)| *action).collect();
    properties.insert("action".into(), json!({"type": "string", "enum": actions}));
    let branches: Vec<_> = f
        .operations
        .iter()
        .map(|(action, old)| {
            let t = raw
                .iter()
                .find(|t| t.name == *old)
                .expect("family operation exists");
            assert!(
                t.read_only,
                "a read family must never inherit a write operation"
            );
            let mut schema = t.input_schema.clone();
            schema.as_object_mut().unwrap().remove("$schema");
            let fields = schema["properties"].as_object_mut().unwrap();
            properties.extend(fields.clone());
            fields.insert("action".into(), json!({"const": action}));
            let mut required = schema["required"].as_array().cloned().unwrap_or_default();
            required.push(json!("action"));
            schema["required"] = json!(required);
            schema["additionalProperties"] = json!(false);
            schema
        })
        .collect();
    ToolDescriptor {
        name: f.name.into(),
        description: f.description.into(),
        input_schema: json!({"type": "object", "properties": properties, "required": ["action"], "additionalProperties": false, "oneOf": branches}),
        read_only: true,
        annotations: Value::Null,
    }
}

pub fn catalog(raw: Vec<ToolDescriptor>) -> Vec<ToolDescriptor> {
    let mut out = vec![];
    for t in &raw {
        let name = canonical(&t.name);
        if name == t.name {
            out.push(t.clone());
        } else if !out.iter().any(|t: &ToolDescriptor| t.name == name) {
            out.push(descriptor(
                FAMILIES.iter().find(|f| f.name == name).unwrap(),
                &raw,
            ));
        }
    }
    out
}

/// Validate the full action-specific schema even on direct calls, before touching any handler.
pub fn resolve(
    name: &str,
    args: &Value,
    raw: &[ToolDescriptor],
) -> Result<Option<(&'static str, Value)>, String> {
    let Some(f) = FAMILIES.iter().find(|f| f.name == name) else {
        return Ok(None);
    };
    super::tool_registry::validate(&descriptor(f, raw).input_schema, args)
        .map_err(|e| format!("Invalid arguments for {name}: {}", e.join("; ")))?;
    let old = f
        .operations
        .iter()
        .find(|(action, _)| args["action"] == *action)
        .unwrap()
        .1;
    let mut forwarded = args.clone();
    forwarded.as_object_mut().unwrap().remove("action");
    Ok(Some((old, forwarded)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::tool_service::raw_builtin_catalog;

    #[test]
    fn old_pins_and_searches_resolve_to_one_read_entry() {
        let entries = super::super::tool_registry::steve_entries(catalog(raw_builtin_catalog()));
        let mut r = super::super::tool_registry::Registry {
            entries,
            ..Default::default()
        };
        let pins = aliases("steve_contacts")
            .iter()
            .map(|name| steve_store::caps::tools::Pin {
                kind: "tool".into(),
                target: (*name).into(),
            })
            .collect::<Vec<_>>();
        r.list(&pins);
        assert_eq!(
            r.listed.iter().filter(|n| *n == "steve_contacts").count(),
            1
        );
        for old in aliases("steve_contacts") {
            let (found, _) = super::super::tool_registry::search(
                &r.entries,
                &[],
                &super::super::tool_registry::Query {
                    text: old.into(),
                    limit: 1,
                    ..Default::default()
                },
            );
            assert_eq!(found[0].name, "steve_contacts");
            assert!(found[0].read_only);
            assert!(found[0].resources.iter().any(|r| r == "contact"));
        }
        for (query, expected) in [
            ("谁负责", "steve_contacts"),
            ("附件", "steve_mail_read"),
            ("CI 日志", "steve_github_read"),
            ("跨对话", "steve_matters"),
        ] {
            let (found, _) = super::super::tool_registry::search(
                &r.entries,
                &[],
                &super::super::tool_registry::Query {
                    text: query.into(),
                    limit: 20,
                    ..Default::default()
                },
            );
            assert!(
                found.iter().any(|e| e.name == expected),
                "{query} did not find {expected}"
            );
        }
        assert!(
            aliases("steve_github_comment").is_empty(),
            "outward writes remain separate"
        );
    }

    #[test]
    fn families_preserve_every_operation_and_reject_cross_action_fields() {
        let raw = raw_builtin_catalog();
        let cases = [
            (
                "steve_contacts",
                json!({"action":"find","query":"Ada"}),
                "steve_contacts_find",
            ),
            (
                "steve_contacts",
                json!({"action":"whois","query":"bot"}),
                "steve_whois",
            ),
            (
                "steve_contacts",
                json!({"action":"holdings","person":"me"}),
                "steve_person_holdings",
            ),
            (
                "steve_github_read",
                json!({"action":"status"}),
                "steve_github_status",
            ),
            (
                "steve_github_read",
                json!({"action":"ci_log","job":"build"}),
                "steve_github_ci_log",
            ),
            (
                "steve_mail_read",
                json!({"action":"thread"}),
                "steve_mail_thread",
            ),
            (
                "steve_mail_read",
                json!({"action":"attachment","id":"mla_test"}),
                "steve_mail_attachment",
            ),
            (
                "steve_matters",
                json!({"action":"search","query":"launch"}),
                "steve_matters_search",
            ),
            (
                "steve_matters",
                json!({"action":"read","id":"matter_test"}),
                "steve_matter_read",
            ),
        ];
        for (name, args, old) in cases {
            let (target, forwarded) = resolve(name, &args, &raw).unwrap().unwrap();
            assert_eq!(target, old);
            assert!(forwarded.get("action").is_none());
            let schema = &raw.iter().find(|t| t.name == old).unwrap().input_schema;
            assert!(super::super::tool_registry::validate(schema, &forwarded).is_ok());
            assert_eq!(canonical(old), name);
        }
        for (name, args) in [
            ("steve_contacts", json!({"action":"find","person":"Ada"})),
            (
                "steve_contacts",
                json!({"action":"whois","query":"Ada","role":"owner"}),
            ),
            (
                "steve_github_read",
                json!({"action":"status","job":"build"}),
            ),
            ("steve_github_read", json!({"action":"comment","body":"no"})),
            ("steve_github_read", json!({"action":"ci_log"})),
            ("steve_mail_read", json!({"action":"attachment"})),
            ("steve_mail_read", json!({"action":"thread","id":"x"})),
            ("steve_matters", json!({"action":"read","query":"x"})),
            ("steve_matters", json!({"action":"search","query":3})),
        ] {
            assert!(resolve(name, &args, &raw).is_err(), "{name}: {args}");
        }
        let merged = catalog(raw.clone());
        assert_eq!(merged.len(), raw.len() - 5);
        for f in FAMILIES {
            assert!(merged.iter().any(|t| t.name == f.name && t.read_only));
            for (_, old) in f.operations {
                assert!(!merged.iter().any(|t| t.name == *old));
            }
        }
    }
}
