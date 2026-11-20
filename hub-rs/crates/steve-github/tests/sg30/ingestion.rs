use super::fixture::Env;
use serde_json::{Value, json};
use steve_github::fake::{Fault, defaults};
use steve_github::{
    repo::{self, DeliveryRow},
    sync::Source,
};
use steve_modules::Clock;
use steve_modules::testkit::{ME, WS, caller};

fn seed(h: &Env, id: &str, source: &str, payload: &str, next: i64) {
    h.env
        .store()
        .transaction(|tx| -> steve_store::Result<_> {
            repo::insert_delivery(
                tx,
                &DeliveryRow {
                    delivery_id: id.into(),
                    workspace_id: Some(WS.into()),
                    repo_id: Some(h.repo.clone()),
                    event: "ping".into(),
                    action: None,
                    status: repo::RECEIVED.into(),
                    error: None,
                    received: h.env.clock.now(),
                },
                source,
                payload,
            )?;
            tx.execute(
                "UPDATE gh_delivery_inputs SET next_at=? WHERE delivery_id=?",
                rusqlite::params![next, id],
            )?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn sg30_recovery_claims_once_skips_future_work_and_marks_unreadable_inputs_failed() {
    let h = Env::new(true).await;
    let source = serde_json::to_string(&Source::Conn {
        ws: WS.into(),
        connection_id: h.connection.clone(),
    })
    .unwrap();
    seed(&h, "good-a", &source, "{}", 0);
    seed(&h, "good-b", &source, "{}", 0);
    seed(&h, "bad-source", "{", "{}", 0);
    seed(&h, "bad-payload", &source, "{", 0);
    seed(&h, "future", &source, "{}", h.env.clock.now() + 10_000);
    assert_eq!(h.gh.recover_deliveries(), 2);
    assert_eq!(
        h.gh.recover_deliveries(),
        0,
        "claimed before workers are polled"
    );
    h.gh.settle().await;
    for id in ["good-a", "good-b"] {
        assert_ne!(
            h.scalar(
                "SELECT status FROM gh_deliveries WHERE delivery_id=?",
                &[&id]
            ),
            "received"
        );
        assert_eq!(
            h.scalar(
                "SELECT count(*) FROM gh_delivery_inputs WHERE delivery_id=?",
                &[&id]
            ),
            0
        );
    }
    for id in ["bad-source", "bad-payload"] {
        assert_eq!(
            h.scalar(
                "SELECT status FROM gh_deliveries WHERE delivery_id=?",
                &[&id]
            ),
            "failed"
        );
        assert_eq!(
            h.scalar(
                "SELECT error FROM gh_deliveries WHERE delivery_id=?",
                &[&id]
            ),
            "存下的内容读不出来"
        );
    }
    assert_eq!(
        h.scalar(
            "SELECT status FROM gh_deliveries WHERE delivery_id='future'",
            &[]
        ),
        "received"
    );
    h.env.clock.advance(10_000);
    assert_eq!(h.gh.recover_deliveries(), 1);
    h.gh.settle().await;
    assert_eq!(h.gh.recover_deliveries(), 0);
}

#[tokio::test]
async fn sg30_recovery_reader_failure_does_not_claim_or_change_a_delivery() {
    let h = Env::new(true).await;
    let source = serde_json::to_string(&Source::Conn {
        ws: WS.into(),
        connection_id: h.connection.clone(),
    })
    .unwrap();
    seed(&h, "decode-row", &source, "{}", 0);
    h.env
        .store()
        .transaction(|tx| -> steve_store::Result<_> {
            tx.execute(
                "UPDATE gh_delivery_inputs SET source=? WHERE delivery_id='decode-row'",
                [vec![0xffu8]],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(h.gh.recover_deliveries(), 0);
    assert_eq!(
        h.scalar(
            "SELECT status FROM gh_deliveries WHERE delivery_id='decode-row'",
            &[]
        ),
        "received"
    );
    h.env
        .store()
        .transaction(|tx| -> steve_store::Result<_> {
            tx.execute(
                "UPDATE gh_delivery_inputs SET source=? WHERE delivery_id='decode-row'",
                [source],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(h.gh.recover_deliveries(), 1);
    h.gh.settle().await;
    assert_eq!(h.gh.recover_deliveries(), 0);
}

#[tokio::test]
async fn sg30_failed_processing_retries_only_when_due_and_stops_at_the_original_budget() {
    let h = Env::new(true).await;
    seed(
        &h,
        "retry",
        &serde_json::to_string(&Source::Poll {
            repo_id: h.repo.clone(),
        })
        .unwrap(),
        "{}",
        0,
    );
    h.env
        .store()
        .transaction(|tx| -> steve_store::Result<_> {
            tx.execute(
                "UPDATE gh_repos SET name=? WHERE id=?",
                rusqlite::params![vec![0xffu8], h.repo],
            )?;
            Ok(())
        })
        .unwrap();
    for attempt in 1..=steve_store::outbox::MAX_ATTEMPTS {
        assert_eq!(h.gh.recover_deliveries(), 1, "attempt {attempt}");
        h.gh.settle().await;
        assert_eq!(h.gh.recover_deliveries(), 0);
        if attempt < steve_store::outbox::MAX_ATTEMPTS {
            assert_eq!(
                h.scalar(
                    "SELECT status FROM gh_deliveries WHERE delivery_id='retry'",
                    &[]
                ),
                "received"
            );
            assert_eq!(
                h.scalar(
                    "SELECT attempts FROM gh_delivery_inputs WHERE delivery_id='retry'",
                    &[]
                ),
                attempt
            );
            h.env.clock.advance(steve_store::outbox::BACKOFF_MAX_MS + 1);
        }
    }
    assert_eq!(
        h.scalar(
            "SELECT status FROM gh_deliveries WHERE delivery_id='retry'",
            &[]
        ),
        "failed"
    );
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM gh_delivery_inputs WHERE delivery_id='retry'",
            &[]
        ),
        1
    );
}

#[tokio::test]
async fn sg30_poll_checks_each_unsettled_sha_once_skips_pr_shaped_issues_and_preserves_concurrent_state()
 {
    let h = Env::new(true).await;
    let pending = "a".repeat(40);
    let passed = "b".repeat(40);
    h.fake
        .open_pr(defaults::REPO, "one", &pending, "One", "Body", "guest")
        .unwrap();
    h.fake
        .open_pr(defaults::REPO, "two", &pending, "Two", "Body", "guest")
        .unwrap();
    h.fake
        .open_pr(defaults::REPO, "three", &passed, "Three", "Body", "guest")
        .unwrap();
    h.fake.set_check(defaults::REPO, &pending, "pending", None);
    h.fake
        .set_status(defaults::REPO, &pending, "legacy", "pending", "");
    h.fake
        .set_check(defaults::REPO, &passed, "passed", Some("success"));
    h.fake
        .set_status(defaults::REPO, &passed, "legacy", "success", "");
    h.issue("Issue, not a PR", "Details").await;
    h.env.store().transaction(|tx| -> steve_store::Result<_> {
        tx.execute_batch("CREATE TRIGGER sg30_poll_state AFTER INSERT ON gh_deliveries WHEN NEW.delivery_id LIKE 'poll:%' BEGIN UPDATE gh_repos SET poll_state=json_set(poll_state,'$.concurrent',true) WHERE id=NEW.repo_id; END")?;Ok(())
    }).unwrap();
    h.fake.clear_calls();
    let first = h.gh.poll_repo(&h.repo).await.unwrap();
    h.gh.settle().await;
    assert_eq!(first.applied, 6, "{first:?}");
    assert_eq!(h.scalar("SELECT count(*) FROM gh_issues", &[]), 1);
    assert_eq!(h.scalar("SELECT count(*) FROM gh_prs", &[]), 3);
    let state: Value = serde_json::from_str(
        h.scalar("SELECT poll_state FROM gh_repos WHERE id=?", &[&h.repo])
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(state["concurrent"], 1);
    assert_eq!(state["last_poll"], h.env.clock.now());
    assert!(state["pulls_since"].is_number());
    assert!(state["issues_since"].is_number());
    assert!(state["pulls_scan"].is_null());
    h.fake.clear_calls();
    let second = h.gh.poll_repo(&h.repo).await.unwrap();
    assert!(second.pulls_unchanged);
    assert_eq!((second.applied, second.seen), (0, 1));
    assert_eq!(
        h.fake
            .calls_to(
                "GET",
                &format!("/repos/{}/commits/{pending}/check-runs", defaults::REPO)
            )
            .len(),
        1
    );
    assert!(
        h.fake
            .calls_to(
                "GET",
                &format!("/repos/{}/commits/{passed}", defaults::REPO)
            )
            .is_empty()
    );
    assert!(
        h.fake
            .calls_to("GET", &format!("/repos/{}/issues", defaults::REPO))
            .is_empty()
    );
    h.env.clock.advance(steve_github::poll::ISSUES_EVERY_MS);
    let third = h.gh.poll_repo(&h.repo).await.unwrap();
    assert_eq!(third.applied, 0);
    assert!(
        h.fake
            .calls_to("GET", &format!("/repos/{}/issues", defaults::REPO))
            .iter()
            .any(|c| c.query.contains("since="))
    );
}

#[tokio::test]
async fn sg30_poll_failure_preserves_the_cursor_and_replays_only_durable_synthetic_ids() {
    let h = Env::new(true).await;
    h.fake
        .open_pr(
            defaults::REPO,
            "topic",
            &"c".repeat(40),
            "PR",
            "Body",
            "guest",
        )
        .unwrap();
    h.issue("Issue", "Details").await;
    let before = h.scalar("SELECT poll_state FROM gh_repos WHERE id=?", &[&h.repo]);
    h.fake.fail_next_on(
        "/issues",
        Fault::Status {
            status: 422,
            message: "issues refused".into(),
        },
    );
    assert!(
        h.gh.poll_repo(&h.repo)
            .await
            .unwrap_err()
            .contains("issues refused")
    );
    assert_eq!(
        h.scalar("SELECT poll_state FROM gh_repos WHERE id=?", &[&h.repo]),
        before
    );
    assert_eq!(h.scalar("SELECT count(*) FROM gh_prs", &[]), 1);
    let retried = h.gh.poll_repo(&h.repo).await.unwrap();
    assert!(retried.seen >= 1);
    assert_eq!(h.scalar("SELECT count(*) FROM gh_prs", &[]), 1);
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM gh_deliveries WHERE event='pull_request'",
            &[]
        ),
        1
    );
    let state: Value = serde_json::from_str(
        h.scalar("SELECT poll_state FROM gh_repos WHERE id=?", &[&h.repo])
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(state["last_poll"].is_number());
    h.gh.patch_repo(&caller(WS, ME), &h.repo, &json!({"enabled":false}))
        .unwrap();
    h.fake.clear_calls();
    assert_eq!(h.gh.poll_repo(&h.repo).await.unwrap(), Default::default());
    assert!(h.fake.calls().is_empty());
    assert_eq!(
        h.gh.poll_repo("missing").await.unwrap_err(),
        "仓库已经取消关联"
    );
}
