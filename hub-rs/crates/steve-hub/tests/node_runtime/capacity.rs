use super::stream_interruption::{retry_at, say};
use super::*;

const STOP: &str = steve_core::runtime::MODEL_SERVICE_UNAVAILABLE;
const ERROR: &str = "503 Service Unavailable: no eligible upstream account is currently available";

fn failure(error: &str) -> Completion {
    let mut c = build::done();
    c.outcome = Outcome::Failed;
    c.stop_reason = "error".into();
    c.error = Some(ApiError::new(ErrorCode::Internal, error));
    c.acted = true;
    c
}

async fn send_pending(h: &H, count: usize) -> ExecutionRef {
    let at = retry_at(h);
    h.advance_to(at - 1).await;
    assert_eq!(
        h.count("start_turn"),
        count - 1,
        "not sent before recovery and jitter"
    );
    h.advance_to(at).await;
    until("retry sent", || h.count("start_turn") == count).await;
    h.attempts().last().unwrap().clone()
}

#[tokio::test]
async fn capacity_after_tools_retries_the_same_turn_then_exhausts_without_continues() {
    let h = clocked_hub();
    h.bind();
    let mut exec = h.running("original task @tool already finished").await;
    let turn = turn_of(&exec);
    for ordinal in 1..=3 {
        // An adapter error, despite acted=true and transient=false, must be a capacity failure.
        h.node
            .finish(&exec, failure(&format!("{ERROR}; Retry-After: 120")))
            .await
            .unwrap();
        until("capacity retry planned", || {
            h.attempts().len() == ordinal + 1
        })
        .await;
        assert!(h.ends().is_empty());
        assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM runtime_turns"), 1);
        assert_eq!(
            h.q::<i64>("SELECT COUNT(*) FROM events WHERE kind='turn_end'"),
            0
        );
        assert_eq!(h.stat_rows().last().unwrap().1.as_deref(), Some(STOP));
        let delta = retry_at(&h) - h.clock().now_ms();
        assert!((120_000..=125_000).contains(&delta), "{delta}");
        let spec: String = h.q("SELECT spec FROM runtime_attempts ORDER BY ordinal DESC LIMIT 1");
        let spec: TurnSpec = serde_json::from_str(&spec).unwrap();
        assert_eq!(spec.turn, turn);
        assert_eq!(spec.run.prompt.len(), 1);
        let Part::Text { text } = &spec.run.prompt[0] else {
            panic!("reminder");
        };
        assert!(
            text.contains("模型服务容量不足")
                && text.contains("不要重做")
                && text.contains("不占用未交付续跑次数"),
            "{text}"
        );
        exec = send_pending(&h, ordinal + 1).await;
    }
    h.node
        .finish(&exec, failure(&format!("{ERROR}; Retry-After: 120")))
        .await
        .unwrap();
    until("one exhausted end", || h.ends().len() == 1).await;
    assert_eq!(h.attempts().len(), 4);
    assert_eq!(h.ends()[0].1, STOP);
    let error = h.ends()[0].2.clone().unwrap();
    assert!(
        error.contains("已自动重试 3 次仍失败") && error.contains("预计恢复时间：20"),
        "{error}"
    );
    assert_eq!(h.phase(), "ready");
    assert_eq!(
        h.q::<String>("SELECT status FROM sessions WHERE id='s1'"),
        "error"
    );
}

#[tokio::test]
async fn capacity_deadline_and_jitter_survive_restart_and_event_replay() {
    let mut h = clocked_hub();
    h.bind();
    let exec = h.running("work").await;
    *h.seen.stash.lock().unwrap() = Some(vec![]);
    // The text and successful completion are projected in one transaction, as with #435.
    say(
        &h,
        &exec,
        Role::Assistant,
        &[
            "finished work\nAPI Error: 503 Service Unavailable\n",
            "Retry-After: 120",
        ],
    )
    .await;
    h.node.finish(&exec, build::done()).await.unwrap();
    let batches = h.seen.stash.lock().unwrap().take().unwrap();
    let replay: Vec<_> = batches.into_iter().flat_map(|b| b.events).collect();
    Sink(Arc::downgrade(&h.rt))
        .commit(EventBatch {
            events: replay.clone(),
        })
        .await
        .unwrap();
    until("capacity retry", || h.attempts().len() == 2).await;
    let at = retry_at(&h);
    let receipt: String =
        h.q("SELECT receipt FROM node_commands WHERE state='pending' AND method='start_turn'");
    assert!(h.rt.is_busy(S));
    h.advance_to(at - 1_000).await;
    assert_eq!(h.count("start_turn"), 1);
    h.restart_hub().await;
    Sink(Arc::downgrade(&h.rt))
        .commit(EventBatch { events: replay })
        .await
        .unwrap();
    assert_eq!(h.attempts().len(), 2);
    assert_eq!(retry_at(&h), at);
    assert_eq!(
        h.q::<String>(
            "SELECT receipt FROM node_commands WHERE state='pending' AND method='start_turn'"
        ),
        receipt
    );
    let next = send_pending(&h, 2).await;
    h.node.finish(&next, build::done()).await.unwrap();
    until("successful recovery", || h.ends().len() == 1).await;
    assert_eq!(h.ends()[0].1, "end_turn");
    assert_eq!(h.q::<i64>("SELECT retry_ordinal FROM runtime_turns"), 1);
}

