//! The reading side of the mail REST: the thread list (five views, account, search,
//! paging), one thread with messages, drafts and items, `PATCH` of the state, a message's sanitized
//! HTML under a strict CSP, inline `cid:` images, the original `.eml`, the IMAP presets, and the first
//! sync of a new mailbox starting on its connection event. Every route checks the thread is the
//! caller's. Fake IMAP/SMTP and temp dirs only.

mod support;

use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, header};
use serde_json::{Value, json};
use steve_mail::fake::fixtures;
use steve_mail::nodes;
use steve_mail::threads::csp;
use steve_modules::Caller;
use steve_modules::testkit::{ME, OTHER, WS, WS2, call, caller};
use support::*;
use tower::ServiceExt;

struct R {
    h: H,
    r: axum::Router,
    conn: String,
    chain: String,
    order: String,
    logo: String,
}

async fn r() -> R {
    let h = setup().await;
    let [t1, t2, t3] = fixtures::thread();
    h.inject("INBOX", t1);
    h.inject("Sent", t2);
    h.inject("INBOX", t3);
    h.inject("INBOX", fixtures::get("html-tracking.eml").unwrap());
    h.inject("INBOX", fixtures::get("inline-cid.eml").unwrap());
    let conn = h.connect().await;
    h.sync(&conn).await;
    let chain = h.by_subject(&conn, "预算审批流程").thread_id;
    let order = h
        .by_subject(&conn, "Votre commande est confirmée à la caisse")
        .thread_id;
    let logo = h.by_subject(&conn, "New logo draft").thread_id;
    let r = steve_mail::api::routes(h.mail.clone()).with_state(h.env.modules.clone());
    R {
        h,
        r,
        conn,
        chain,
        order,
        logo,
    }
}

