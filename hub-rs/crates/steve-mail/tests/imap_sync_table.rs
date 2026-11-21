//! Command failures must preserve the unconfirmed window for the next sync.
mod support;

use steve_mail::fake::fixtures;
use steve_mail::{
    AdapterError, FetchedMessage, FolderState, MailError, MailSink, ScopeMode, SyncState,
};
use steve_modules::Clock;
use steve_modules::testkit::WS;
use support::*;

#[derive(Default)]
struct Sink {
    messages: Vec<FetchedMessage>,
    fail: bool,
}

#[async_trait::async_trait]
impl MailSink for Sink {
    async fn put_batch(&mut self, messages: Vec<FetchedMessage>) -> Result<(), MailError> {
        if self.fail {
            return Err(MailError::Other("table store failure".into()));
        }
        self.messages.extend(messages);
        Ok(())
    }
    async fn flags_batch(&mut self, _: Vec<(String, Vec<String>)>) -> Result<(), MailError> {
        Ok(())
    }
    async fn removed(&mut self, _: &str) -> Result<(), MailError> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn command_failures_keep_cursors_and_replay_the_window() {
    let stages = ["examine", "search", "sizes", "bodies", "store"];
    for (stage, name) in stages.into_iter().enumerate() {
        let h = setup().await;
        h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
        let conn = h.connect().await;
        let cx = h.conns.cx(WS, &conn).unwrap();
        let adapter = h.mail.adapter("imap").unwrap();
        let mut state = SyncState {
            folders: vec![FolderState {
                ext_id: "INBOX".into(),
                name: "INBOX".into(),
                role: "inbox".into(),
                mode: ScopeMode::Receive,
                uidvalidity: None,
                last_uid: None,
                modseq: None,
                cursor: None,
                synced: None,
                flags_checked: None,
            }],
            cursor: None,
            sync_days: 30,
            now: h.env.clock.now(),
        };
        let mut sink = Sink {
            fail: name == "store",
            ..Default::default()
        };
        if name == "bodies" {
            h.imap.fail_next("UID FETCH", "OK scripted empty sizes");
        }
        if name != "store" {
            let command = match name {
                "examine" => "EXAMINE",
                "search" => "UID SEARCH",
                _ => "UID FETCH",
            };
            h.imap
                .fail_next(command, "NO [UNAVAILABLE] scripted failure");
        }
        let start = h.imap.commands().len();
        let result = adapter.sync(&cx, &mut state, &mut sink).await;
        let expected = if name == "store" {
            AdapterError::Store("table store failure".into())
        } else {
            AdapterError::Rejected("邮件服务器拒绝了这个操作".into())
        };
        let error = result.unwrap_err();
        if name == "store" {
            assert_eq!(error, expected, "{name}");
        } else {
            let command = match name {
                "examine" => "EXAMINE",
                "search" => "UID SEARCH",
                _ => "UID FETCH",
            };
            let AdapterError::Rejected(message) = error else {
                panic!("{name}: {error:?}")
            };
            assert!(
                message.contains("INBOX") && message.contains(command),
                "{name}: {message}"
            );
        }
        let commands: Vec<_> = h.imap.commands()[start..]
            .iter()
            .filter_map(|c| {
                if c.contains(" EXAMINE ") {
                    Some("examine")
                } else if c.contains(" UID SEARCH ") {
                    Some("search")
                } else if c.contains("(UID RFC822.SIZE)") {
                    Some("sizes")
                } else if c.contains("BODY.PEEK[]") {
                    Some("bodies")
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(commands, stages[..=stage.min(3)], "{name}");
        let folder = &state.folders[0];
        assert_eq!(folder.uidvalidity.is_some(), stage > 0, "{name}");
        assert_eq!(
            (folder.last_uid, folder.synced, folder.cursor.as_deref()),
            (None, None, None),
            "{name}"
        );
        assert!(sink.messages.is_empty(), "{name}");
        sink.fail = false;
        let report = adapter.sync(&cx, &mut state, &mut sink).await.unwrap();
        assert_eq!(report.fetched, 1, "{name}");
        assert_eq!(state.folders[0].last_uid, Some(1), "{name}");
        assert_eq!(state.folders[0].synced, Some(state.now), "{name}");
        assert_eq!(sink.messages.len(), 1, "{name}");
        assert!(sink.messages.iter().all(|m| m.backfill), "{name}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn transient_and_store_errors_stop_before_the_next_folder() {
    for failure in ["transient", "store"] {
        let h = setup().await;
        let conn = h.connect().await;
        let adapter = h.mail.adapter("imap").unwrap();
        let cx = h.conns.cx(WS, &conn).unwrap();
        let mut state = SyncState {
            folders: ["INBOX", "Sent"]
                .into_iter()
                .map(|name| FolderState {
                    ext_id: name.into(),
                    name: name.into(),
                    role: name.into(),
                    mode: ScopeMode::Record,
                    uidvalidity: None,
                    last_uid: None,
                    modseq: None,
                    cursor: None,
                    synced: None,
                    flags_checked: None,
                })
                .collect(),
            cursor: None,
            sync_days: 30,
            now: h.env.clock.now(),
        };
        let mut sink = Sink::default();
        adapter.sync(&cx, &mut state, &mut sink).await.unwrap();
        let before = state.clone();
        h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
        h.inject("Sent", fixtures::get("big5.eml").unwrap());
        if failure == "transient" {
            h.imap.fail_next("EXAMINE", "OK without UIDVALIDITY");
        }
        sink.fail = failure == "store";
        let start = h.imap.commands().len();
        let error = adapter.sync(&cx, &mut state, &mut sink).await.unwrap_err();
        assert!(
            matches!(
                (&error, failure),
                (AdapterError::Transient(_), "transient") | (AdapterError::Store(_), "store")
            ),
            "{failure}: {error:?}"
        );
        assert_eq!(state, before, "{failure}");
        assert!(
            h.imap.commands()[start..]
                .iter()
                .all(|c| !(c.contains("EXAMINE") && c.contains("Sent")))
        );
    }
}
