use super::*;
use steve_node_api::error as rpc_error;

const ERROR: &str = "stream disconnected before completion: upstream stream interrupted";
const STOP: &str = steve_core::runtime::MODEL_STREAM_INTERRUPTED;

pub(super) async fn say(h: &H, exec: &ExecutionRef, role: Role, parts: &[&str]) {
    let subject = Subject::Execution(exec.clone());
    h.node
        .emit(
            subject.clone(),
            EventBody::MessageStarted {
                message: exec.attempt().to_string(),
                block: "b".into(),
                role,
            },
        )
        .await
        .unwrap();
    let mut offset = 0;
    for text in parts {
        h.node
            .emit(
                subject.clone(),
                EventBody::TextDelta {
                    message: exec.attempt().to_string(),
                    block: "b".into(),
                    utf8_offset: offset,
                    text: text.to_string(),
                },
            )
            .await
            .unwrap();
        offset += text.len() as u64;
    }
}

pub(super) fn retry_at(h: &H) -> i64 {
    h.q("SELECT retry_at FROM node_commands WHERE target_attempt=(SELECT active_attempt FROM runtime_sessions WHERE session_id='s1')")
}

/// Advance retry time across a coordinator restart without editing the command ledger: the Node
/// cannot get either retry even one millisecond before its stored 20/60 s deadline.
#[tokio::test]
async fn provider_stream_interruption_retries_the_same_turn_after_tools_and_exhausts_once() {
    let mut h = clocked_hub();
    h.bind();
    let mut exec = h.running("original task").await;
    let turn = turn_of(&exec);
    for (ordinal, delay) in [(1, 20_000), (2, 60_000)] {
        say(
            &h,
            &exec,
            Role::Assistant,
            &[
                "检查 PR\nstream disconnected before completion: ",
                "upstream stream interrupted",
            ],
        )
        .await;
        let mut done = build::done();
        done.acted = true;
        let before = h.clock().now();
        h.node.finish(&exec, done).await.unwrap();
        eventually_sync("provider retry planned", || {
            h.attempts().len() == ordinal + 1
        })
        .await;
        assert!(
            h.ends().is_empty(),
            "turn end must not reach the continue budget"
        );
        assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM runtime_turns"), 1);
        assert_eq!(
            h.q::<i64>("SELECT COUNT(*) FROM events WHERE kind='turn_end'"),
            0
        );
        assert_eq!(h.stat_rows().last().unwrap().1.as_deref(), Some(STOP));
        let at = retry_at(&h);
        assert!(
            at >= before + delay,
            "backoff was skipped: {at} vs {before}"
        );
        assert!(at <= h.clock().now() + delay);
        let spec: String = h.q("SELECT spec FROM runtime_attempts ORDER BY ordinal DESC LIMIT 1");
        let spec: TurnSpec = serde_json::from_str(&spec).unwrap();
        assert_eq!(spec.turn, turn);
        assert_eq!(
            spec.run.prompt.len(),
            1,
            "resume context; do not replay the task"
        );
        let Part::Text { text } = &spec.run.prompt[0] else {
            panic!("text reminder")
        };
        assert!(
            text.contains("模型服务或传输中断")
                && text.contains("不要重做")
                && text.contains("不占用未交付续跑次数"),
            "{text}"
        );
        if ordinal == 1 {
            let replay = h.seen.seen.lock().unwrap().clone();
            h.advance_to(at - 1_234).await;
            assert_eq!(h.count("start_turn"), 1);
            h.restart_hub().await;
            assert_eq!(retry_at(&h), at, "restart must keep the backoff and count");
            Sink(Arc::downgrade(&h.rt))
                .commit(EventBatch { events: replay })
                .await
                .unwrap();
            assert_eq!(
                h.attempts().len(),
                2,
                "replay must not create another retry"
            );
        }
        assert_eq!(h.count("start_turn"), ordinal, "not sent before backoff");
        h.advance_to(at - 1).await;
        assert_eq!(
            h.count("start_turn"),
            ordinal,
            "not sent one millisecond early"
        );
        h.advance_to(at).await;
        eventually_sync("retry reached Node after backoff", || {
            h.count("start_turn") == ordinal + 1
        })
        .await;
        assert!(h.clock().now() >= at);
        exec = h.attempts().last().unwrap().clone();
    }
    say(&h, &exec, Role::Assistant, &[ERROR]).await;
    let mut done = build::done();
    done.acted = true;
    h.node.finish(&exec, done).await.unwrap();
    eventually_sync("provider failures exhausted", || h.ends().len() == 1).await;
    let end = h.ends().pop().unwrap();
    assert_eq!(end.1, STOP);
    assert!(end.2.unwrap().contains("已自动重试 2 次仍失败"));
    assert_eq!(h.attempts().len(), 3);
    assert_eq!(
        h.q::<String>("SELECT status FROM sessions WHERE id='s1'"),
        "error"
    );
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM events WHERE kind='turn_end'"),
        1
    );
    // A later user turn has a fresh execution-error budget and cannot inherit the old text.
    let next = h.running("new task").await;
    h.node.finish(&next, build::done()).await.unwrap();
    eventually_sync("new ordinary end", || h.ends().len() == 2).await;
    assert_eq!(h.ends()[1].1, "end_turn");
    let next = h.running("another interrupted task").await;
    say(&h, &next, Role::Assistant, &[ERROR]).await;
    h.node.finish(&next, build::done()).await.unwrap();
    eventually_sync("new turn has its own retry budget", || {
        h.attempts().len() == 6
    })
    .await;
    assert_ne!(turn_of(&next), turn);
    assert_eq!(
        h.q::<i64>("SELECT retry_ordinal FROM runtime_turns ORDER BY rowid DESC LIMIT 1"),
        1
    );
    assert_eq!(retry_at(&h), h.clock().now() + 20_000);
    assert_eq!(h.ends().len(), 2);
}

