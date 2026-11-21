//! Service-owned drain evidence, without Hub/Node-final acknowledgement claims.
use super::*;
use std::{future::Future, pin::Pin, task::Poll};
use steve_modules::vault::inject::RunOutput;
use steve_modules::vault::r#use::Completion;
use steve_modules::vault::{NodePlace, NodeUse, UseNodes, UseOutcome, UseValues, VaultResult};
use tokio::sync::{Semaphore, mpsc};

struct SinkGate {
    entered: mpsc::UnboundedSender<()>,
    release: Semaphore,
}
#[async_trait]
impl SessionSink for SinkGate {
    async fn send(&self, _: &str, _: Option<&str>, _: Option<&str>, _: &str) {
        self.entered.send(()).unwrap();
        self.release.acquire().await.unwrap().forget();
    }
}
fn sink() -> (Arc<SinkGate>, mpsc::UnboundedReceiver<()>) {
    let (entered, receiver) = mpsc::unbounded_channel();
    (
        Arc::new(SinkGate {
            entered,
            release: Semaphore::new(0),
        }),
        receiver,
    )
}
fn owned(e: &Env, sink: Arc<dyn SessionSink>) -> (SecretUses, Modules) {
    let uses = SecretUses::new(e.vault.clone(), e.home.path(), sink);
    let mut reg = Registry::new();
    reg.decision(SUBJECT, uses.decision_handler());
    let modules = Modules::new(e.env.store().clone(), e.env.gateway.clone(), reg);
    uses.start();
    (uses, modules)
}
async fn assert_pending<F: Future>(mut future: Pin<&mut F>) {
    std::future::poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "drain preceded actual work exit"
        );
        Poll::Ready(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn current_hourly_finishes_cleanup_and_notification_before_exit() {
    let e = Env::new();
    e.uses.close_and_wait().await;
    let item = e.token_item();
    let (sink, mut entered) = sink();
    let (uses, _modules) = owned(&e, sink.clone());
    mcp::call(&uses, &e.cx(), mcp::USE, &use_args(&item, "true"))
        .await
        .unwrap();
    let id = e.last_use();
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE secret_uses SET created = created - ? WHERE id = ?",
                params![2 * 86_400_000_i64, id],
            )?;
            Ok(())
        })
        .unwrap();
    let me = uses.clone();
    let tick = tokio::spawn(async move { me.hourly().await });
    entered.recv().await.unwrap();
    let close = uses.close_and_wait();
    tokio::pin!(close);
    assert_pending(close.as_mut()).await;
    assert_eq!(
        e.q::<String>("SELECT status FROM secret_uses WHERE id = ?", [&id]),
        "expired"
    );
    sink.release.add_permits(1);
    tick.await.unwrap().unwrap();
    let report = close.await;
    assert!(report.local_work_exited());
    assert_eq!(report.pending_approvals, 0);
    assert_eq!(report.unconfirmed_notifications.len(), 1);
    assert!(report.unconfirmed_notifications[0].call_returned);
    assert!(
        report.unconfirmed_notifications[0]
            .text
            .contains("一天内没有处理")
    );
    assert_eq!(uses.hourly().await.unwrap_err().status(), 409);
}

struct RunningNode {
    entered: mpsc::UnboundedSender<()>,
    release: Semaphore,
}
#[async_trait]
impl UseNodes for RunningNode {
    async fn place(&self, _: &str, _: &str) -> VaultResult<Option<NodePlace>> {
        Ok(Some(NodePlace {
            device: "n_far".into(),
            files: false,
        }))
    }
    async fn run(&self, _: NodeUse, _: Arc<dyn UseValues>) -> RunOutput {
        self.entered.send(()).unwrap();
        self.release.acquire().await.unwrap().forget();
        RunOutput {
            exit_code: Some(1),
            timed_out: false,
            stdout: "original output".into(),
            stderr: "original failure".into(),
            error: None,
        }
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn accepted_decision_waits_for_command_and_required_notice_and_keeps_failure() {
    let e = Env::new();
    e.uses.close_and_wait().await;
    let item = e.token_item();
    let (sink, mut notice) = sink();
    let (uses, modules) = owned(&e, sink.clone());
    let (entered, mut running) = mpsc::unbounded_channel();
    let node = Arc::new(RunningNode {
        entered,
        release: Semaphore::new(0),
    });
    uses.set_nodes(node.clone());
    mcp::call(&uses, &e.cx(), mcp::USE, &use_args(&item, "exit 1"))
        .await
        .unwrap();
    let id = e.last_use();
    let d = e.open_decision(&id);
    decisions::dispatch(&modules, &caller(WS, ME), &d.id, "once", "")
        .unwrap()
        .unwrap();
    running.recv().await.unwrap();
    let close = uses.close_and_wait();
    tokio::pin!(close);
    assert_pending(close.as_mut()).await;
    node.release.add_permits(1);
    notice.recv().await.unwrap();
    assert_pending(close.as_mut()).await;
    sink.release.add_permits(1);
    let report = close.await;
    assert!(report.local_work_exited());
    let job = report.work.iter().find(|r| r.kind == "decision").unwrap();
    assert_eq!(job.completion, Completion::Failed);
    assert_eq!(job.use_id.as_deref(), Some(id.as_str()));
    let Some(Ok(UseOutcome::Ran { out, .. })) = job.result() else {
        panic!("lost original result")
    };
    assert_eq!(out.exit_code, Some(1));
    assert_eq!(out.stdout, "original output");
    assert_eq!(out.stderr, "original failure");
    assert_eq!(report.unconfirmed_notifications.len(), 1);
    assert!(report.unconfirmed_notifications[0].call_returned);
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_rejects_new_request_and_decision_without_changing_pending_use() {
    let e = Env::new();
    e.uses.close_and_wait().await;
    let item = e.token_item();
    let (sink, _notice) = sink();
    let (uses, modules) = owned(&e, sink);
    mcp::call(&uses, &e.cx(), mcp::USE, &use_args(&item, "true"))
        .await
        .unwrap();
    let id = e.last_use();
    let d = e.open_decision(&id);
    let report = uses.close_and_wait().await;
    assert!(report.local_work_exited());
    assert_eq!(report.pending_approvals, 1);
    let count: i64 = e.q("SELECT COUNT(*) FROM secret_uses", []);
    let rejected = uses
        .request(&e.cx(), steve_modules::vault::UseRequest::default())
        .await
        .unwrap_err();
    assert_eq!(rejected.status(), 409);
    let decision = decisions::dispatch(&modules, &caller(WS, ME), &d.id, "once", "")
        .unwrap()
        .unwrap_err();
    assert_eq!(decision.status(), 409);
    assert_eq!(e.q::<i64>("SELECT COUNT(*) FROM secret_uses", []), count);
    assert_eq!(
        e.q::<String>("SELECT status FROM secret_uses WHERE id = ?", [&id]),
        "pending"
    );
    assert_eq!(e.open_decision(&id).id, d.id);
    assert!(
        uses.close_and_wait()
            .await
            .unconfirmed_notifications
            .is_empty()
    );
}
