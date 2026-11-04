//! An approved release decision records what became of it, and the item never reads as done while the release
//! has not landed: one test per way it can go wrong (ST-11, 2026-11-01: a rebase conflict left the card at
//! "approved" while nothing was released).

use crate::git::tests::{branch_with, g, repo};
use crate::patch;
use crate::repo::{Release, WS};
use crate::testkit::Kit;
use serde_json::{Value, json};

/// A project whose release script waits for `$STEVE_HOME/go`, then copies `$STEVE_HOME/result.json` (if any)
/// to `releases/last.json` and exits — like ops/release.sh for a web-only release or a failure before restart.
fn project(k: &Kit, path: &std::path::Path) -> String {
    let script = k.home().join("release.sh");
    std::fs::write(
        &script,
        super::fake_release_script("mkdir -p \"$STEVE_HOME/releases\"\n[ -f \"$STEVE_HOME/result.json\" ] && cp \"$STEVE_HOME/result.json\" \"$STEVE_HOME/releases/last.json\"\nexit 0\n"),
    )
    .unwrap();
    let rel = Release {
        kind: "script".into(),
        cmd: script.to_string_lossy().into(),
    };
    k.db()
        .project_create(
            WS,
            "R",
            "",
            &path.to_string_lossy(),
            "git",
            true,
            None,
            Some(&rel),
        )
        .unwrap()
        .id
}

/// An item waiting on a release approval for `branch` (cwd as given, or none so the rebase is skipped).
fn asking(k: &Kit, p: &str, branch: &str, cwd: Option<&str>) -> (String, String, String) {
    let i = k.item("wait", "收尾", Some(p));
    k.db()
        .item_patch(&i, patch![branch: branch, cwd: cwd, now: "等你批准发布"])
        .unwrap();
    let s = k.session(&i, "idle");
    let d = release_decision(k, &i, &s, branch);
    (i, s, d)
}

fn release_decision(k: &Kit, i: &str, s: &str, branch: &str) -> String {
    let d = k.decision(i, Some(s), "approve", "release", &["yes", "no"]);
    k.db()
        .exec(
            "UPDATE decisions SET subject_id = ? WHERE id = ?",
            &[&branch, &d],
        )
        .unwrap();
    d
}

fn result(k: &Kit, d: &str) -> Value {
    k.db().decision(d).unwrap().unwrap().result
}

fn fail_notices(k: &Kit) -> Vec<String> {
    k.core
        .notifications(WS)
        .unwrap()
        .into_iter()
        .filter(|n| n.level == "fail")
        .map(|n| n.text)
        .collect()
}

#[test]
fn a_rebase_conflict_records_not_released_and_keeps_the_item_open() {
    let k = Kit::new();
    let r = repo(k.home());
    let p = project(&k, &r);
    let i0 = k.item("wait", "收尾", Some(&p));
    let (name, wt) = branch_with(&r, &k.core.config().worktrees, &i0, "a.txt", "mine\n");
    std::fs::write(r.join("a.txt"), "theirs\n").unwrap();
    g(&r, &["commit", "-qam", "main a"]);
    k.db()
        .item_patch(
            &i0,
            patch![cwd: wt.to_string_lossy().to_string(), branch: name.as_str()],
        )
        .unwrap();
    let s = k.session(&i0, "idle");
    let d = release_decision(&k, &i0, &s, &name);
    let back = k.core.resolve(&d, "yes", "", "me").unwrap();
    /* the answer the web gets back already says it was not released */
    assert_eq!(back.result["outcome"], "not_released");
    assert_eq!(
        result(&k, &d),
        json!({"approved": true, "text": "", "outcome": "not_released", "reason": "rebase 到 main 有冲突（a.txt）"})
    );
    let (status, stage, now) = k.status(&i0);
    assert_eq!(
        (status.as_str(), stage.as_str(), now.as_str()),
        ("run", "收尾", "要先 rebase 解决冲突")
    );
    /* 批准发布 first, then why it did not happen */
    let tl = k.timeline_texts(&i0);
    let n = tl.len();
    assert_eq!(tl[n - 2], format!("批准发布 {name}"));
    assert!(tl[n - 1].starts_with("rebase 到 main 有冲突"));
    assert_eq!(
        fail_notices(&k),
        vec!["批准了发布，但没有发布：rebase 到 main 有冲突（a.txt）；已交回给它处理"]
    );
    assert!(k.core.tx().st.releasing.is_none());

    /* the agent fixes it and asks again: the second approval gets its own outcome, the first stays */
    g(&wt, &["rebase", "-X", "theirs", "main"]);
    let d2 = release_decision(&k, &i0, &s, &name);
    k.core.resolve(&d2, "yes", "", "me").unwrap();
    assert_eq!(result(&k, &d2)["outcome"], "releasing");
    assert_eq!(result(&k, &d)["outcome"], "not_released");
    assert_eq!(k.status(&i0).2, "正在发布");
    std::fs::write(
        k.home().join("result.json"),
        json!({"ok": true, "kind": "web", "version": "v9", "item": i0}).to_string(),
    )
    .unwrap();
    std::fs::write(k.home().join("go"), "").unwrap();
    assert!(k.wait(|| k.status(&i0).0 == "done"));
    assert_eq!(result(&k, &d2)["outcome"], "released");
    assert_eq!(result(&k, &d2)["version"], "v9");
    assert_eq!(result(&k, &d)["outcome"], "not_released");
}