#[tokio::test]
async fn provider_stream_interruption_accepts_adapter_errors_and_stop_reasons() {
    for (stop, error) in [
        ("error", Some("connection reset by peer")),
        ("error", None),
        ("end_turn", Some(ERROR)),
        ("transport_error", None),
        ("stream_interrupted", None),
        ("model_stream_interrupted", Some("")),
    ] {
        let h = hub();
        h.bind();
        let exec = h.running("work").await;
        if stop == "error" && error.is_none() {
            say(&h, &exec, Role::Assistant, &[ERROR]).await;
        }
        let mut c = build::done();
        c.stop_reason = stop.into();
        c.error = error.map(|e| rpc_error(Code::Internal, e));
        c.acted = true;
        h.node.finish(&exec, c).await.unwrap();
        eventually_sync("provider retry", || h.attempts().len() == 2).await;
        assert!(h.ends().is_empty());
        assert_eq!(h.stat_rows().last().unwrap().1.as_deref(), Some(STOP));
    }
}

#[tokio::test]
async fn provider_stream_interruption_ignores_quoted_user_and_thought_output_and_user_stop() {
    for (role, output, cancelled) in [
        (
            Role::Assistant,
            "错误示例：stream disconnected before completion: upstream stream interrupted",
            false,
        ),
        (Role::Assistant, "`upstream stream interrupted`", false),
        (Role::Thought, ERROR, false),
        (Role::User, ERROR, false),
        (Role::Assistant, ERROR, true),
    ] {
        let h = hub();
        h.bind();
        let exec = h.running("work").await;
        say(&h, &exec, role, &[output]).await;
        let mut c = build::done();
        if cancelled {
            c.outcome = Outcome::Cancelled;
            c.stop_reason = "cancelled".into();
        }
        h.node.finish(&exec, c).await.unwrap();
        eventually_sync("ordinary end", || h.ends().len() == 1).await;
        assert_eq!(h.attempts().len(), 1);
        assert_eq!(
            h.ends()[0].1,
            if cancelled { "cancelled" } else { "end_turn" }
        );
    }
}

