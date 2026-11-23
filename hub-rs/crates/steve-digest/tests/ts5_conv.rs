//! Fixed-timestamp source projection contracts missing from SG12's characterization.
#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use serde_json::json;
use steve_digest::{ConvRef, conv};
use steve_modules::testkit::WS;
use steve_store::StoreError;
use steve_store::testkit::digest as fixture;
const AT: i64 = 1_800_000_000_000;

fn environment() -> TestEnv {
    let e = TestEnv::new(|_| {});
    fixture::ts5_space(e.store()).unwrap();
    e
}

#[test]
fn inbound_names_and_persona_or_none_drafts_use_the_owners_write_paths() {
    let e = environment();
    fixture::ts5_channels(e.store()).unwrap();
    let inbound = e
        .store()
        .write(|tx| {
            steve_store::channel_message_repo::ingress::insert(
                tx,
                &steve_store::channel_message_repo::ingress::NewInbound {
                    ws: WS,
                    space_id: "ts5-space",
                    external_id: "ts5-in",
                    thread_key: None,
                    reply_to: None,
                    sender_account_id: Some("ts5-bot-account"),
                    sender_party_id: None,
                    text: "inbound probe",
                    attachments: &json!([]),
                    mentions: &json!([]),
                    created: AT,
                    content_key: None,
                    fallback_from: None,
                    history: None,
                    seen_by: 0,
                },
            )
            .map(|id| id.unwrap())
            .map_err(|e| StoreError::Hook(e.to_string()))
        })
        .unwrap();
    let cv = ConvRef::new("space", "ts5-space");
    let read = || {
        e.store()
            .read(|c| conv::read_after(c, WS, &cv, 0, "", 20))
            .unwrap()
    };
    let incoming = read();
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].id, inbound);
    assert_eq!(incoming[0].sender.name, "Persona");
    assert!(incoming[0].sender.bot);
    for with_persona in [false, true] {
        let (draft, persona) = e
            .store()
            .write(|w| {
                let persona = if with_persona {
                    let n =
                        serde_json::from_value(json!({"name":"Public persona","bot_id":"ts5-bot"}))
                            .unwrap();
                    Some(
                        steve_channels::personas::create(
                            w,
                            &steve_store::base::Viewer::bare(WS, "usr_dev"),
                            &n,
                            AT,
                        )
                        .map_err(|e| StoreError::Hook(e.to_string()))?
                        .id,
                    )
                } else {
                    None
                };
                let target = steve_channels::deliver::OutTarget {
                    ws: WS.into(),
                    connection_id: "ts5-channel-conn".into(),
                    mode: steve_channels::spaces::Mode::Bot,
                    space_id: "ts5-space".into(),
                    space_name: "TS5".into(),
                    reply_to: None,
                    thread_key: None,
                    persona_id: persona.clone(),
                    source_message_id: None,
                };
                let (draft, _) = steve_channels::respond::open_draft_to(w, &target, "not sent", AT)
                    .map_err(|e| StoreError::Hook(e.to_string()))?;
                Ok::<_, StoreError>((draft, persona))
            })
            .unwrap();
        let stored = steve_channels::messages::get(e.store(), &draft)
            .unwrap()
            .unwrap();
        assert_eq!(stored.direction, "out");
        assert_eq!(stored.status, "draft");
        assert_eq!(stored.created, AT);
        assert_eq!(stored.persona_id, persona);
        assert_eq!(
            stored.on_behalf,
            if with_persona { "persona" } else { "none" }
        );
        assert_eq!(
            read().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            [inbound.as_str()],
            "drafts never enter digest projection"
        );
    }
    // Proxy/sent outbound projection is deferred by the TS5 scope ruling to phase 4's fake
    // outbound fixture. Do not manufacture it by updating the source table here.
}

#[test]
fn mail_sender_inline_filter_and_equal_time_cursor_order() {
    let e = environment();
    fixture::ts5_mail(e.store(), AT).unwrap();
    let conv = ConvRef::new("mail_thread", "ts5-mail");
    let mut ids = Vec::new();
    let mut cursor = (0, String::new());
    for _ in 0..3 {
        let page = e
            .store()
            .read(|c| conv::read_after(c, WS, &conv, cursor.0, &cursor.1, 1))
            .unwrap();
        assert_eq!(page.len(), 1);
        cursor = (page[0].at, page[0].id.clone());
        ids.push(page[0].id.clone());
    }
    assert_eq!(ids, ["mail-a", "mail-b", "mail-c"]);
    assert!(
        e.store()
            .read(|c| conv::read_after(c, WS, &conv, cursor.0, &cursor.1, 1))
            .unwrap()
            .is_empty()
    );
    let before = e
        .store()
        .read(|c| conv::read_before(c, WS, &conv, AT, "mail-c", 2))
        .unwrap();
    assert_eq!(
        before.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["mail-b", "mail-c"]
    );
    let messages = e
        .store()
        .read(|c| conv::read_after(c, WS, &conv, 0, "", 10))
        .unwrap();
    assert_eq!(messages[1].sender.name, "Bulk Sender");
    assert!(messages[1].sender.bot);
    assert!(!messages[1].sender.mine);
    assert_eq!(messages[2].sender.name, "sender@example.invalid");
    assert!(!messages[2].sender.bot);
    let attachments = &messages[1].attachments;
    assert_eq!(attachments.len(), 2);
    assert!(attachments.iter().all(|a| a["inline"] == false));
    assert_ne!(attachments[0]["key"], attachments[1]["key"]);
    let shaped: Vec<_> = attachments
        .iter()
        .map(|a| json!({"type":a["type"],"name":a["name"],"mime":a["mime"],"size":a["size"]}))
        .collect();
    assert_eq!(
        shaped,
        vec![
            json!({"type":"file","name":"file.bin","mime":"application/pdf","size":7}),
            json!({"type":"image","name":"image.bin","mime":"image/png","size":7})
        ]
    );
}