#[test]
fn a_dirty_worktree_records_not_released() {
    let k = Kit::new();
    let r = repo(k.home());
    let p = project(&k, &r);
    let i = k.item("wait", "收尾", Some(&p));
    let (name, wt) = branch_with(&r, &k.core.config().worktrees, &i, "b.txt", "b\n");
    std::fs::write(wt.join("b.txt"), "changed\n").unwrap();
    k.db()
        .item_patch(
            &i,
            patch![cwd: wt.to_string_lossy().to_string(), branch: name.as_str()],
        )
        .unwrap();
    let s = k.session(&i, "idle");
    let d = release_decision(&k, &i, &s, &name);
    k.core.resolve(&d, "yes", "", "me").unwrap();
    let res = result(&k, &d);
    assert_eq!(res["outcome"], "not_released");
    assert!(
        res["reason"]
            .as_str()
            .unwrap()
            .starts_with("工作区有未提交的改动（")
    );
    let (status, _, now) = k.status(&i);
    assert_eq!(
        (status.as_str(), now.as_str()),
        ("run", "发布前要先处理分支")
    );
}

#[test]
fn a_failing_rebase_records_not_released() {
    let k = Kit::new();
    let p = project(&k, k.home());
    /* cwd exists but is no git worktree: the rebase errors */
    let plain = k.home().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let (i, _, d) = asking(&k, &p, "steve/x", Some(&plain.to_string_lossy()));
    k.core.resolve(&d, "yes", "", "me").unwrap();
    let res = result(&k, &d);
    assert_eq!(res["outcome"], "not_released");
    assert!(
        res["reason"]
            .as_str()
            .unwrap()
            .starts_with("rebase 到 main 失败（")
    );
    assert_ne!(k.status(&i).0, "done");
    assert_eq!(k.status(&i).2, "发布前要先处理分支");
}

#[test]
fn a_missing_script_records_not_released_with_a_fail_card() {
    let k = Kit::new();
    let rel = Release {
        kind: "script".into(),
        cmd: "/nonexistent/release.sh".into(),
    };
    let p = k
        .db()
        .project_create(
            WS,
            "R",
            "",
            &k.home().to_string_lossy(),
            "dir",
            false,
            None,
            Some(&rel),
        )
        .unwrap()
        .id;
    let (i, _, d) = asking(&k, &p, "steve/x", None);
    k.core.resolve(&d, "yes", "", "me").unwrap();
    assert_eq!(
        result(&k, &d),
        json!({"approved": true, "text": "", "outcome": "not_released", "reason": "发布脚本不存在（/nonexistent/release.sh）"})
    );
    let (status, stage, now) = k.status(&i);
    assert_eq!(
        (status.as_str(), stage.as_str(), now.as_str()),
        ("wait", "收尾", "没有发布")
    );
    assert_eq!(k.open_kinds(&i), vec!["fail"]);
}

