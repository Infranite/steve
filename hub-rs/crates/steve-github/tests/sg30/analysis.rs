use serde_json::{Value, json};
use steve_github::fake::{Fault, defaults};
use steve_modules::testkit::WS;
use steve_modules::{GatewayOut, Tier};

use super::fixture::{BOT, Env};

#[tokio::test]
async fn sg30_issue_analysis_preserves_report_lifecycle_provenance_and_deduplication() {
    let h = Env::new(true).await;
    h.patch(json!({"issue_triage":true}));
    let title = "T".repeat(130);
    let body = "B".repeat(6_100);
    let n = h.issue(&title, &body).await;
    let report = format!(" {} ", "R".repeat(8_100));
    h.env.gateway.script_out(
        "你是 Steve",
        GatewayOut {
            text: report,
            cost_usd: Some(0.75),
            model: "fixture-model".into(),
            ..Default::default()
        },
    );
    let p = h.payload("issue", "opened", n).await;
    assert_eq!(h.send("sg30-issue", "issues", &p).await, 202);
    let trace = h.analyzed("sg30-issue", "done").await;
    let item_id = trace["runs"][0]["item_id"].as_str().unwrap();
    let item = h.env.store().items().get(item_id).unwrap().unwrap();
    assert_eq!(
        (
            item.status.as_str(),
            item.phase.as_str(),
            item.outcome.as_deref()
        ),
        ("done", "closed", Some("completed"))
    );
    assert_eq!(item.bot_id.as_deref(), Some(BOT));
    assert_eq!(item.title.chars().count(), 80);
    let goal = h.env.store().items().goal(item_id).unwrap();
    assert!(goal.contains("Bot：Analysis bot · fixture-model"), "{goal}");
    assert!(goal.contains(&format!("https://github.com/{}/issues/{n}", defaults::REPO)));
    let props = h.scalar(
        "SELECT props FROM links WHERE subject_id=? AND predicate='github_analysis'",
        &[&item_id],
    );
    let props: Value = serde_json::from_str(props.as_str().unwrap()).unwrap();
    assert_eq!(
        props["subject"],
        json!({"kind":"issue_triage","repo_id":h.repo,"number":n})
    );
    assert_eq!(props["report"], "R".repeat(8_000));
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM links WHERE subject_id=? AND predicate='from'",
            &[&item_id]
        ),
        2
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM sessions WHERE item_id=?", &[&item_id]),
        0
    );
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM decisions WHERE item_id=?",
            &[&item_id]
        ),
        0
    );
    let cats: Vec<_> = h
        .env
        .store()
        .tags()
        .kinds(WS)
        .unwrap()
        .into_iter()
        .filter(|t| {
            h.env
                .store()
                .tags()
                .of_item(item_id)
                .unwrap()
                .contains(&t.id)
        })
        .map(|t| t.name)
        .collect();
    assert_eq!(cats, ["doc"]);
    let prompt = h.env.gateway.last_prompt().unwrap();
    assert!(prompt.contains(&format!("正文（最多 6000 字）：{}", "B".repeat(6_000))));
    assert!(!prompt.contains(&"B".repeat(6_001)));
    assert!(prompt.contains(steve_modules::outbound::UNTRUSTED_OPEN));
    assert_eq!(h.env.gateway.calls.lock().unwrap()[0].0, Tier::Fast);
    assert_eq!(
        h.scalar(
            "SELECT cost FROM flow_runs WHERE work_item_id=?",
            &[&item_id]
        ),
        0.75
    );
    assert_eq!(h.send("sg30-issue", "issues", &p).await, 200);
    assert_eq!(h.send("sg30-issue-duplicate", "issues", &p).await, 202);
    assert_eq!(h.trace("sg30-issue-duplicate")["reason"], "duplicate");
    assert_eq!(h.env.gateway.calls.lock().unwrap().len(), 1);
    assert_eq!(h.env.store().items().all(WS).unwrap().len(), 1);
    assert!(h.fake.calls().iter().all(|c| c.method == "GET"));
}

fn pr(h: &Env) -> (u64, String, String) {
    let base = "1".repeat(40);
    let head = "2".repeat(40);
    h.fake.set_branch(defaults::REPO, "main", &base);
    let n = h
        .fake
        .open_pr(
            defaults::REPO,
            "topic",
            &head,
            "Review this",
            "Evidence only",
            "guest",
        )
        .unwrap();
    (n, base, head)
}

