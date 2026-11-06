//! The media of a channel side outside the built-ins: a [`MediaFetch`] registered for a test
//! adapter is the one the Hub calls to fetch a resource of a message its connection received
//! (with the same [`MessageRef`] recall and reactions get), and the one that reads an old message
//! again from its raw event when it is shown.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use steve_channels::Channels;
use steve_channels::channel::MessageRef;
use steve_channels::ext::{MediaError, MediaFetch};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::media::{self, Limits};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, ConnCx, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_store::base::Viewer;
use tempfile::TempDir;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRtest";

/// Serves the test adapter: every download is recorded and answered with [`PNG`]; a raw event
/// marked `legacy` of this very message reads as `再读的正文` with one image.
#[derive(Default)]
struct TestMedia(Mutex<Vec<(MessageRef, String, bool)>>);

#[async_trait::async_trait]
impl MediaFetch for TestMedia {
    fn adapter_id(&self) -> &str {
        FakeChannel::ID
    }

    async fn download(
        &self,
        _: &ConnCx,
        msg: &MessageRef,
        key: &str,
        image: bool,
        _: usize,
    ) -> Result<Vec<u8>, MediaError> {
        self.0
            .lock()
            .unwrap()
            .push((msg.clone(), key.to_string(), image));
        Ok(PNG.to_vec())
    }

    fn reparse(&self, msg: &MessageRef, raw: &Value) -> Option<(String, Vec<Value>)> {
        (raw["legacy"] == true && raw["id"].as_str() == msg.id()).then(|| {
            (
                "再读的正文".to_string(),
                vec![json!({"type": "image", "key": "img_again", "source": "message"})],
            )
        })
    }
}

struct H {
    env: TestEnv,
    _keys: TempDir,
    ch: Channels,
    media: Arc<TestMedia>,
    con: String,
}

fn setup() -> H {
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core.clone());
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
    });
    *core.store.lock().unwrap() = Some(env.store().clone());
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    let fake = Arc::new(FakeChannel::new());
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(Vec::new(), true, |_| None),
    )
    .provider(fake.clone())
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let media = Arc::new(TestMedia::default());
    let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow))
        .adapter(fake.clone())
        .media(media.clone())
        .build();
    let (con, _) = FakeChannel::seed_connection(env.store(), &caller(WS, ME), None).expect("bot");
    H {
        env,
        _keys: keys,
        ch,
        media,
        con,
    }
}

fn sql(h: &H, s: &str) {
    let s = s.to_string();
    h.env
        .store()
        .transaction(move |tx| -> Result<(), steve_store::StoreError> {
            tx.execute_batch(&s)?;
            Ok(())
        })
        .expect("sql");
}