async fn get_raw(r: &axum::Router, who: &Caller, path: &str) -> (u16, HeaderMap, Vec<u8>) {
    let mut req = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(who.clone());
    let res = r.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let headers = res.headers().clone();
    let b = axum::body::to_bytes(res.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap();
    (status, headers, b.to_vec())
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_thread_list_views_search_and_paging() {
    let e = r().await;
    let list = |q: &str| {
        let r = e.r.clone();
        let q = q.to_string();
        async move { call(&r, Some(&me()), "GET", &format!("/mail/threads{q}"), None).await }
    };
    let (s, v) = list("").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        ids(&v),
        [e.logo.clone(), e.order.clone(), e.chain.clone()],
        "inbox, newest first"
    );
    let chain = v.as_array().unwrap()[2].clone();
    assert_eq!(chain["connection"], json!({"id": e.conn, "address": ADDR}));
    assert_eq!(chain["count"], 3);
    assert_eq!(chain["from"]["name"], "Alice Zhou");
    assert_eq!(chain["from"]["addr"], "alice@acme.example");
    assert!(chain["from"]["party_id"].as_str().is_some());
    assert_eq!(chain["snippet"], "收到，谢谢！我抄送了 Bob，他负责填表。");
    assert_eq!(chain["has_attachments"], false);
    assert_eq!(chain["draft"], Value::Null);
    assert_eq!(chain["item_id"], Value::Null);

    let (_, v) = list("?view=sent").await;
    assert_eq!(ids(&v), std::slice::from_ref(&e.chain));
    let (_, v) = list("?view=needs_reply").await;
    assert!(ids(&v).is_empty());
    e.h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE mail_threads SET state = 'needs_reply' WHERE id = ?",
                [&e.chain],
            )?;
            c.execute(
                "UPDATE mail_threads SET state = 'waiting' WHERE id = ?",
                [&e.order],
            )?;
            Ok(())
        })
        .unwrap();
    let (_, v) = list("?view=needs_reply").await;
    assert_eq!(ids(&v), std::slice::from_ref(&e.chain));
    let (_, v) = list("?view=waiting").await;
    assert_eq!(ids(&v), std::slice::from_ref(&e.order));

    // drafts
    let d = e
        .h
        .mail
        .create_draft(
            &me(),
            serde_json::from_value(json!({"connection_id": e.conn, "mode": "reply", "thread_id": e.order,
                "to": [{"addr": "orders@shop.example"}], "subject": "Re: commande", "body": "Merci"}))
            .unwrap(),
        )
        .unwrap();
    let (_, v) = list("?view=drafts").await;
    assert_eq!(ids(&v), std::slice::from_ref(&e.order));
    assert_eq!(v[0]["draft"], json!({"id": d.id, "status": "draft"}));
    let (_, v) = list("?view=all").await;
    assert_eq!(v.as_array().unwrap().len(), 3);

    // search: sender name, subject, snippet; LIKE characters are literal
    let (_, v) = list("?q=alice").await;
    assert_eq!(ids(&v), std::slice::from_ref(&e.chain));
    let (_, v) = list("?q=logo").await;
    assert_eq!(ids(&v), std::slice::from_ref(&e.logo));
    let (_, v) = list("?q=%25").await;
    assert!(ids(&v).is_empty());

    // paging
    let (_, v) = list("?limit=1").await;
    assert_eq!(ids(&v), std::slice::from_ref(&e.logo));
    let before = v[0]["last_at"].as_i64().unwrap();
    let (_, v) = list(&format!("?limit=1&before={before}")).await;
    assert_eq!(ids(&v), std::slice::from_ref(&e.order));

    // account filter
    let (_, v) = list(&format!("?account={}", e.conn)).await;
    assert_eq!(v.as_array().unwrap().len(), 3);
    let (s, _) = list("?account=con_nope").await;
    assert_eq!(s, 404);
    for bad in ["?limit=500", "?limit=0", "?view=bogus", "?before=x"] {
        let (s, v) = list(bad).await;
        assert_eq!(s, 400, "{bad}: {v}");
        assert!(v["error"].as_str().unwrap().starts_with("参数不对："));
    }

    // somebody else, another workspace: nothing
    for who in [caller(WS, OTHER), caller(WS2, ME)] {
        let (s, v) = call(&e.r, Some(&who), "GET", "/mail/threads?view=all", None).await;
        assert_eq!(s, 200);
        assert_eq!(v, json!([]));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn one_thread_with_messages_drafts_and_items_and_its_state() {
    let e = r().await;
    let msg = e.h.by_subject(&e.conn, "预算审批流程").id;
    e.h.env
        .store()
        .transaction(|tx| -> Result<(), steve_mail::MailError> {
            tx.execute(
                "INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated) VALUES ('ST-9', ?, 9, 'task', '回复预算审批', 'open', '', 1, 1)",
                [WS],
            )?;
            nodes::link_item(tx, WS, "ST-9", &msg, &e.chain)
        })
        .unwrap();
    let path = format!("/mail/threads/{}", e.chain);
    let (s, v) = call(&e.r, Some(&me()), "GET", &path, None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["id"], e.chain.as_str());
    assert_eq!(v["item_id"], "ST-9");
    assert_eq!(
        v["items"],
        json!([{"id": "ST-9", "title": "回复预算审批", "status": "open"}])
    );
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["direction"], "in");
    assert_eq!(msgs[1]["direction"], "out");
    assert_eq!(
        msgs[1]["text_new"],
        "三步：部门负责人、财务、CFO。表格我稍后发你。"
    );
    assert!(msgs[1]["text"].as_str().unwrap().contains("> 你好"));
    assert_eq!(msgs[2]["cc"][0]["addr"], "bob@acme.example");
    assert_eq!(msgs[0]["has_html"], false);
    assert_eq!(msgs[0]["purged"], false);
    assert_eq!(v["drafts"], json!([]));

    let (s, v) = call(
        &e.r,
        Some(&me()),
        "GET",
        &format!("/mail/threads/{}", e.logo),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let a = &v["messages"][0]["attachments"][0];
    assert_eq!(a["name"], "logo.png");
    assert_eq!(a["inline"], true);

    for who in [caller(WS, OTHER), caller(WS2, ME)] {
        let (s, v) = call(&e.r, Some(&who), "GET", &path, None).await;
        assert_eq!(s, 404);
        assert_eq!(v["error"], "没有这个往来");
        let (s, _) = call(
            &e.r,
            Some(&who),
            "PATCH",
            &path,
            Some(&json!({"state": "muted"})),
        )
        .await;
        assert_eq!(s, 404);
    }

    // mute: out of the inbox; done; bad values
    let (s, v) = call(
        &e.r,
        Some(&me()),
        "PATCH",
        &path,
        Some(&json!({"state": "muted"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["state"], "muted");
    let (_, l) = call(&e.r, Some(&me()), "GET", "/mail/threads", None).await;
    assert!(!ids(&l).contains(&e.chain));
    let (_, v) = call(
        &e.r,
        Some(&me()),
        "PATCH",
        &path,
        Some(&json!({"state": "done"})),
    )
    .await;
    assert_eq!(v["state"], "done");
    let (s, _) = call(
        &e.r,
        Some(&me()),
        "PATCH",
        &path,
        Some(&json!({"state": "waiting"})),
    )
    .await;
    assert_eq!(s, 400);
    let (s, _) = call(
        &e.r,
        Some(&me()),
        "PATCH",
        &path,
        Some(&json!({"colour": 1})),
    )
    .await;
    assert_eq!(s, 400);
    let n: i64 =
        e.h.env
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT COUNT(*) FROM audit WHERE action = 'mail_thread.patch' AND target = ?",
                    [&e.chain],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
    assert_eq!(n, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn html_is_sanitized_and_served_under_a_strict_csp() {
    let e = r().await;
    let order =
        e.h.by_subject(&e.conn, "Votre commande est confirmée à la caisse")
            .id;
    let (s, hd, b) = get_raw(&e.r, &me(), &format!("/mail/messages/{order}/html")).await;
    assert_eq!(s, 200);
    assert_eq!(hd[header::CONTENT_SECURITY_POLICY], csp(false).as_str());
    assert_eq!(
        hd[header::CONTENT_SECURITY_POLICY],
        "default-src 'none'; style-src 'unsafe-inline'; img-src data: cid:; sandbox"
    );
    assert_eq!(hd[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(hd[header::CACHE_CONTROL], "no-store");
    assert_eq!(hd[header::CONTENT_TYPE], "text/html; charset=utf-8");
    let html = String::from_utf8(b).unwrap();
    assert!(
        html.contains("Bonjour, votre commande au Café de la Gare"),
        "{html}"
    );
    for bad in [
        "<script",
        "onload",
        "onclick",
        "<iframe",
        "stylesheet",
        "evil.example",
    ] {
        assert!(!html.contains(bad), "{bad}: {html}");
    }
    let (_, hd, _) = get_raw(
        &e.r,
        &me(),
        &format!("/mail/messages/{order}/html?images=1"),
    )
    .await;
    assert_eq!(
        hd[header::CONTENT_SECURITY_POLICY],
        "default-src 'none'; style-src 'unsafe-inline'; img-src data: cid: https:; sandbox"
    );

    // inline images are in the page as data: URLs and on their own route
    let logo = e.h.by_subject(&e.conn, "New logo draft").id;
    let (_, _, b) = get_raw(&e.r, &me(), &format!("/mail/messages/{logo}/html")).await;
    assert!(
        String::from_utf8(b)
            .unwrap()
            .contains("src=\"data:image/png;base64,iVBORw0KGgo")
    );
    let (s, hd, b) = get_raw(
        &e.r,
        &me(),
        &format!("/mail/messages/{logo}/cid/logo@acme.example"),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(hd[header::CONTENT_TYPE], "image/png");
    assert_eq!(hd[header::CACHE_CONTROL], "no-store");
    assert_eq!(hd[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert!(b.starts_with(b"\x89PNG"));
    let (s, _, _) = get_raw(&e.r, &me(), &format!("/mail/messages/{logo}/cid/nope@x")).await;
    assert_eq!(s, 404);

    // a text-only message has no HTML
    let chain = e.h.by_subject(&e.conn, "预算审批流程").id;
    let (s, _, _) = get_raw(&e.r, &me(), &format!("/mail/messages/{chain}/html")).await;
    assert_eq!(s, 404);

    // the original
    let (s, hd, b) = get_raw(&e.r, &me(), &format!("/mail/messages/{chain}/raw")).await;
    assert_eq!(s, 200);
    assert_eq!(hd[header::CONTENT_TYPE], "message/rfc822");
    assert_eq!(hd[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        hd[header::CONTENT_DISPOSITION],
        format!("attachment; filename=\"{chain}.eml\"").as_str()
    );
    assert!(String::from_utf8_lossy(&b).contains("Message-ID: <thread-1.alice@acme.example>"));

    // only the owner
    for who in [caller(WS, OTHER), caller(WS2, ME)] {
        for p in [
            format!("/mail/messages/{chain}/raw"),
            format!("/mail/messages/{order}/html"),
            format!("/mail/messages/{logo}/cid/logo@acme.example"),
        ] {
            let (s, _, b) = get_raw(&e.r, &who, &p).await;
            assert_eq!(s, 404, "{p}");
            assert!(String::from_utf8_lossy(&b).contains("没有这个邮件"));
        }
    }

    // past retention: 410
    let rel =
        e.h.by_subject(&e.conn, "Votre commande est confirmée à la caisse")
            .raw_path
            .unwrap();
    std::fs::remove_file(e.h.env.dir.path().join(rel)).unwrap();
    let (s, _, b) = get_raw(&e.r, &me(), &format!("/mail/messages/{order}/html")).await;
    assert_eq!(s, 410);
    assert!(String::from_utf8_lossy(&b).contains("保留期"));
    let (s, _, _) = get_raw(&e.r, &me(), &format!("/mail/messages/{order}/raw")).await;
    assert_eq!(s, 410);
}

#[tokio::test(flavor = "multi_thread")]
async fn presets_for_the_dialog() {
    let e = r().await;
    let (s, v) = call(&e.r, Some(&me()), "GET", "/mail/presets", None).await;
    assert_eq!(s, 200);
    let gmail = v
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "gmail")
        .unwrap();
    assert_eq!(gmail["imap_host"], "imap.gmail.com");
    assert_eq!(gmail["imap_port"], 993);
    assert!(v.as_array().unwrap().iter().any(|p| p["id"] == "163"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_mailbox_syncs_on_its_connection_event() {
    let h = setup().await;
    let [t1, _, _] = fixtures::thread();
    h.inject("INBOX", t1);
    let conn = h.connect().await;
    assert!(h.messages(&conn).is_empty(), "nothing synced yet");
    h.mail.on_event(&steve_store::ChangeEvent::Connection {
        id: conn.clone(),
        ws: WS.into(),
    });
    let mut synced = false;
    for _ in 0..200 {
        if !h.messages(&conn).is_empty() {
            synced = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(synced, "the runner started and synced at once");
    h.mail.stop(&conn);
}