#[test]
fn a_release_while_another_runs_is_not_released_and_a_failed_script_reopens() {
    let k = Kit::new();
    let p = project(&k, k.home());
    let (a, _, da) = asking(&k, &p, "steve/a", None);
    k.core.resolve(&da, "yes", "", "me").unwrap();
    assert_eq!(result(&k, &da)["outcome"], "releasing");
    assert!(
        result(&k, &da)["log"]
            .as_str()
            .unwrap()
            .contains("release-")
    );
    let (status, _, now) = k.status(&a);
    assert_eq!((status.as_str(), now.as_str()), ("run", "正在发布"));

    let (b, _, db) = asking(&k, &p, "steve/b", None);
    k.core.resolve(&db, "yes", "", "me").unwrap();
    assert_eq!(result(&k, &db)["outcome"], "not_released");
    assert_eq!(
        result(&k, &db)["reason"],
        format!("另一个发布（{a}）正在进行")
    );
    assert_eq!(k.status(&b).2, "没有发布");
    assert_eq!(
        result(&k, &da)["outcome"],
        "releasing",
        "the running one is untouched"
    );

    /* the build fails before any restart: last.json says so and the script exits */
    std::fs::write(
        k.home().join("result.json"),
        json!({"ok": false, "kind": "hub", "version": "v2", "prev": "v1", "item": a, "error": "构建失败"}).to_string(),
    )
    .unwrap();
    std::fs::write(k.home().join("go"), "").unwrap();
    // the outcome is written first, the item and its card right after (one core transaction, read without it)
    assert!(k.wait(|| result(&k, &da)["outcome"] == "failed" && k.status(&a).0 == "wait"));
    let res = result(&k, &da);
    assert_eq!(res["reason"], "构建失败");
    assert_eq!(res["note"], "已回到 v1");
    assert_eq!(res["version"], "v2");
    let (status, stage, now) = k.status(&a);
    assert_eq!(
        (status.as_str(), stage.as_str(), now.as_str()),
        ("wait", "收尾", "发布失败")
    );
    assert_eq!(k.open_fails(&a), vec!["发布 v2 失败，已回到 v1：构建失败"]);
    /* noted once on the timeline (it used to be written twice) */
    let notes = k.timeline_texts(&a);
    assert_eq!(
        notes
            .iter()
            .filter(|t| *t == "发布 v2 失败，已回到 v1：构建失败")
            .count(),
        1
    );
}

#[test]
fn a_script_that_ends_without_a_result_fails_the_release() {
    let k = Kit::new();
    let p = project(&k, k.home());
    let (i, _, d) = asking(&k, &p, "steve/x", None);
    k.core.resolve(&d, "yes", "", "me").unwrap();
    std::fs::write(k.home().join("go"), "").unwrap();
    assert!(k.wait(|| result(&k, &d)["outcome"] == "failed"));
    assert_eq!(result(&k, &d)["reason"], "发布脚本没有报告结果就退出了");
    let (status, _, now) = k.status(&i);
    assert_ne!(status, "done");
    assert_eq!(now, "发布失败");
    assert_eq!(k.open_kinds(&i), vec!["fail"]);
    assert!(fail_notices(&k)[0].starts_with("发布脚本没有报告结果就退出了"));
}