#[tokio::test]
async fn sg30_pr_analysis_checks_both_live_reads_and_quotes_bounded_diff_evidence() {
    let h = Env::new(true).await;
    h.patch(json!({"pr_review":true}));
    let (n, base, head) = pr(&h);
    h.fake.set_compare(
        defaults::REPO,
        &base,
        &head,
        json!({"files":[
            {"filename":"sample.rs","status":"modified","patch":"+bounded()"},
            {"filename":"binary.dat","status":"added"}
        ]}),
    );
    h.env.gateway.script(
        "你是 Steve",
        "**Finding**\n![image](https://example.test/image)\n@owner",
    );
    let p = h.payload("pull_request", "opened", n).await;
    h.fake.clear_calls();
    assert_eq!(h.send("sg30-pr", "pull_request", &p).await, 202);
    let trace = h.analyzed("sg30-pr", "done").await;
    let item = trace["runs"][0]["item_id"].as_str().unwrap();
    let goal = h.env.store().items().goal(item).unwrap();
    assert!(goal.contains(&format!("base: {base}\nhead: {head}")));
    assert!(goal.contains("    **Finding**\n    ![image](https://example.test/image)\n    @owner"));
    let prompt = h.env.gateway.last_prompt().unwrap();
    assert!(prompt.contains("sample.rs (modified)\n+bounded()"));
    assert!(prompt.contains("[GitHub 未提供该文件的 patch]"));
    assert_eq!(h.env.gateway.calls.lock().unwrap()[0].0, Tier::Smart);
    assert_eq!(
        h.fake
            .calls_to("GET", &format!("/repos/{}/pulls/{n}", defaults::REPO))
            .len(),
        2
    );
    assert_eq!(
        h.fake
            .calls_to("GET", &format!("/repos/{}/compare/", defaults::REPO))
            .len(),
        1
    );
    assert!(h.fake.calls().iter().all(|c| c.method == "GET"));
}

