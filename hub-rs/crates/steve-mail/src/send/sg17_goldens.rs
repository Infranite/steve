//! Byte goldens for the real private MIME builder. Only MIME boundaries are normalized:
//! mail-builder generates them from time/thread counters and exposes no injection in this path.
use super::*;
#[path = "../../../steve-channels/tests/support/digest_env.rs"]
mod digest_env;
use crate::MailConfig;
use crate::repo::{MessageRow, ThreadRow};
use digest_env::TestEnv;
use rusqlite::params;
use steve_modules::accounts::connections::{Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, WS};
use steve_modules::vault::{Vault, keystore::FileKeyStore};
const NOW: i64 = 1_800_000_000_000;

fn normalized(raw: &[u8]) -> Vec<u8> {
    let mut text = String::from_utf8(raw.to_vec()).unwrap();
    let mut boundaries = Vec::new();
    for (_, tail) in text
        .match_indices("boundary=\"")
        .map(|(at, _)| (at, &text[at + 10..]))
    {
        let end = tail.find('"').expect("quoted MIME boundary");
        let boundary = tail[..end].to_string();
        if !boundaries.contains(&boundary) {
            boundaries.push(boundary);
        }
    }
    assert!(
        !boundaries.is_empty(),
        "text + HTML must form multipart/alternative"
    );
    for (index, boundary) in boundaries.iter().enumerate() {
        text = text.replace(boundary, &format!("SG17_BOUNDARY_{}", index + 1));
    }
    text.into_bytes()
}