#[tokio::test]
async fn capacity_wait_has_a_durable_ceiling_and_never_sends_before_recovery() {
    let mut h = clocked_hub();
    h.bind();
    let exec = h.running("work").await;
    let before = h.clock().now_ms();
    h.node
        .finish(&exec, failure(&format!("{ERROR}; Retry-After: 3600")))
        .await
        .unwrap();
    until("bounded wait", || h.attempts().len() == 2).await;
    let at = retry_at(&h);
    assert!((before + 900_000..=h.clock().now_ms() + 900_000).contains(&at));
    assert!(
        h.ends().is_empty(),
        "no immediate failure card for a long outage"
    );
    assert_eq!(h.count("start_turn"), 1);
    h.restart_hub().await;
    assert_eq!(retry_at(&h), at);
    // The unmodified persisted ceiling expires before any lease, even at retry_at itself.
    h.advance_to(at - 1).await;
    assert!(h.ends().is_empty());
    assert_eq!(h.count("start_turn"), 1);
    h.restart_hub_at(None, Some(at)).await;
    until("capacity wait expired", || h.ends().len() == 1).await;
    assert_eq!(
        h.count("start_turn"),
        1,
        "no request before announced recovery"
    );
    assert_eq!(h.ends()[0].1, STOP);
    let error = h.ends()[0].2.clone().unwrap();
    assert!(
        error.contains("已自动重试 0 次")
            && error.contains("15 分钟上限")
            && error.contains("预计恢复时间：20"),
        "{error}"
    );
}

#[tokio::test]
async fn simultaneous_capacity_outages_use_independent_jitter() {
    let h = clocked_hub();
    let common = h.clock().now_ms() + 120_000;
    let error =
        steve_acp::capacity_summary(&format!("{ERROR}; Retry-After: 120"), common - 120_000);
    let mut times = std::collections::HashSet::new();
    for i in 0..8 {
        let sid = format!("capacity-{i}");
        h.store.transaction::<_, StoreError>(|tx| {
            tx.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','active',1,1)", [&sid])?;
            Ok(())
        }).unwrap();
        h.rt.bind(&Binding {
            session: sid.clone(),
            resource: format!("item:{sid}"),
            prepare: PrepareKind::Scratch,
            choice: build::choice(),
            policy: build::policy(),
        })
        .unwrap();
        h.rt.prompt(&sid, "work", "user", &[]).unwrap();
        until("session started", || h.count("start_turn") == i + 1).await;
        let attempt = h.rt.active_attempt(&sid).unwrap().unwrap();
        let exec = h
            .attempts()
            .into_iter()
            .find(|e| e.attempt().as_str() == attempt)
            .unwrap();
        h.node.finish(&exec, failure(&error)).await.unwrap();
        until("wait planned", || h.attempts().len() == (i + 1) * 2).await;
        let at: i64 = h.q(&format!("SELECT retry_at FROM node_commands WHERE target_attempt=(SELECT active_attempt FROM runtime_sessions WHERE session_id='{sid}')"));
        assert!(
            (common + 1_000..=common + 5_000).contains(&at),
            "{at} vs {common}"
        );
        times.insert(at);
    }
    assert!(
        times.len() > 1,
        "independent samples must spread the common recovery instant"
    );
    assert_eq!(
        h.count("start_turn"),
        8,
        "none of the retries has run early"
    );
    assert!(h.ends().is_empty());
}

