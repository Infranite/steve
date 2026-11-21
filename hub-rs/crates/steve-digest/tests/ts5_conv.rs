//! Fixed-timestamp source projection contracts missing from SG12's characterization.
#[path = "../../steve-channels/tests/support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use serde_json::json;
use steve_digest::{ConvRef, conv};
use steve_modules::testkit::WS;
use steve_store::StoreError;
const AT: i64 = 1_800_000_000_000;

fn environment() -> TestEnv {
    let e = TestEnv::new(|_| {});
    e.store().with_conn(|c|{c.execute_batch("INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,created,updated) VALUES('ts5-space','default','test','group','ts5','TS5',1,1);
        INSERT INTO parties(id,workspace_id,type,name,user_id,created_by,created,updated) VALUES('ts5-owner','default','person','Owner','usr_dev','usr_dev',1,1),('ts5-bot','default','person','',NULL,'usr_dev',1,1);
        INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,display_name,source,created,updated) VALUES('ts5-bot-account','default','test','other','ts5-bot','app_bot','bot','fallback','Persona','fixture',1,1); ")?;Ok(())}).unwrap();
    e
}

#[test]
fn inbound_names_and_persona_or_none_drafts_use_the_owners_write_paths() {
    let e = environment();
    e.store().with_conn(|c| {
        c.execute_batch("INSERT INTO connections(id,workspace_id,account_id,adapter,auth,created_by,created,updated) VALUES('ts5-channel-conn','default','ts5-bot-account','fixture','none','usr_dev',1,1);
        INSERT INTO bots(id,workspace_id,name,harness,enabled,updated) VALUES('ts5-bot','default','TS5','claude-code',1,1);
        INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES('ts5-bot','default',1);")?;Ok(())
    }).unwrap();
    let inbound = e
        .store()
        .transaction(|tx| {
            steve_channels::messages::insert_inbound(
                tx,
                &steve_channels::messages::NewInbound {
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
            .reader(|c| conv::read_after(c, WS, &cv, 0, "", 20))
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
            .transaction(|tx| {
                let persona = if with_persona {
                    let n =
                        serde_json::from_value(json!({"name":"Public persona","bot_id":"ts5-bot"}))
                            .unwrap();
                    Some(
                        steve_channels::personas::create(
                            tx,
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
                let (draft, _) =
                    steve_channels::respond::open_draft_to(tx, &target, "not sent", AT)
                        .map_err(|e| StoreError::Hook(e.to_string()))?;
                Ok::<_, StoreError>((draft, persona))
            })
            .unwrap();
        let stored = e
            .store()
            .reader(|c| {
                steve_channels::messages::get(c, &draft)
                    .map_err(|e| StoreError::Hook(e.to_string()))
            })
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
    e.store().transaction(|tx|{
        tx.execute_batch("INSERT INTO connections(id,workspace_id,account_id,adapter,auth,created_by,created,updated) VALUES('ts5-mail-conn','default','ts5-bot-account','fixture','none','usr_dev',1,1);
        INSERT INTO mail_threads(id,workspace_id,connection_id,thread_key,subject,last_at,created_by,created,updated) VALUES('ts5-mail','default','ts5-mail-conn','thread','TS5',1,'usr_dev',1,1);")?;
        // Reverse insertion order defeats accidental rowid tie-breaking.
        for (id,name,bulk) in [("mail-c","  ",false),("mail-b","Bulk Sender",true),("mail-a","Alice",false)] {
            steve_mail::repo::insert_message(tx,&steve_mail::repo::MessageRow {
                id:id.into(),workspace_id:WS.into(),connection_id:"ts5-mail-conn".into(),thread_id:"ts5-mail".into(),ext_id:id.into(),message_id:Some(format!("<{id}>")),
                in_reply_to:None,refs:vec![],folders:vec![],flags:vec![],direction:"in".into(),from_addr:"sender@example.invalid".into(),from_name:name.into(),
                from_account_id:None,from_party_id:None,reply_to:None,to_addrs:json!([]),cc_addrs:json!([]),subject:"TS5".into(),snippet:"body".into(),date:AT,size:4,
                has_attachments:id=="mail-b",bulk,body_text:Some("body".into()),body_new:None,has_html:false,raw_path:None,purged:None,event_id:None,status:"received".into(),created:AT,
            }).map_err(|e|StoreError::Hook(e.to_string()))?;
        }
        for (name,part,mime,inline) in [("file","2","application/pdf",false),("inline","1","image/png",true),("image","3","image/png",false)] {
            steve_mail::repo::insert_attachment(tx,WS,"mail-b",&steve_mail::adapter::PartMeta {part:part.into(),name:format!("{name}.bin"),mime:mime.into(),size:7,content_id:None,inline},AT).map_err(|e|StoreError::Hook(e.to_string()))?;
        }
        Ok::<_,StoreError>(())
    }).unwrap();
    let conv = ConvRef::new("mail_thread", "ts5-mail");
    let mut ids = Vec::new();
    let mut cursor = (0, String::new());
    for _ in 0..3 {
        let page = e
            .store()
            .reader(|c| conv::read_after(c, WS, &conv, cursor.0, &cursor.1, 1))
            .unwrap();
        assert_eq!(page.len(), 1);
        cursor = (page[0].at, page[0].id.clone());
        ids.push(page[0].id.clone());
    }
    assert_eq!(ids, ["mail-a", "mail-b", "mail-c"]);
    assert!(
        e.store()
            .reader(|c| conv::read_after(c, WS, &conv, cursor.0, &cursor.1, 1))
            .unwrap()
            .is_empty()
    );
    let before = e
        .store()
        .reader(|c| conv::read_before(c, WS, &conv, AT, "mail-c", 2))
        .unwrap();
    assert_eq!(
        before.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["mail-b", "mail-c"]
    );
    let messages = e
        .store()
        .reader(|c| conv::read_after(c, WS, &conv, 0, "", 10))
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