async fn received(h: &H, ext: &str, text: &str, attachments: Vec<Value>, legacy: bool) -> String {
    let mut m = FakeChannel::message("C_MEDIA", ext, "ulilei", text);
    m.event_id = Some(format!("ev_{ext}"));
    m.attachments = attachments;
    m.raw = json!({"id": ext, "legacy": legacy});
    let r = h.ch.on_message(WS, &h.con, m).await.expect("message");
    r.message_id
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resource_is_fetched_through_the_view_of_the_receiving_adapter() {
    let h = setup();
    let msg = received(
        &h,
        "m_1",
        "看图",
        vec![json!({"type": "image", "key": "img_1", "source": "message"})],
        false,
    )
    .await;
    // the remote's delivery id of the message, as this connection knows it
    sql(
        &h,
        &format!(
            "INSERT INTO chan_message_seen (connection_id, source_id, message_id) VALUES ('{}', 'dlv_m1', '{msg}')",
            h.con
        ),
    );
    let got = media::fetch_for_viewer(&h.ch, &Viewer::bare(WS, ME), &msg, 0, Limits::default())
        .await
        .expect("fetched");
    assert_eq!((got.mime, got.bytes.as_slice()), ("image/png", PNG));
    let calls = h.media.0.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    let (r, key, image) = &calls[0];
    assert_eq!((key.as_str(), *image), ("img_1", true));
    assert_eq!(r.id(), Some("m_1"));
    assert_eq!(r.space_ext_id, "C_MEDIA");
    assert_eq!(r.source_id.as_deref(), Some("dlv_m1"));
    assert_eq!(
        r.sender.as_ref().map(|s| s.handle.as_str()),
        Some(FakeChannel::handle("ulilei").handle.as_str())
    );
    assert!(r.created_exact);
    assert_eq!(r.text.as_deref(), Some("看图"));

    // what is shown: a url for the resource, no key
    let shown = h
        .env
        .store()
        .reader(|c| Ok::<_, steve_store::StoreError>(media::present(c, &msg)))
        .expect("present")
        .expect("stored");
    assert_eq!(
        shown.1[0]["url"],
        format!("/api/chan-messages/{msg}/resources/0")
    );
    assert!(shown.1[0].get("key").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_old_message_is_read_again_by_the_view_of_its_adapter() {
    let h = setup();
    let msg = received(&h, "m_2", "[图片]", Vec::new(), true).await;
    let shown = h
        .env
        .store()
        .reader(|c| Ok::<_, steve_store::StoreError>(media::present(c, &msg)))
        .expect("present")
        .expect("stored");
    assert_eq!(shown.0.as_deref(), Some("再读的正文"));
    assert_eq!(shown.1[0]["type"], "image");
    assert_eq!(
        shown.1[0]["url"],
        format!("/api/chan-messages/{msg}/resources/0")
    );
    // the re-read image is fetched with its key
    media::fetch_for_viewer(&h.ch, &Viewer::bare(WS, ME), &msg, 0, Limits::default())
        .await
        .expect("fetched");
    assert_eq!(h.media.0.lock().unwrap()[0].1, "img_again");
    // nothing was written back
    let text: String = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT text FROM chan_messages WHERE id = ?",
                [&msg],
                |r| r.get(0),
            )?)
        })
        .expect("text");
    assert_eq!(text, "[图片]");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resource_is_fetched_only_by_the_adapter_its_key_belongs_to() {
    let h = setup();
    let msg = received(
        &h,
        "m_3",
        "看图",
        vec![json!({"type": "image", "key": "img_3", "source": "message"})],
        false,
    )
    .await;
    // stored with the adapter its key belongs to; never shown
    let stored: String = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT attachments FROM chan_messages WHERE id = ?",
                [&msg],
                |r| r.get(0),
            )?)
        })
        .expect("attachments");
    let stored: Value = serde_json::from_str(&stored).expect("json");
    assert_eq!(stored[0]["adapter"], FakeChannel::ID);
    let shown = h
        .env
        .store()
        .reader(|c| Ok::<_, steve_store::StoreError>(media::present(c, &msg)))
        .expect("present")
        .expect("stored");
    assert!(shown.1[0].get("adapter").is_none(), "{}", shown.1);

    // a key of another adapter (a copy merged from another side): no connection of it is left, so
    // it is not handed to this adapter's view
    sql(
        &h,
        &format!(
            "UPDATE chan_messages SET attachments = json_set(attachments, '$[0].adapter', 'other_side') WHERE id = '{msg}'"
        ),
    );
    let e = media::fetch_for_viewer(&h.ch, &Viewer::bare(WS, ME), &msg, 0, Limits::default())
        .await
        .expect_err("not this adapter's key");
    assert_eq!(e, MediaError::Disconnected);
    assert!(h.media.0.lock().unwrap().is_empty());
    let shown = h
        .env
        .store()
        .reader(|c| Ok::<_, steve_store::StoreError>(media::present(c, &msg)))
        .expect("present")
        .expect("stored");
    assert_eq!(shown.1[0]["unavailable"], MediaError::Disconnected.reason());
}
