use super::*;
use crate::{testkit::TestEnv, vault::keystore::FileKeyStore};
use std::{future::Future, pin::Pin, task::Poll};
use tokio::sync::Semaphore;

struct BlockedSink {
    entered: mpsc::UnboundedSender<()>,
    release: Semaphore,
}
#[async_trait]
impl SessionSink for BlockedSink {
    async fn send(&self, _: &str, _: Option<&str>, _: Option<&str>, _: &str) {
        self.entered.send(()).unwrap();
        self.release.acquire().await.unwrap().forget();
    }
}
fn service(sink: Arc<dyn SessionSink>) -> (TestEnv, tempfile::TempDir, SecretUses) {
    let env = TestEnv::new(|_| {});
    let home = tempfile::tempdir().unwrap();
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(home.path()).unwrap()),
    );
    let uses = SecretUses::new(vault, home.path(), sink);
    (env, home, uses)
}
async fn assert_pending<F: Future>(mut future: Pin<&mut F>) {
    std::future::poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "closed before accepted work exited"
        );
        Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn closing_keeps_accepted_children_and_each_unconfirmed_notice() {
    let (entered, mut rx) = mpsc::unbounded_channel();
    let sink = Arc::new(BlockedSink {
        entered,
        release: Semaphore::new(0),
    });
    let (_env, _home, uses) = service(sink.clone());
    let parent = uses.0.lifecycle.admit("accepted parent", None).unwrap();
    uses.begin_close();
    let notes = vec![
        ("default".into(), None, None, "first".into()),
        ("default".into(), None, None, "second".into()),
    ];
    uses.enqueue(Job::Notes(notes), parent.child("necessary child"))
        .unwrap();
    parent.finish();
    rx.recv().await.unwrap();
    let close = uses.close_and_wait();
    tokio::pin!(close);
    assert_pending(close.as_mut()).await;
    sink.release.add_permits(1);
    rx.recv().await.unwrap();
    assert_pending(close.as_mut()).await;
    sink.release.add_permits(1);
    let report = close.await;
    assert!(report.local_work_exited());
    assert_eq!(report.work.len(), 2);
    assert_eq!(report.unconfirmed_notifications.len(), 2);
    assert!(
        report
            .unconfirmed_notifications
            .iter()
            .all(|n| n.call_returned)
    );
    assert_eq!(report.unconfirmed_notifications[1].text, "second");
    assert!(uses.close_and_wait().await.local_work_exited());
}

struct BlockedNodes {
    entered: mpsc::UnboundedSender<()>,
    release: Semaphore,
}
#[async_trait]
impl UseNodes for BlockedNodes {
    async fn place(&self, _: &str, _: &str) -> VaultResult<Option<NodePlace>> {
        self.entered.send(()).unwrap();
        self.release.acquire().await.unwrap().forget();
        Err(VaultError::Denied)
    }
    async fn run(&self, _: NodeUse, _: Arc<dyn UseValues>) -> RunOutput {
        panic!("failed placement must not run a command")
    }
}
#[tokio::test]
async fn cancelled_request_waiter_does_not_cancel_owned_work_or_lose_failure() {
    let (entered, mut rx) = mpsc::unbounded_channel();
    let nodes = Arc::new(BlockedNodes {
        entered,
        release: Semaphore::new(0),
    });
    let (sink_entered, _sink_rx) = mpsc::unbounded_channel();
    let (_env, _home, uses) = service(Arc::new(BlockedSink {
        entered: sink_entered,
        release: Semaphore::new(0),
    }));
    uses.set_nodes(nodes.clone());
    let me = uses.clone();
    let caller = tokio::spawn(async move {
        me.request(
            &UseCx {
                ws: "default".into(),
                user: "usr_dev".into(),
                bot_id: "bot".into(),
                item_id: None,
                session_id: Some("session".into()),
            },
            UseRequest::default(),
        )
        .await
    });
    rx.recv().await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let close = uses.close_and_wait();
    tokio::pin!(close);
    assert_pending(close.as_mut()).await;
    nodes.release.add_permits(1);
    let report = close.await;
    assert!(report.local_work_exited());
    let request = &report.work[0];
    assert_eq!(request.completion, Completion::Failed);
    assert_eq!(request.result(), Some(&Err(VaultError::Denied)));
    assert_eq!(request.error(), Some(&VaultError::Denied));
    assert!(report.unconfirmed_notifications.is_empty());
}

struct PanickingSink;
#[async_trait]
impl SessionSink for PanickingSink {
    async fn send(&self, _: &str, _: Option<&str>, _: Option<&str>, _: &str) {
        panic!("sink ended without confirmation")
    }
}
#[tokio::test]
async fn unwound_child_is_unconfirmed_and_retains_unsent_payload() {
    let (_env, _home, uses) = service(Arc::new(PanickingSink));
    let work = uses.0.lifecycle.admit("notice", None).unwrap();
    uses.enqueue(
        Job::Notes(vec![
            ("default".into(), None, None, "retain".into()),
            ("default".into(), None, None, "also retain".into()),
        ]),
        work,
    )
    .unwrap();
    let report = uses.close_and_wait().await;
    assert!(report.worker_joined);
    assert!(!report.local_work_exited());
    assert!(
        report
            .work
            .iter()
            .all(|r| r.completion == Completion::Unconfirmed)
    );
    assert_eq!(report.unconfirmed_notifications.len(), 2);
    assert_eq!(report.unconfirmed_notifications[0].text, "retain");
    assert!(
        report
            .unconfirmed_notifications
            .iter()
            .all(|n| !n.call_returned)
    );
    assert_eq!(report.unconfirmed_notifications[1].text, "also retain");
}

#[tokio::test]
async fn close_waiter_can_be_cancelled_and_report_does_not_keep_the_store() {
    let (entered, mut rx) = mpsc::unbounded_channel();
    let sink = Arc::new(BlockedSink {
        entered,
        release: Semaphore::new(0),
    });
    let (env, home, uses) = service(sink.clone());
    let work = uses.0.lifecycle.admit("notice", None).unwrap();
    uses.enqueue(
        Job::Notes(vec![("default".into(), None, None, "retain".into())]),
        work,
    )
    .unwrap();
    rx.recv().await.unwrap();
    let mut close = Box::pin(uses.close_and_wait());
    assert_pending(close.as_mut()).await;
    drop(close);
    sink.release.add_permits(1);
    let report = uses.close_and_wait().await;
    assert!(report.local_work_exited());
    let TestEnv {
        dir,
        modules,
        gateway,
        clock,
        events,
    } = env;
    let path = dir.path().join("steve.sqlite3");
    assert!(steve_store::Store::open(&path, steve_store::NoPublisher).is_err());
    drop((uses, modules, gateway, clock, events, home));
    let reopened = steve_store::Store::open(&path, steve_store::NoPublisher).unwrap();
    assert_eq!(report.unconfirmed_notifications[0].text, "retain");
    drop(reopened);
}