#[tokio::test]
async fn provider_stream_interruption_reads_text_and_end_applied_in_the_same_batch() {
    let h = hub();
    h.bind();
    let exec = h.running("work").await;
    *h.seen.stash.lock().unwrap() = Some(vec![]);
    say(
        &h,
        &exec,
        Role::Assistant,
        &[
            "stream disconnected before completion: ",
            "upstream stream interrupted",
        ],
    )
    .await;
    h.node.finish(&exec, build::done()).await.unwrap();
    let batches = h.seen.stash.lock().unwrap().take().unwrap();
    Sink(Arc::downgrade(&h.rt))
        .commit(EventBatch {
            events: batches.into_iter().flat_map(|b| b.events).collect(),
        })
        .await
        .unwrap();
    eventually_sync("batched diagnostic retried", || h.attempts().len() == 2).await;
    assert!(h.ends().is_empty());
    assert_eq!(h.stat_rows().last().unwrap().1.as_deref(), Some(STOP));
}

#[tokio::test]
async fn provider_stream_interruption_honors_delivery_question_and_cancel_during_backoff() {
    for state in ["closing", "question", "ended", "cancel"] {
        let h = hub();
        h.bind();
        let exec = h.running("work").await;
        h.store.transaction::<_, StoreError>(|tx| {
            tx.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','default',1,'test','run','执行',1,1)", [])?;
            tx.execute("UPDATE sessions SET item_id='i1' WHERE id='s1'", [])?;
            if state == "closing" {
                tx.execute("UPDATE items SET phase='closing' WHERE id='i1'", [])?;
            } else if state == "question" {
                tx.execute("INSERT INTO decisions(id,workspace_id,item_id,session_id,kind,subject_type,subject_id,question,status,requested_by,created) VALUES ('d1','default','i1','s1','ask','item','i1','input','open','steve',1)", [])?;
            } else if state == "ended" {
                tx.execute("UPDATE sessions SET status='ended' WHERE id='s1'", [])?;
            }
            Ok(())
        }).unwrap();
        let mut c = build::done();
        c.outcome = Outcome::Failed;
        c.stop_reason = "error".into();
        c.error = Some(rpc_error(Code::Internal, ERROR));
        c.acted = true;
        h.node.finish(&exec, c).await.unwrap();
        if state == "cancel" {
            eventually_sync("backoff planned", || h.attempts().len() == 2).await;
            assert!(h.rt.cancel_attempt(S, exec.attempt().as_str()).unwrap());
        }
        eventually_sync("boundary respected", || h.ends().len() == 1).await;
        assert_eq!(h.count("start_turn"), 1, "{state}: no automatic start");
        assert_eq!(h.attempts().len(), if state == "cancel" { 2 } else { 1 });
        assert_eq!(
            h.ends()[0].1,
            if state == "cancel" { "cancelled" } else { STOP }
        );
    }
}

#[tokio::test]
async fn discarded_failure_details_never_reclassify_or_retry_old_completion_hints() {
    for stop in [
        "error",
        STOP,
        steve_core::runtime::MODEL_SERVICE_UNAVAILABLE,
        steve_node_api::NODE_RESTARTED,
    ] {
        let h = clocked_hub();
        h.bind();
        let exec = h.running("retained failure after journal upgrade").await;
        let mut completion = build::done();
        completion.outcome = Outcome::Failed;
        completion.stop_reason = stop.into();
        completion.transient = true;
        completion.error = Some(NodeReason::error_detail_discarded(
            "error details from before the upgrade were discarded",
        ));
        let expected = serde_json::to_value(&completion).unwrap();
        h.node.finish(&exec, completion).await.unwrap();
        eventually_sync("discarded failure ends its turn", || h.ends().len() == 1).await;
        assert_eq!(h.ends()[0].1, "node.error_detail_discarded");
        assert_eq!(
            h.q::<String>("SELECT status FROM sessions WHERE id='s1'"),
            "error"
        );
        let stored: String =
            h.q("SELECT completion FROM runtime_attempts ORDER BY ordinal DESC LIMIT 1");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stored).unwrap(),
            expected
        );
        h.advance_to(h.clock().now() + 20 * 60_000).await;
        assert_eq!(
            h.count("start_turn"),
            1,
            "a discarded cause cannot authorize retry"
        );
        assert_eq!(h.attempts().len(), 1);
        assert_eq!(h.q::<i64>("SELECT retry_ordinal FROM runtime_turns"), 0);
    }
}