#[test]
fn hub_released_but_web_failed_is_not_done() {
    let k = Kit::new();
    let p = project(&k, k.home());
    let (i, _, d) = asking(&k, &p, "steve/x", None);
    k.core.resolve(&d, "yes", "", "me").unwrap();
    /* the Hub restarted (a new process): the result arrives through last.json alone */
    k.core.tx().st.releasing = None;
    std::fs::create_dir_all(k.home().join("releases")).unwrap();
    std::fs::write(
        k.home().join("releases/last.json"),
        json!({"ok": false, "kind": "hub+web", "version": "v3", "prev": "v2", "web_prev": "w2", "hub_ok": true, "web_ok": false, "item": i, "error": "前端构建失败"}).to_string(),
    )
    .unwrap();
    k.core.report_release().unwrap();
    let res = result(&k, &d);
    assert_eq!(res["outcome"], "failed");
    assert_eq!(
        (res["hub_ok"].clone(), res["web_ok"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!(res["note"], "Hub 已是新版本，前端仍是 w2");
    let (status, _, now) = k.status(&i);
    assert_ne!(status, "done");
    assert_eq!(now, "Hub 已发布，前端发布失败");
    assert_eq!(
        k.open_fails(&i),
        vec!["发布Hub 和前端 v3 失败，Hub 已是新版本，前端仍是 w2：前端构建失败"]
    );
    std::fs::write(k.home().join("go"), "").unwrap();
}

#[test]
fn a_health_check_rollback_after_restart_fails_the_release() {
    let k = Kit::new();
    let p = project(&k, k.home());
    let (i, _, d) = asking(&k, &p, "steve/x", None);
    k.core.resolve(&d, "yes", "", "me").unwrap();
    k.core.tx().st.releasing = None;
    std::fs::create_dir_all(k.home().join("releases")).unwrap();
    /* still pending while the script holds the release lock: nothing is reported yet */
    let last = k.home().join("releases/last.json");
    let mut lock = hold_release_lock(&k);
    std::fs::write(
        &last,
        json!({"ok": false, "pending": true, "item": i}).to_string(),
    )
    .unwrap();
    k.core.report_release().unwrap();
    assert_eq!(result(&k, &d)["outcome"], "releasing");
    assert_eq!(k.status(&i).2, "正在发布");
    std::fs::write(
        &last,
        json!({"ok": false, "kind": "hub", "version": "v4", "prev": "v3", "item": i, "error": "新版本启动后健康检查没有通过"}).to_string(),
    )
    .unwrap();
    k.core.report_release().unwrap();
    assert_eq!(result(&k, &d)["outcome"], "failed");
    assert_eq!(result(&k, &d)["reason"], "新版本启动后健康检查没有通过");
    assert_ne!(k.status(&i).0, "done");
    let _ = lock.kill();
    let _ = lock.wait();
    std::fs::write(k.home().join("go"), "").unwrap();
}

#[test]
fn a_degraded_release_says_it_stayed_and_is_not_handed_back() {
    let k = Kit::new();
    let p = project(&k, k.home());
    let (i, _, d) = asking(&k, &p, "steve/x", None);
    k.core.resolve(&d, "yes", "", "me").unwrap();
    k.core.tx().st.releasing = None;
    std::fs::create_dir_all(k.home().join("releases")).unwrap();
    std::fs::write(
        k.home().join("releases/last.json"),
        json!({"ok": false, "degraded": true, "kind": "hub", "version": "v4", "prev": "v3", "item": i,
               "error": "新版本健康检查没有通过；数据库已被新版本迁移，……没有切回 v3，新版本保留（降级运行）"}).to_string(),
    )
    .unwrap();
    k.core.report_release().unwrap();
    let r = result(&k, &d);
    assert_eq!(
        (r["outcome"].as_str(), r["degraded"].as_bool()),
        (Some("failed"), Some(true))
    );
    let notice = fail_notices(&k).pop().unwrap();
    assert!(
        notice.contains("没有回滚") && !notice.contains("已回到"),
        "{notice}"
    );
    assert_eq!(k.status(&i).2, "发布失败，新版本降级运行");
}

#[test]
fn a_rollback_result_is_noted_on_the_item_not_taken_for_its_release() {
    let k = Kit::new();
    let i = k.item("done", "完成", None);
    std::fs::create_dir_all(k.home().join("releases")).unwrap();
    let last = k.home().join("releases/last.json");
    let note = |k: &Kit| {
        k.db()
            .timeline(&i)
            .unwrap()
            .into_iter()
            .map(|t| t.text)
            .collect::<Vec<_>>()
    };
    std::fs::write(
        &last,
        json!({"ok": false, "kind": "rollback", "version": "v3", "prev": "v4", "item": i,
        "error": "不能回滚到 v3：它只认识到 schema v25，数据库（v26）需要至少 v26；当前版本保留"})
        .to_string(),
    )
    .unwrap();
    k.core.report_release().unwrap();
    assert!(
        note(&k)
            .iter()
            .any(|t| t.starts_with("回滚到 v3 没有成功：不能回滚")),
        "{:?}",
        note(&k)
    );
    assert!(
        fail_notices(&k)
            .iter()
            .any(|n| n.contains("回滚到 v3 没有成功"))
    );
    assert_eq!(k.status(&i).0, "done", "the item is not reopened");
    std::fs::write(
        &last,
        json!({"ok": true, "kind": "rollback", "version": "v3", "prev": "v4", "item": i})
            .to_string(),
    )
    .unwrap();
    k.core.report_release().unwrap();
    assert!(
        note(&k).iter().any(|t| t == "已回滚到 v3（之前是 v4）"),
        "{:?}",
        note(&k)
    );
    assert_eq!(k.status(&i).0, "done");
}

/// Hold `releases/.release.lock` the way ops/release.sh does (`zsystem flock`) until killed.
fn hold_release_lock(k: &Kit) -> std::process::Child {
    let rel = k.home().join("releases");
    std::fs::create_dir_all(&rel).unwrap();
    let lock = rel.join(".release.lock");
    let child = std::process::Command::new("/bin/zsh")
        .arg("-c")
        .arg(format!(
            "zmodload zsh/system; touch '{0}'; zsystem flock -t 0 '{0}' && sleep 60",
            lock.display()
        ))
        .spawn()
        .unwrap();
    assert!(
        k.wait(|| crate::release::lock_holder(&lock).is_some()),
        "zsh took the release lock"
    );
    child
}

/// After a restart last.json still says pending, but no script holds the lock: it will never report — failed.
#[test]
fn a_pending_release_whose_script_is_gone_fails_after_a_restart() {
    let k = Kit::new();
    let p = project(&k, k.home());
    let (i, _, d) = asking(&k, &p, "steve/x", None);
    k.core.resolve(&d, "yes", "", "me").unwrap();
    k.core.tx().st.releasing = None;
    let last = k.home().join("releases/last.json");
    let pending = json!({"ok": false, "pending": true, "kind": "hub", "version": "v5", "prev": "v4", "item": i, "error": "发布进行中", "log": "/tmp/r.log", "at": crate::util::now() / 1000});
    /* the script is still running (it holds the lock): wait */
    let mut lock = hold_release_lock(&k);
    std::fs::write(&last, pending.to_string()).unwrap();
    k.core.report_release().unwrap();
    assert_eq!(result(&k, &d)["outcome"], "releasing");
    /* it died without a result */
    lock.kill().unwrap();
    lock.wait().unwrap();
    assert!(crate::release::lock_holder(&k.home().join("releases/.release.lock")).is_none());
    k.core.report_release().unwrap();
    let res = result(&k, &d);
    assert_eq!(res["outcome"], "failed");
    assert_eq!(res["reason"], "发布脚本没有报告结果就退出了");
    let (status, stage, now) = k.status(&i);
    assert_eq!(
        (status.as_str(), stage.as_str(), now.as_str()),
        ("wait", "收尾", "发布失败")
    );
    let why =
        "发布 v5 失败：发布脚本没有报告结果就退出了，不确定现在是哪个版本；请看发布日志 /tmp/r.log";
    assert_eq!(k.open_fails(&i), vec![why]);
    assert_eq!(fail_notices(&k), vec![why]);
    let r: Value = serde_json::from_str(&std::fs::read_to_string(&last).unwrap()).unwrap();
    assert_eq!(
        (r["reported"].clone(), r["pending"].clone()),
        (json!(true), json!(false))
    );
    /* reported once */
    k.core.report_release().unwrap();
    assert_eq!(fail_notices(&k).len(), 1);
    std::fs::write(k.home().join("go"), "").unwrap();
}

/// A pending result older than 30 minutes is given up on even if something still holds the lock.
#[test]
fn a_pending_release_times_out_after_30_minutes() {
    let k = Kit::new();
    let p = project(&k, k.home());
    let (i, _, d) = asking(&k, &p, "steve/x", None);
    k.core.resolve(&d, "yes", "", "me").unwrap();
    k.core.tx().st.releasing = None;
    let mut lock = hold_release_lock(&k);
    let at = crate::util::now() / 1000 - 31 * 60;
    std::fs::write(
        k.home().join("releases/last.json"),
        json!({"ok": false, "pending": true, "version": "v6", "item": i, "at": at}).to_string(),
    )
    .unwrap();
    k.core.report_release().unwrap();
    assert_eq!(result(&k, &d)["outcome"], "failed");
    assert_eq!(result(&k, &d)["reason"], "发布脚本没有报告结果就退出了");
    assert_ne!(k.status(&i).0, "done");
    assert_eq!(k.open_kinds(&i), vec!["fail"]);
    let _ = lock.kill();
    let _ = lock.wait();
    std::fs::write(k.home().join("go"), "").unwrap();
}
