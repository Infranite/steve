//! Drives the fake Microsoft Graph mail API with `reqwest`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use steve_mail::fake::{FakeGraph, INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN, fixtures};

const ME: &str = "me@example.com";

struct Api {
    graph: FakeGraph,
    http: Client,
    token: String,
}

impl Api {
    async fn new() -> Self {
        Self {
            graph: FakeGraph::start(ME).await.unwrap(),
            http: Client::new(),
            token: INITIAL_ACCESS_TOKEN.into(),
        }
    }
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.graph.api_base())
    }
    async fn get_url(&self, url: &str, prefer: Option<&str>) -> (StatusCode, Value) {
        let mut req = self.http.get(url).bearer_auth(&self.token);
        if let Some(p) = prefer {
            req = req.header("Prefer", p);
        }
        let r = req.send().await.unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }
    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.get_url(&self.url(path), None).await
    }
}

/// Follows `@odata.nextLink` to the end; returns the items and the deltaLink.
async fn drain(
    api: &Api,
    first: &str,
    prefer: Option<&str>,
) -> (Vec<Value>, Option<String>, usize) {
    let mut url = first.to_string();
    let mut items = Vec::new();
    let mut pages = 0;
    loop {
        let (s, v) = api.get_url(&url, prefer).await;
        assert_eq!(s, 200, "{v}");
        pages += 1;
        items.extend(v["value"].as_array().unwrap().iter().cloned());
        if let Some(n) = v["@odata.nextLink"].as_str() {
            url = n.to_string();
        } else {
            return (
                items,
                v["@odata.deltaLink"].as_str().map(String::from),
                pages,
            );
        }
    }
}

#[tokio::test]
async fn me_and_folders() {
    let api = Api::new().await;
    let r = api.http.get(api.url("me")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let (_, me) = api.get("me").await;
    assert_eq!(me["mail"], ME);

    let sub = api.graph.add_folder("Receipts", Some("inbox")).unwrap();
    let (_, f) = api.get("me/mailFolders?$top=100").await;
    let names: Vec<&str> = f["value"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["displayName"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "Inbox",
            "Sent Items",
            "Drafts",
            "Archive",
            "Deleted Items",
            "Junk Email"
        ]
    );
    let (_, f) = api.get("me/mailFolders?$top=4").await;
    assert_eq!(f["value"].as_array().unwrap().len(), 4);
    assert!(f["@odata.nextLink"].as_str().unwrap().contains("$skip=4"));

    let (_, inbox) = api.get("me/mailFolders/inbox").await;
    assert_eq!(inbox["id"], api.graph.folder_id("inbox").unwrap());
    assert_eq!(inbox["childFolderCount"], 1);
    let (_, kids) = api.get("me/mailFolders/inbox/childFolders").await;
    assert_eq!(kids["value"][0]["id"], sub);
}

#[tokio::test]
async fn delta_pages_changes_and_removals() {
    let api = Api::new().await;
    let mut ids = Vec::new();
    for i in 0..25 {
        let raw = fixtures::ALL[i % fixtures::ALL.len()].1;
        ids.push(api.graph.inject("inbox", raw).unwrap());
    }
    let select = "$select=id,conversationId,internetMessageId,isRead,parentFolderId,receivedDateTime,hasAttachments";
    let first = api.url(&format!("me/mailFolders/inbox/messages/delta?{select}"));
    let (items, delta, pages) = drain(&api, &first, Some("odata.maxpagesize=10")).await;
    assert_eq!(pages, 3);
    assert_eq!(items.len(), 25);
    let keys: Vec<&String> = items[0].as_object().unwrap().keys().collect();
    assert!(
        keys.iter()
            .all(|k| select.contains(k.as_str()) || *k == "@odata.etag"),
        "{keys:?}"
    );
    assert!(items.iter().all(|m| m["isRead"] == false));
    let delta = delta.unwrap();
    assert!(
        delta.contains("$deltatoken=") && delta.contains("$select="),
        "{delta}"
    );

    // Nothing changed → empty round with a new deltaLink.
    let (items, again, _) = drain(&api, &delta, None).await;
    assert!(items.is_empty());

    api.graph.set_read(&ids[0], true).unwrap();
    let moved = api.graph.move_message(&ids[1], "archive").unwrap();
    api.graph.delete(&ids[2]).unwrap();
    let fresh = api
        .graph
        .inject("inbox", fixtures::get("newsletter.eml").unwrap())
        .unwrap();

    let (items, _, _) = drain(&api, &again.unwrap(), None).await;
    assert_eq!(items.len(), 4, "{items:#?}");
    assert_eq!(items[0]["id"], ids[0]);
    assert_eq!(items[0]["isRead"], true);
    assert_eq!(items[1]["id"], ids[1]);
    assert_eq!(items[1]["@removed"]["reason"], "deleted");
    assert_eq!(items[2]["id"], ids[2]);
    assert!(items[2]["@removed"].is_object());
    assert_eq!(items[3]["id"], fresh);

    // The moved copy has a new id and shows up in the archive's delta.
    assert_ne!(moved, ids[1]);
    let (arch, _, _) = drain(
        &api,
        &api.url("me/mailFolders/archive/messages/delta"),
        None,
    )
    .await;
    assert_eq!(arch.len(), 1);
    assert_eq!(arch[0]["id"], moved);

    api.graph.expire_delta();
    let (s, e) = api.get_url(&delta, None).await;
    assert_eq!(s, 410);
    assert_eq!(e["error"]["code"], "SyncStateNotFound");
}

#[tokio::test]
async fn initial_delta_filter_and_mime_value() {
    let api = Api::new().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let [t1, t2, t3] = fixtures::thread();
    api.graph.inject_at("inbox", t1, now - 90 * 86400).unwrap();
    let recent = api.graph.inject_at("inbox", t3, now - 86400).unwrap();
    let sent = api
        .graph
        .inject_at("sentitems", t2, now - 2 * 86400)
        .unwrap();

    let since = mail_parser::DateTime::from_timestamp(now - 30 * 86400).to_rfc3339();
    let url = api.url(&format!(
        "me/mailFolders/inbox/messages/delta?$filter=receivedDateTime ge {since}"
    ));
    let (items, _, _) = drain(&api, &url, None).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], recent);
    assert_eq!(items[0]["subject"], "Re: 预算审批流程");
    assert_eq!(
        items[0]["from"]["emailAddress"]["address"],
        "alice@acme.example"
    );

    // All three share a conversation.
    let convs: std::collections::BTreeSet<String> = api
        .graph
        .messages()
        .into_iter()
        .map(|m| m.conversation_id)
        .collect();
    assert_eq!(convs.len(), 1);

    let r = api
        .http
        .get(api.url(&format!("me/messages/{sent}/$value")))
        .bearer_auth(&api.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.bytes().await.unwrap().as_ref(), t2);

    let (_, m) = api
        .get(&format!("me/messages/{recent}?$select=internetMessageId"))
        .await;
    assert_eq!(m["internetMessageId"], "<thread-3.alice@acme.example>");
    assert!(m.get("subject").is_none());

    let (s, _) = api.get("me/messages/nope").await;
    assert_eq!(s, 404);
}