#[tokio::test]
async fn sg30_changes_during_the_model_call_cannot_publish_a_stale_or_disabled_result() {
    for changed in ["head", "settings"] {
        let h = Env::new(true).await;
        h.patch(json!({"pr_review":true}));
        let (n, base, head) = pr(&h);
        h.fake
            .set_compare(defaults::REPO, &base, &head, json!({"files":[]}));
        h.env.gateway.script_out(
            "你是 Steve",
            GatewayOut {
                text: "Result".into(),
                cost_usd: Some(0.5),
                model: "fixture".into(),
                ..Default::default()
            },
        );
        if changed == "head" {
            let fake = h.fake.clone();
            *h.on_model.lock().unwrap() = Some(Box::new(move || {
                fake.push_to_pr(defaults::REPO, n, &"3".repeat(40))
            }));
        } else {
            let store = h.env.store().clone();
            let repo = h.repo.clone();
            *h.on_model.lock().unwrap() = Some(Box::new(move || {
                store
                    .transaction(|tx| -> steve_store::Result<_> {
                        tx.execute("UPDATE gh_repos SET enabled=0 WHERE id=?", [&repo])?;
                        Ok(())
                    })
                    .unwrap();
            }));
        }
        let p = h.payload("pull_request", "opened", n).await;
        h.send(&format!("sg30-change-{changed}"), "pull_request", &p)
            .await;
        let trace = h.analyzed(&format!("sg30-change-{changed}"), "done").await;
        assert!(trace["runs"][0]["item_id"].is_null(), "{trace}");
        assert_eq!(h.env.store().items().all(WS).unwrap().len(), 0);
        let label = trace["runs"][0]["label"].as_str().unwrap_or("");
        assert!(
            label.contains(if changed == "head" {
                "分析期间 PR 已更新或关闭"
            } else {
                "提交或仓库设置已变更"
            }),
            "{trace}"
        );
        assert_eq!(h.env.gateway.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn sg30_analysis_failures_keep_the_stage_and_never_write_a_partial_report() {
    for case in ["bot", "live", "diff", "empty", "model", "save"] {
        let h = Env::new(true).await;
        h.patch(json!({"pr_review":true}));
        let (n, base, head) = pr(&h);
        h.fake.set_compare(
            defaults::REPO,
            &base,
            &head,
            if case == "diff" {
                json!({})
            } else {
                json!({"files":[]})
            },
        );
        let p = h.payload("pull_request", "opened", n).await;
        h.env
            .gateway
            .script("你是 Steve", if case == "empty" { "  " } else { "Report" });
        let expected = match case {
            "bot" => {
                h.env
                    .store()
                    .transaction(|tx| -> steve_store::Result<_> {
                        tx.execute("UPDATE bots SET enabled=0 WHERE id=?", [BOT])?;
                        Ok(())
                    })
                    .unwrap();
                "选择 Bot：工作空间没有可用 Bot"
            }
            "live" => {
                h.fake.fail_next_on(
                    &format!("/repos/{}/pulls/{n}", defaults::REPO),
                    Fault::Status {
                        status: 422,
                        message: "live refused".into(),
                    },
                );
                "读取 GitHub 对象："
            }
            "diff" => "读取 PR 差异：GitHub 没有返回文件列表",
            "empty" => "Bot 分析：模型没有返回报告",
            "model" => {
                h.env.gateway.unscript("你是 Steve");
                h.env.gateway.script_fail("你是 Steve", "model refused");
                "Bot 分析：model refused"
            }
            "save" => {
                h.env.store().transaction(|tx| -> steve_store::Result<_> {
                    tx.execute_batch("CREATE TRIGGER sg30_refuse_report BEFORE INSERT ON items BEGIN SELECT RAISE(ABORT,'sg30 report refused'); END")?;Ok(())
                }).unwrap();
                "保存分析事项："
            }
            _ => unreachable!(),
        };
        let id = format!("sg30-fail-{case}");
        assert_eq!(h.send(&id, "pull_request", &p).await, 202);
        let trace = h.analyzed(&id, "failed").await;
        assert!(
            trace["runs"][0]["error"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{case}: {trace}"
        );
        assert!(h.env.store().items().all(WS).unwrap().is_empty());
        assert_eq!(
            h.scalar(
                "SELECT count(*) FROM links WHERE predicate='github_analysis'",
                &[]
            ),
            0
        );
        assert!(
            h.fake
                .calls_to("POST", &format!("/repos/{}/issues", defaults::REPO))
                .is_empty()
        );
    }
}

#[tokio::test]
async fn sg30_a_report_linked_while_the_model_runs_wins_without_replacing_its_content() {
    let h = Env::new(true).await;
    h.patch(json!({"issue_triage":true}));
    let n = h.issue("Race", "Details").await;
    let doc = h
        .env
        .store()
        .tags()
        .kinds(WS)
        .unwrap()
        .into_iter()
        .find(|t| t.name == "doc")
        .unwrap()
        .id;
    let prior = h
        .env
        .store()
        .items()
        .create(
            WS,
            &steve_store::NewItem {
                title: "Prior report".into(),
                goal: "Prior report content".into(),
                bot_id: BOT.into(),
                tags: vec![doc],
                status: Some("done".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    h.env
        .store()
        .transaction(|tx| -> steve_store::Result<_> {
            steve_store::lifecycle_write_in(
                tx,
                &prior.id,
                &steve_store::LifecycleWrite {
                    phase: "closed".into(),
                    cycle: prior.cycle,
                    revision: prior.revision,
                    outcome: Some("completed".into()),
                    ..Default::default()
                },
            )?;
            Ok(())
        })
        .unwrap();
    let store = h.env.store().clone();
    let item = prior.id.clone();
    let repo = h.repo.clone();
    *h.on_model.lock().unwrap() = Some(Box::new(move || {
        store.transaction(|tx| -> steve_store::Result<_> {
            let event:String=tx.query_row("SELECT event_id FROM flow_runs WHERE status='running' AND event_id IS NOT NULL ORDER BY started DESC LIMIT 1",[],|r|r.get(0))?;
            let mut link=steve_store::base::links::NewLink::new(
                steve_store::base::ObjRef::new("item",&item),"github_analysis",
                steve_store::base::ObjRef::new("event",&event),"github");
            link.props=json!({"subject":{"kind":"issue_triage","repo_id":repo,"number":n},"report":"Prior report content"});
            steve_store::base::links::add(tx,WS,&link)?;Ok(())
        }).unwrap();
    }));
    h.env
        .gateway
        .script("你是 Steve", "New result must not replace the prior report");
    let payload = h.payload("issue", "opened", n).await;
    h.send("sg30-report-race", "issues", &payload).await;
    let trace = h.analyzed("sg30-report-race", "done").await;
    assert_eq!(trace["runs"][0]["item_id"], prior.id);
    assert_eq!(h.env.store().items().all(WS).unwrap().len(), 1);
    assert_eq!(
        h.env.store().items().goal(&prior.id).unwrap(),
        "Prior report content"
    );
    assert_eq!(
        h.env.store().items().get(&prior.id).unwrap().unwrap().phase,
        "closed"
    );
}
