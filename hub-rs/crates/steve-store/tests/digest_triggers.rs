//! A normal Store open must execute the migration, not merely leave a SQL file on disk.
#[test]
fn current_store_has_no_implicit_digest_membership_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        steve_store::Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher)
            .unwrap();
    store
        .with_conn(|c| {
            let n: i64 = c.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND
                 (name LIKE 'digest_pending_%' OR name='digest_receipt_ack'
                  OR name LIKE 'digest_chan_messages_%' OR name LIKE 'digest_mail_messages_%')",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(
                n, 0,
                "migration 114 must be included in the old runner before explicit maintenance"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn conditional_delivery_updates_only_synchronize_matched_rows() {
    use serde_json::json;
    use steve_store::channel_message_repo::{delivery, ingress};
    use steve_store::{NoPublisher, Store, StoreError, WS};

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    store
        .write(|w| {
            w.legacy().execute(
                "INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, created, updated)
                 VALUES ('s', ?1, 'test', 'group', 's', 'S', 1, 1)",
                [WS],
            )?;
            let message = ingress::insert(
                w,
                &ingress::NewInbound {
                    ws: WS,
                    space_id: "s",
                    external_id: "received",
                    thread_key: None,
                    reply_to: None,
                    sender_account_id: None,
                    sender_party_id: None,
                    text: "message",
                    attachments: &json!([]),
                    mentions: &json!([]),
                    created: 1,
                    content_key: None,
                    fallback_from: None,
                    history: None,
                    seen_by: 0,
                },
            )?
            .expect("message was inserted");
            let position = |w: &steve_store::W<'_>| {
                use rusqlite::OptionalExtension;
                w.legacy()
                    .query_row(
                        "SELECT position FROM digest_pending WHERE message_id = ?",
                        [&message],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional()
            };
            let first = position(w)?.expect("message was queued");
            let keep = delivery::SentAt::Keep;
            assert!(!delivery::transition(w, &message, &["draft"], "pending", keep)?);
            assert_eq!(position(w)?, Some(first));
            assert!(delivery::transition(w, &message, &[], "sent", keep)?);
            assert_eq!(position(w)?, Some(first));
            assert!(delivery::transition(w, &message, &[], "pending", keep)?);
            assert_eq!(position(w)?, None);
            Ok::<_, StoreError>(())
        })
        .unwrap();
}
