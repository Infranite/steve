//! The MCP servers of a session's Bot ([`OpenSession::mcp_servers`]) as ACP `mcpServers` entries,
//! for every harness alike. A secret value is taken from the Hub ([`HubSecrets::take`] under
//! [`mcp_secrets_op`]) each time the harness opens, kept in memory only and handed to the adapter in
//! `session/new|load`; it is never journaled, logged or put in an event.
//!
//! A server whose secret cannot be had is left out (with a warning in the Node's log) and the
//! session opens without it: one unreachable tool server does not keep the Bot from working.

use std::sync::Arc;

use serde_json::{Value, json};
use steve_node_api::*;
use zeroize::Zeroizing;

/// The ACP entries of `servers`, in order, without those whose secrets could not be taken.
pub(crate) async fn servers(
    hub: Option<Arc<dyn HubSecrets>>,
    session: &SessionId,
    servers: &[McpServer],
) -> Vec<Value> {
    let mut out = vec![];
    for s in servers {
        match server(hub.as_ref(), session, s).await {
            Ok(v) => out.push(v),
            Err(why) => tracing::warn!(
                session = %session,
                server = %s.name,
                "MCP server left out of the session: {why}"
            ),
        }
    }
    out
}

async fn server(
    hub: Option<&Arc<dyn HubSecrets>>,
    session: &SessionId,
    s: &McpServer,
) -> Result<Value, String> {
    match &s.transport {
        McpTransport::Stdio { command, args, env } => Ok(json!({
            "name": s.name,
            "command": command,
            "args": args,
            "env": vars(hub, session, env).await?,
        })),
        McpTransport::Http { url, headers } => Ok(json!({
            "type": "http",
            "name": s.name,
            "url": url,
            "headers": vars(hub, session, headers).await?,
        })),
        McpTransport::Sse { url, headers } => Ok(json!({
            "type": "sse",
            "name": s.name,
            "url": url,
            "headers": vars(hub, session, headers).await?,
        })),
    }
}

/// `[{name, value}]`, each secret taken now.
async fn vars(
    hub: Option<&Arc<dyn HubSecrets>>,
    session: &SessionId,
    vars: &[McpVar],
) -> Result<Vec<Value>, String> {
    let mut out = vec![];
    for v in vars {
        let value = match &v.secret {
            None => v.value.clone(),
            Some(id) => {
                let hub = hub.ok_or("no secrets from the Hub here")?;
                let bytes = hub
                    .take(mcp_secrets_op(session), id.clone())
                    .await
                    .map_err(|e| format!("secret of {}: {}", v.name, e.safe_detail))?;
                let bytes = Zeroizing::new(bytes.expose().to_vec());
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| format!("secret of {}: not text", v.name))?;
                if text.contains(['\n', '\r', '\0']) {
                    return Err(format!("secret of {}: more than one line", v.name));
                }
                text.to_string()
            }
        };
        out.push(json!({ "name": v.name, "value": value }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Hub(Mutex<Vec<(String, String)>>);

    #[async_trait::async_trait]
    impl HubSecrets for Hub {
        async fn take(&self, operation: OperationId, id: String) -> ApiResult<SecretBytes> {
            self.0
                .lock()
                .unwrap()
                .push((operation.to_string(), id.clone()));
            match id.as_str() {
                "bmcp_1#env:TOKEN" => Ok(SecretBytes::new(b"tok-123".to_vec())),
                "bmcp_2#header:Authorization" => Ok(SecretBytes::new(b"Bearer abc".to_vec())),
                "bmcp_3#header:X" => Ok(SecretBytes::new(b"a\nb".to_vec())),
                _ => Err(ApiError::new(ErrorCode::Denied, "no")),
            }
        }
    }

    fn var(name: &str, value: &str, secret: Option<&str>) -> McpVar {
        McpVar {
            name: name.into(),
            value: value.into(),
            secret: secret.map(Into::into),
        }
    }

    #[tokio::test]
    async fn every_transport_becomes_its_acp_entry_with_secrets_taken_now() {
        let hub = Arc::new(Hub(Mutex::new(vec![])));
        let session = SessionId::new("s_1").unwrap();
        let list = vec![
            McpServer {
                name: "files".into(),
                transport: McpTransport::Stdio {
                    command: "npx".into(),
                    args: vec!["-y".into(), "srv".into()],
                    env: vec![
                        var("LOG", "debug", None),
                        var("TOKEN", "", Some("bmcp_1#env:TOKEN")),
                    ],
                },
            },
            McpServer {
                name: "remote".into(),
                transport: McpTransport::Http {
                    url: "https://mcp.example.com/mcp".into(),
                    headers: vec![var(
                        "Authorization",
                        "",
                        Some("bmcp_2#header:Authorization"),
                    )],
                },
            },
            McpServer {
                name: "events".into(),
                transport: McpTransport::Sse {
                    url: "http://127.0.0.1:9/sse".into(),
                    headers: vec![],
                },
            },
            // a secret the Hub refuses, and one that would be two header lines: both left out
            McpServer {
                name: "gone".into(),
                transport: McpTransport::Http {
                    url: "https://x".into(),
                    headers: vec![var("A", "", Some("nope"))],
                },
            },
            McpServer {
                name: "split".into(),
                transport: McpTransport::Http {
                    url: "https://x".into(),
                    headers: vec![var("X", "", Some("bmcp_3#header:X"))],
                },
            },
        ];
        let got = servers(Some(hub.clone() as Arc<dyn HubSecrets>), &session, &list).await;
        assert_eq!(
            Value::Array(got),
            json!([
                {"name": "files", "command": "npx", "args": ["-y", "srv"],
                 "env": [{"name": "LOG", "value": "debug"}, {"name": "TOKEN", "value": "tok-123"}]},
                {"type": "http", "name": "remote", "url": "https://mcp.example.com/mcp",
                 "headers": [{"name": "Authorization", "value": "Bearer abc"}]},
                {"type": "sse", "name": "events", "url": "http://127.0.0.1:9/sse", "headers": []},
            ])
        );
        let asked = hub.0.lock().unwrap().clone();
        assert!(asked.iter().all(|(op, _)| op == "mcp:s_1"), "{asked:?}");
        assert_eq!(asked.len(), 4);
        // without the Hub's secrets channel, only servers without secrets go
        let got = servers(None, &session, &list).await;
        let names: Vec<_> = got.iter().map(|v| v["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["events"]);
    }

    #[test]
    fn what_a_node_is_given_is_checked() {
        let ok = |name: &str| McpServer {
            name: name.into(),
            transport: McpTransport::Stdio {
                command: "x".into(),
                args: vec![],
                env: vec![],
            },
        };
        assert!(McpServer::check_all(&[ok("a"), ok("b-1_c")]).is_ok());
        for bad in [
            vec![ok("steve")],
            vec![ok("Steve")],
            vec![ok("a"), ok("A")],
            vec![ok("a b")],
            vec![ok("")],
        ] {
            assert!(McpServer::check_all(&bad).is_err(), "{bad:?}");
        }
        let http = |url: &str, v: McpVar| McpServer {
            name: "h".into(),
            transport: McpTransport::Http {
                url: url.into(),
                headers: vec![v],
            },
        };
        assert!(McpServer::check_all(&[http("https://a", var("A", "b", None))]).is_ok());
        assert!(McpServer::check_all(&[http("file:///a", var("A", "b", None))]).is_err());
        assert!(McpServer::check_all(&[http("https://a", var("A", "b\nc", None))]).is_err());
        assert!(McpServer::check_all(&[http("https://a", var("", "b", None))]).is_err());
    }
}