#[tokio::test]
async fn patch_move_and_send_mail() {
    let api = Api::new().await;
    let id = api
        .graph
        .inject("inbox", fixtures::get("gbk.eml").unwrap())
        .unwrap();
    let r = api
        .http
        .patch(api.url(&format!("me/messages/{id}")))
        .bearer_auth(&api.token)
        .json(&json!({"isRead": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(api.graph.messages()[0].is_read);

    let r = api
        .http
        .post(api.url(&format!("me/messages/{id}/move")))
        .bearer_auth(&api.token)
        .json(&json!({"destinationId": "archive"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let moved: Value = r.json().await.unwrap();
    assert_ne!(moved["id"], id);
    assert_eq!(
        moved["parentFolderId"],
        api.graph.folder_id("archive").unwrap()
    );

    // MIME send: base64 body, text/plain.
    let mime = fixtures::get("thread-2.eml").unwrap();
    let r = api
        .http
        .post(api.url("me/sendMail"))
        .bearer_auth(&api.token)
        .header("Content-Type", "text/plain")
        .body(STANDARD.encode(mime))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let sent = api.graph.sent();
    assert_eq!(sent[0].mime.as_deref(), Some(mime));
    let sent_items = api.graph.folder_id("sentitems").unwrap();
    assert!(
        api.graph
            .messages()
            .iter()
            .any(|m| m.folder_id == sent_items && m.raw == mime)
    );

    // JSON send.
    let r = api
        .http
        .post(api.url("me/sendMail"))
        .bearer_auth(&api.token)
        .json(&json!({"message": {"subject": "Hi", "body": {"contentType": "Text", "content": "x"},
            "toRecipients": [{"emailAddress": {"address": "alice@acme.example"}}]}, "saveToSentItems": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    assert_eq!(
        api.graph.sent()[1].json.as_ref().unwrap()["message"]["subject"],
        "Hi"
    );

    let r = api
        .http
        .post(api.url("me/sendMail"))
        .bearer_auth(&api.token)
        .header("Content-Type", "text/plain")
        .body("not base64!!")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}

#[tokio::test]
async fn token_rotation() {
    let mut api = Api::new().await;
    api.graph.expire_tokens();
    assert_eq!(api.get("me").await.0, 401);
    let r: Value = api
        .http
        .post(api.graph.token_url())
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", INITIAL_REFRESH_TOKEN),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let new_refresh = r["refresh_token"].as_str().unwrap().to_string();
    api.token = r["access_token"].as_str().unwrap().into();
    assert_eq!(api.get("me").await.0, 200);
    // Microsoft rotates: the old refresh token is spent.
    let old = api
        .http
        .post(api.graph.token_url())
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", INITIAL_REFRESH_TOKEN),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(old.status(), 400);
    let ok = api
        .http
        .post(api.graph.token_url())
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", new_refresh.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
}