#[tokio::test]
async fn capacity_respects_final_text_roles_cancellation_and_committed_boundaries() {
    for state in [
        "quoted",
        "thought",
        "user",
        "cancelled",
        "closing",
        "question",
        "ended",
        "cancel_wait",
    ] {
        let h = clocked_hub();
        h.bind();
        let exec = h.running("work").await;
        let role = match state {
            "thought" => Role::Thought,
            "user" => Role::User,
            _ => Role::Assistant,
        };
        let output = if state == "quoted" {
            format!("错误示例：{ERROR}")
        } else {
            format!("API Error: {ERROR}")
        };
        say(&h, &exec, role, &[&output]).await;
        if matches!(state, "closing" | "question" | "ended") {
            h.store.transaction::<_, StoreError>(|tx| {
                tx.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','default',1,'test','run','执行',1,1)", [])?;
                tx.execute("UPDATE sessions SET item_id='i1' WHERE id='s1'", [])?;
                match state {
                    "closing" => { tx.execute("UPDATE items SET phase='closing' WHERE id='i1'", [])?; }
                    "question" => { tx.execute("INSERT INTO decisions(id,workspace_id,item_id,session_id,kind,subject_type,subject_id,question,status,requested_by,created) VALUES ('d1','default','i1','s1','ask','item','i1','input','open','steve',1)", [])?; }
                    "ended" => { tx.execute("UPDATE sessions SET status='ended' WHERE id='s1'", [])?; }
                    _ => unreachable!(),
                }
                Ok(())
            }).unwrap();
        }
        let mut c = build::done();
        if state == "cancelled" {
            c.outcome = Outcome::Cancelled;
            c.stop_reason = "cancelled".into();
        }
        h.node.finish(&exec, c).await.unwrap();
        if state == "cancel_wait" {
            until("capacity wait", || h.attempts().len() == 2).await;
            assert!(h.rt.cancel_attempt(S, exec.attempt().as_str()).unwrap());
        }
        until("boundary respected", || h.ends().len() == 1).await;
        assert_eq!(h.count("start_turn"), 1, "{state}");
        assert_eq!(
            h.attempts().len(),
            if state == "cancel_wait" { 2 } else { 1 },
            "{state}"
        );
        assert_eq!(
            h.ends()[0].1,
            match state {
                "cancelled" | "cancel_wait" => "cancelled",
                "closing" | "question" | "ended" => STOP,
                _ => "end_turn",
            },
            "{state}"
        );
    }
}

#[tokio::test]
async fn a_question_committed_during_a_capacity_wait_prevents_the_retry() {
    let h = clocked_hub();
    h.bind();
    let exec = h.running("work").await;
    h.node
        .finish(&exec, failure(&format!("{ERROR}; Retry-After: 120")))
        .await
        .unwrap();
    until("capacity wait", || h.attempts().len() == 2).await;
    h.store.transaction::<_, StoreError>(|tx| {
        tx.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','default',1,'test','run','执行',1,1)", [])?;
        tx.execute("UPDATE sessions SET item_id='i1' WHERE id='s1'", [])?;
        tx.execute("INSERT INTO decisions(id,workspace_id,item_id,session_id,kind,subject_type,subject_id,question,status,requested_by,created) VALUES ('d1','default','i1','s1','ask','item','i1','input','open','steve',1)", [])?;
        Ok(())
    }).unwrap();
    h.advance_to(h.clock().now_ms() + 1_000).await;
    until("question boundary ends wait", || h.ends().len() == 1).await;
    assert_eq!(h.count("start_turn"), 1);
    assert_eq!(
        h.q::<String>("SELECT status FROM decisions WHERE id='d1'"),
        "open"
    );
    assert_eq!(h.ends()[0].1, STOP);
    assert!(!h.ends()[0].2.as_ref().unwrap().contains("上限"));
}

#[tokio::test]
async fn capacity_without_a_valid_hint_uses_bounded_backoff() {
    for hint in ["", "; Retry-After: malformed"] {
        let h = clocked_hub();
        h.bind();
        let mut exec = h.running("work").await;
        for (ordinal, delay) in [(1, 20_000), (2, 60_000), (3, 120_000)] {
            let before = h.clock().now_ms();
            h.node
                .finish(&exec, failure(&format!("{ERROR}{hint}")))
                .await
                .unwrap();
            until("fallback retry planned", || {
                h.attempts().len() == ordinal + 1
            })
            .await;
            assert!(
                (before + delay + 1_000..=before + delay + 5_000).contains(&retry_at(&h)),
                "{hint}"
            );
            assert!(h.ends().is_empty());
            exec = send_pending(&h, ordinal + 1).await;
        }
    }
}

#[tokio::test]
async fn capacity_with_an_elapsed_hint_retries_after_jitter_without_a_new_backoff() {
    for hint in [
        "; Retry-After: 0",
        "; expected to be available at 2020-01-01T00:00:00Z",
    ] {
        let h = clocked_hub();
        h.bind();
        let exec = h.running("work").await;
        let before = h.clock().now_ms();
        h.node
            .finish(&exec, failure(&format!("{ERROR}{hint}")))
            .await
            .unwrap();
        until("elapsed hint retry planned", || h.attempts().len() == 2).await;
        assert!(
            (before + 1_000..=h.clock().now_ms() + 5_000).contains(&retry_at(&h)),
            "{hint}"
        );
        assert!(h.ends().is_empty());
    }
}
