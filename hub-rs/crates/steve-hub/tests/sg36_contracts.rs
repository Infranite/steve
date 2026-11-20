//! Public coordinator paths pin the bookkeeping and recovery boundaries before extraction.
mod sg36;

use sg36::{Hub, SESSION};
use steve_hub::wire::node_runtime::{ConfigChange, CostCaps, Planned};
use steve_node_api::testkit::{Scripted, build};
use steve_node_api::*;
use steve_runtime::testkit::eventually_sync;

fn usage(
    id: &str,
    amount: &str,
    currency: &str,
    epoch: &str,
    measurement: Measurement,
    scope: UsageScope,
) -> EventBody {
    EventBody::Usage {
        report: UsageReport {
            report_id: id.into(),
            model: None,
            meter_epoch: Epoch::new(epoch).unwrap(),
            scope,
            measurement,
            tokens: Tokens::default(),
            context_used: None,
            context_limit: None,
            cost: Some(Money {
                decimal: amount.into(),
                currency: currency.into(),
            }),
        },
    }
}

#[tokio::test]
async fn cumulative_high_water_survives_refusals_and_epochs_while_attempt_caps_span_reports() {
    let h = Hub::new().await;
    h.rt.set_cost_caps(CostCaps::new(5.0, 6.0));
    h.bind();
    h.ready().await;
    let exec = h.turn().await;
    // A refused jump still advances the cumulative high water. A refused attempt-cap delta
    // does too; changing meter epoch resets that high water but never the attempt's budget.
    for (i, (amount, currency, epoch, measurement, scope, total)) in [
        (
            "1",
            " USD ",
            "p",
            Measurement::Cumulative,
            UsageScope::Main,
            1.0,
        ),
        (
            "NaN",
            "USD",
            "p",
            Measurement::Cumulative,
            UsageScope::Main,
            1.0,
        ),
        (
            "2",
            "EUR",
            "p",
            Measurement::Cumulative,
            UsageScope::Main,
            1.0,
        ),
        (
            "100",
            "USD",
            "p",
            Measurement::Cumulative,
            UsageScope::Background,
            1.0,
        ),
        (
            "0.5",
            "USD",
            "p",
            Measurement::Cumulative,
            UsageScope::Main,
            1.0,
        ),
        (
            "11",
            "USD",
            "p",
            Measurement::Cumulative,
            UsageScope::Main,
            1.0,
        ),
        (
            "13",
            "USD",
            "p",
            Measurement::Cumulative,
            UsageScope::Main,
            3.0,
        ),
        (
            "16.5",
            "USD",
            "p",
            Measurement::Cumulative,
            UsageScope::Main,
            3.0,
        ),
        (
            "17",
            "USD",
            "p",
            Measurement::Cumulative,
            UsageScope::Main,
            3.5,
        ),
        (
            "1",
            "usd",
            "q",
            Measurement::Cumulative,
            UsageScope::Main,
            4.5,
        ),
        ("1.5", "USD", "q", Measurement::Delta, UsageScope::Main, 6.0),
        (
            "0.01",
            "USD",
            "q",
            Measurement::Final,
            UsageScope::Main,
            6.0,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        h.emit(
            &exec,
            usage(
                &format!("u{i}"),
                amount,
                currency,
                epoch,
                measurement,
                scope,
            ),
        )
        .await;
        assert_eq!(h.q::<f64>("SELECT cost FROM sessions"), total, "report {i}");
    }
    h.finish(&exec).await;
    assert_eq!(h.domain.facts.lock().unwrap()[0].cost, Some(6.0));
}

fn message(role: Role) -> EventBody {
    EventBody::MessageStarted {
        message: "m".into(),
        block: "b".into(),
        role,
    }
}
fn text(offset: u64, value: &str) -> EventBody {
    EventBody::TextDelta {
        message: "m".into(),
        block: "b".into(),
        utf8_offset: offset,
        text: value.into(),
    }
}

#[tokio::test]
async fn realtime_counts_only_new_assistant_text_and_marks_missing_or_invalid_evidence() {
    for (role, deltas, expected) in [
        (
            Some(Role::Assistant),
            vec![(0, "abc"), (1, "bcde"), (0, "abcde")],
            vec![("text", 3), ("text", 2), ("activity", 0)],
        ),
        (
            Some(Role::Thought),
            vec![(0, "好"), (1, "好")],
            // Non-ASCII scalars count four quarter-token units.
            vec![("text", 4), ("gap", 0)],
        ),
        (Some(Role::User), vec![(0, "abc")], vec![("activity", 0)]),
        (Some(Role::Assistant), vec![(2, "abc")], vec![("gap", 0)]),
        (None, vec![(0, "abc")], vec![("gap", 0)]),
        (
            Some(Role::Assistant),
            vec![(u64::MAX, "abc")],
            vec![("gap", 0)],
        ),
    ] {
        let h = Hub::new().await;
        h.bind();
        h.ready().await;
        let exec = h.turn().await;
        if let Some(role) = role {
            h.emit(&exec, message(role)).await;
        }
        for ((offset, value), (kind, units)) in deltas.into_iter().zip(expected) {
            h.emit(&exec, text(offset, value)).await;
            assert_eq!(
                h.q::<String>(
                    "SELECT kind FROM realtime_output_samples ORDER BY rowid DESC LIMIT 1"
                ),
                kind
            );
            assert_eq!(
                h.q::<i64>("SELECT units FROM realtime_output_samples ORDER BY rowid DESC LIMIT 1"),
                units
            );
        }
        h.finish(&exec).await;
        assert_eq!(
            h.q::<i64>("SELECT ended FROM realtime_output_attempts"),
            h.at
        );
        assert_eq!(
            h.q::<String>("SELECT kind FROM realtime_output_samples ORDER BY rowid DESC LIMIT 1"),
            "end"
        );
    }
}

#[tokio::test]
async fn realtime_bad_clock_uses_receipt_time_and_final_tokens_without_text_are_a_gap() {
    let h = Hub::new().await;
    h.bind();
    h.ready().await;
    let exec = h.turn().await;
    h.emit(&exec, message(Role::Assistant)).await;
    h.node.set_now(0);
    h.emit(&exec, text(0, "untrusted time")).await;
    assert_eq!(
        h.q::<i64>("SELECT at FROM realtime_output_samples ORDER BY rowid DESC LIMIT 1"),
        h.at
    );
    assert_eq!(
        h.q::<i64>("SELECT source_at FROM realtime_output_samples ORDER BY rowid DESC LIMIT 1"),
        0
    );
    assert_eq!(
        h.q::<String>("SELECT kind FROM realtime_output_samples ORDER BY rowid DESC LIMIT 1"),
        "gap"
    );
    h.node.set_now(h.at);
    let EventBody::Usage { mut report } = usage(
        "final",
        "0",
        "USD",
        "p",
        Measurement::Final,
        UsageScope::Main,
    ) else {
        unreachable!()
    };
    report.tokens.output = Some(9);
    h.emit(&exec, EventBody::Usage { report }).await;
    assert_eq!(
        h.q::<String>("SELECT kind FROM realtime_output_samples ORDER BY rowid DESC LIMIT 1"),
        "gap"
    );
    h.node.set_now(h.at + 2_000);
    h.finish(&exec).await;
    assert_eq!(
        h.q::<i64>("SELECT ended FROM realtime_output_attempts"),
        h.at
    );
    assert_eq!(
        h.q::<String>("SELECT kind FROM realtime_output_samples ORDER BY rowid DESC LIMIT 1"),
        "gap"
    );
}

#[tokio::test]
async fn stop_retires_each_unknown_session_control_once_and_replans_its_dependent_work() {
    for method in [
        "prepare_workspace",
        "open_session",
        "release_session",
        "configure_session",
    ] {
        let h = Hub::new().await;
        if matches!(method, "prepare_workspace" | "open_session") {
            h.node.script_once(method, Scripted::Unknown);
            h.bind();
        } else {
            h.bind();
            h.ready().await;
            h.node.script_once(method, Scripted::Unknown);
            if method == "release_session" {
                h.rt.release(SESSION).unwrap();
            } else {
                h.rt.configure(
                    SESSION,
                    &ConfigChange {
                        permissions: Some(PermissionMode::FullAccess),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
        }
        eventually_sync(
            "the scripted control is unknown in the durable ledger",
            || h.state(method) == "unknown",
        )
        .await;
        let target = h
            .node
            .log()
            .into_iter()
            .find(|(m, _)| *m == method)
            .unwrap()
            .1;
        h.rt.prompt(SESSION, "after recovery", "user", &[]).unwrap();
        // On this current-thread runtime no async dispatch runs between these two Stop calls.
        assert!(h.rt.user_stop(SESSION).unwrap());
        assert!(h.rt.user_stop(SESSION).unwrap());
        assert_eq!(
            h.q::<i64>("SELECT COUNT(*) FROM node_commands WHERE method='recover'"),
            1
        );
        assert_eq!(h.q::<String>("SELECT json_extract(request,'$.body.target') FROM node_commands WHERE method='recover'"), target.as_str());
        eventually_sync(
            "recovery opened the waiting turn under a new control id",
            || h.count("start_turn") == 1,
        )
        .await;
        assert_eq!(h.count("recover"), 1);
        assert_eq!(
            h.node.log().iter().filter(|(_, op)| *op == target).count(),
            1
        );
        assert_eq!(
            h.count("prepare_workspace"),
            if method == "prepare_workspace" { 2 } else { 1 }
        );
        if method == "prepare_workspace" {
            assert_eq!(h.q::<String>("SELECT state FROM node_commands WHERE method='open_session' ORDER BY rowid LIMIT 1"), "rejected");
        }
        if method == "configure_session" {
            assert_eq!(h.q::<String>("SELECT json_extract(request,'$.body.policy.permissions') FROM node_commands WHERE method='open_session' ORDER BY rowid DESC LIMIT 1"), "full_access");
        }
    }
}

#[tokio::test]
async fn reconcile_holds_stranger_work_once_and_lifts_the_hold_when_the_node_forgets_it() {
    let h = Hub::new().await;
    h.bind();
    h.ready().await;
    let op = OperationId::new("stranger-op").unwrap();
    let mut command = build::cmd(
        op.as_str(),
        build::turn_on(
            "sg36-node",
            SESSION,
            "stranger-turn",
            "stranger-attempt",
            "outside the Hub",
        ),
    );
    command.body.run.limits.deadline = Timestamp(h.at + 600_000);
    command.body.run.limits.offline_until = Timestamp(h.at + 600_000);
    command.issued_at = Timestamp(h.at);
    command.accept_before = Timestamp(h.at + 60_000);
    h.node.start_turn(command).await.unwrap();
    assert_eq!(h.rt.reconcile_again().await.unwrap(), 1);
    assert_eq!(
        h.rt.prompt(SESSION, "queued", "user", &[]).unwrap(),
        Planned::Held
    );
    assert_eq!(
        h.rt.reconcile_again().await.unwrap(),
        0,
        "an existing hold adds no second note"
    );
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM events WHERE kind='system'"),
        1
    );
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM node_commands WHERE operation_id='stranger-op'"),
        0
    );
    assert!(!h.node.receipt(&op).unwrap().state.is_finished());
    h.node.restart();
    assert_eq!(
        h.rt.reconcile().await.unwrap(),
        1,
        "the idle session was released"
    );
    assert!(
        h.rt.is_uncertain(SESSION),
        "a restart keeps the stranger as unknown work"
    );
    assert_eq!(h.phase(), "released");
    h.node.forget(&op);
    assert_eq!(h.rt.reconcile().await.unwrap(), 0);
    assert!(
        !h.rt.is_uncertain(SESSION),
        "an empty snapshot replaces the previous hold set"
    );
}

#[tokio::test]
async fn reconcile_expires_an_orphaned_permission_and_marks_the_missing_command_unknown() {
    let h = Hub::new().await;
    h.bind();
    h.ready().await;
    let exec = h.turn().await;
    let mut request = build::permission("orphan-card", exec, "inspect work");
    request.expires_at = Timestamp(h.at + 600_000);
    h.node.ask_permission(request).await.unwrap();
    eventually_sync("the Node permission has an open owner card", || {
        h.q::<i64>("SELECT COUNT(*) FROM decisions WHERE status='open'") == 1
    })
    .await;
    h.node.reset_journal();
    assert_eq!(
        h.rt.reconcile().await.unwrap(),
        2,
        "the unknown command and orphaned card each changed"
    );
    assert_eq!(h.state("start_turn"), "unknown");
    assert_eq!(h.q::<String>("SELECT status FROM decisions"), "expired");
    assert_eq!(
        h.q::<String>("SELECT state FROM runtime_permissions"),
        "expired"
    );
    assert_eq!(
        h.rt.reconcile().await.unwrap(),
        0,
        "repeating the same snapshot is idempotent"
    );
}