#[test]
fn rfc5322_reply_forward_attachment_and_unicode_goldens() {
    let env = TestEnv::new(|_| {});
    let keys = tempfile::tempdir().unwrap();
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).unwrap()),
    );
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(vec![], false, |_| None),
    )
    .build();
    let home = env.dir.path().display().to_string();
    let cfg = MailConfig::from_env(move |key| match key {
        "STEVE_HOME" => Some(home.clone()),
        _ => None,
    })
    .unwrap();
    let mail = Mail::builder(env.modules.clone(), conns, cfg.clone()).build();
    env.store().with_conn(|c|{c.execute_batch("INSERT INTO parties(id,workspace_id,type,name,user_id,created_by,created,updated) VALUES('sg17-owner','default','person','王明','usr_dev','usr_dev',1,1);
        INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,source,created,updated) VALUES('sg17-account','default','mail','email','sg17-owner','personal','ming@example.com','ming@example.com','fixture',1,1);
        INSERT INTO connections(id,workspace_id,account_id,adapter,auth,config,created_by,created,updated) VALUES('sg17-conn','default','sg17-account','imap','none','{\"address\":\"ming@example.com\"}','usr_dev',1,1);")?;Ok(())}).unwrap();
    env.store()
        .transaction(|tx| -> MailResult<()> {
            repo::insert_thread(
                tx,
                &ThreadRow {
                    id: "sg17-thread".into(),
                    workspace_id: WS.into(),
                    connection_id: "sg17-conn".into(),
                    thread_key: "<root@example.com>".into(),
                    subject: "旧主题".into(),
                    last_at: NOW,
                    count: 1,
                    unread: 1,
                    participants: vec![],
                    state: "active".into(),
                    visibility: "private".into(),
                    created_by: ME.into(),
                },
                NOW,
            )?;
            repo::insert_message(
                tx,
                &MessageRow {
                    id: "sg17-original".into(),
                    workspace_id: WS.into(),
                    connection_id: "sg17-conn".into(),
                    thread_id: "sg17-thread".into(),
                    ext_id: "INBOX:1:1".into(),
                    message_id: Some("<original@example.com>".into()),
                    in_reply_to: Some("<root@example.com>".into()),
                    refs: vec!["<root@example.com>".into()],
                    folders: vec!["INBOX".into()],
                    flags: vec![],
                    direction: "in".into(),
                    from_addr: "alice@example.com".into(),
                    from_name: "Alice".into(),
                    from_account_id: None,
                    from_party_id: None,
                    reply_to: None,
                    to_addrs: json!([]),
                    cc_addrs: json!([]),
                    subject: "旧主题".into(),
                    snippet: "original".into(),
                    date: NOW - 1000,
                    size: 8,
                    has_attachments: false,
                    bulk: false,
                    body_text: Some("original".into()),
                    body_new: None,
                    has_html: false,
                    raw_path: None,
                    purged: None,
                    event_id: None,
                    status: "received".into(),
                    created: NOW - 1000,
                },
            )?;
            Ok(())
        })
        .unwrap();
    let data = b"SG17 attachment\n";
    let path = files_dir(cfg.home(), WS, "ab", "sg17-file").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, data).unwrap();
    env.store().with_conn(|c|{c.execute("INSERT INTO files(id,workspace_id,sha256,name,mime,size,created_by,created) VALUES('sg17-file',?,'ab','预算.txt','text/plain',?,?,?)",params![WS,data.len() as i64,ME,NOW])?;Ok(())}).unwrap();
    for (name, mode, attach, unicode) in [
        ("reply", "reply", false, false),
        ("forward", "forward", false, false),
        ("attachment", "reply", true, false),
        ("unicode", "reply_all", true, true),
    ] {
        let rcpt = |addr: &str, name: &str| Rcpt {
            addr: addr.into(),
            name: name.into(),
        };
        let d = DraftRow {
            id: format!("md_sg17_{name}"),
            workspace_id: WS.into(),
            connection_id: "sg17-conn".into(),
            thread_id: Some("sg17-thread".into()),
            reply_to_message: Some("sg17-original".into()),
            mode: mode.into(),
            to: vec![rcpt(
                if unicode {
                    "用户@example.com"
                } else {
                    "alice@example.com"
                },
                if unicode { "张三" } else { "Alice" },
            )],
            cc: vec![rcpt("cc@example.com", "Copy")],
            bcc: vec![rcpt("audit@example.com", "")],
            subject: if unicode {
                "Re: 预算 · café"
            } else if mode == "forward" {
                "Fwd: report"
            } else {
                "Re: report"
            }
            .into(),
            body: if mode == "forward" {
                "Forwarded message:\n> original\n.leading dot"
            } else {
                "Reply <ok> & \"quote\"\n> original\n.leading dot"
            }
            .into(),
            files: if attach {
                vec![serde_json::from_value(json!({"id":"sg17-file","name":"预算.txt","mime":"text/plain","size":data.len()})).unwrap()]
            } else {
                vec![]
            },
            author: ME.into(),
            item_id: None,
            session_id: None,
            run_id: None,
            decision_id: None,
            basis: None,
            content_hash: "sg17-fixed-hash".into(),
            status: "approved".into(),
            send_at: Some(NOW),
            sent_message: None,
            error: None,
            created: NOW,
            updated: NOW,
            sent: None,
        };
        let built = env.store().reader(|c| mail.build(c, &d, NOW)).unwrap();
        assert_eq!(
            built.message_id,
            format!("md_sg17_{name}.sg17-fixed-hash@steve.local")
        );
        assert_eq!(
            built.meta.rcpt,
            vec![
                d.to[0].addr.clone(),
                "cc@example.com".into(),
                "audit@example.com".into()
            ]
        );
        let bytes = normalized(&built.mime);
        assert!(
            !String::from_utf8_lossy(&bytes).contains("audit@example.com"),
            "Bcc stays envelope-only"
        );
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/fixtures/sg17/{name}.json"));
        if std::env::var_os("STEVE_SG17_GENERATE_GOLDENS").is_some() {
            assert!(
                !file.exists(),
                "goldens cannot silently overwrite an existing baseline"
            );
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(
                &file,
                serde_json::to_vec_pretty(&String::from_utf8(bytes).unwrap()).unwrap(),
            )
            .unwrap();
        } else {
            let expected: String = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            assert_eq!(bytes, expected.into_bytes(), "{name}");
        }
    }
}
