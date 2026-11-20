use super::fixture::{Env, PROJECT};
use serde_json::{Value, json};
use steve_github::fake::defaults;
use steve_modules::testkit::{ME, WS, WS2, caller};

#[tokio::test]
async fn sg30_link_validates_ownership_project_and_duplicates_before_remote_writes() {
    let h = Env::new(false).await;
    for (who, project) in [
        (caller(WS2, ME), None),
        (caller(WS, ME), Some("missing-project")),
    ] {
        assert!(
            h.gh.link_repo(&who, &h.connection, defaults::REPO_ID as i64, project)
                .await
                .is_err()
        );
        assert!(h.fake.calls().is_empty());
        assert!(h.fake.hooks(defaults::REPO).is_empty());
    }
    let row = h.link(defaults::REPO_ID as i64, Some(PROJECT)).await;
    assert_eq!(row["ingest"], "repo_hook");
    assert_eq!(row["project"]["id"], PROJECT);
    assert_eq!(row["revision"], 1);
    assert_eq!(row["settings"]["issue_triage"], false);
    assert_eq!(h.fake.hooks(defaults::REPO).len(), 1);
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM audit WHERE action='github.repo_link'",
            &[]
        ),
        1
    );
    h.fake.clear_calls();
    assert!(
        h.gh.link_repo(
            &caller(WS, ME),
            &h.connection,
            defaults::REPO_ID as i64,
            None
        )
        .await
        .is_err()
    );
    assert!(h.fake.calls().is_empty());
}

#[tokio::test]
async fn sg30_link_refreshes_a_missing_cached_repo_and_falls_back_to_poll_without_a_hook() {
    let h = Env::new(false).await;
    h.gh.available(WS, &h.connection, false).await.unwrap();
    let new = h.fake.add_repo("steve-test/new-repo", false, "main");
    h.fake.set_hooks_allowed("steve-test/new-repo", false);
    h.fake.clear_calls();
    let row = h.link(new as i64, None).await;
    assert_eq!(row["full_name"], "steve-test/new-repo");
    assert_eq!(row["ingest"], "poll");
    assert_eq!(row["project"], Value::Null);
    assert_eq!(h.fake.calls_to("GET", "/user/repos").len(), 1);
    assert!(h.fake.hooks("steve-test/new-repo").is_empty());
    assert_eq!(h.scalar("SELECT count(*) FROM vault_items WHERE title LIKE 'GitHub Webhook %' AND status='active'",&[]),0);
    assert_eq!(h.scalar("SELECT count(*) FROM gh_repos", &[]), 1);
}

#[tokio::test]
async fn sg30_link_database_refusal_cleans_up_the_created_hook_before_returning_the_error() {
    let h = Env::new(false).await;
    h.env.store().transaction(|tx| -> steve_store::Result<_> {
        tx.execute_batch("CREATE TRIGGER sg30_refuse_link BEFORE INSERT ON gh_repos BEGIN SELECT RAISE(ABORT,'sg30 link refused'); END")?;Ok(())
    }).unwrap();
    let error =
        h.gh.link_repo(
            &caller(WS, ME),
            &h.connection,
            defaults::REPO_ID as i64,
            Some(PROJECT),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("sg30 link refused"), "{error}");
    assert_eq!(h.scalar("SELECT count(*) FROM gh_repos", &[]), 0);
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM audit WHERE action='github.repo_link'",
            &[]
        ),
        0
    );
    assert_eq!(h.scalar("SELECT count(*) FROM vault_items WHERE title LIKE 'GitHub Webhook %' AND status='active'",&[]),0);
    assert!(h.fake.hooks(defaults::REPO).is_empty());
    let calls = h.fake.calls();
    let methods: Vec<_> = calls
        .iter()
        .filter(|c| c.path.contains("/hooks"))
        .map(|c| c.method.as_str())
        .collect();
    assert_eq!(methods, ["POST", "DELETE"]);
    assert_eq!(
        h.gh.overview(&caller(WS, ME)).await.unwrap()["repos"],
        json!([])
    );
}
