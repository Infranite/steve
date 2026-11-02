//! hub/test/restart.test.ts and the release hand-off: resume after a restart, the drain hold, the release script.

use crate::git::tests::{branch_with, g, repo};
use crate::orchestrator::RESUME;
use crate::patch;
use crate::repo::{Release, WS};
use crate::runtime::AgentRuntime;
use crate::testkit::Kit;

#[test]
fn after_a_restart_the_interrupted_turn_resumes_once_with_what_was_queued() {
    let k = Kit::new();
    let cut_i = k.item("run", "执行", None);
    let cut = k.session(&cut_i, "running");
    for (t, v) in [
        ("请继续。", "steve"),
        ("请继续。", "steve"),
        ("补充：用中文", "me"),
    ] {
        k.db().queue_add(&cut, t, v, &[]).unwrap();
    }
    let wait_i = k.item("run", "执行", None);
    let waiting = k.session(&wait_i, "running");
    k.decision(&wait_i, Some(&waiting), "ask", "question", &["a"]);
    k.db().queue_add(&waiting, "等一下再说", "me", &[]).unwrap();
    let held_i = k.item("run", "执行", None);
    let held = k.session(&held_i, "idle");
    k.db().queue_add(&held, "新的一回合", "me", &[]).unwrap();

    k.core.recover().unwrap();
    k.core.fire_resume();
    assert_eq!(
        k.rt.prompts_of(&cut),
        vec![format!("{RESUME}\n\n请继续。\n\n补充：用中文")]
    );
    assert!(k.db().queued(&cut).unwrap().is_empty());
    assert_eq!(
        k.rt.prompts_of(&held),
        vec!["新的一回合"],
        "a prompt held for the restart is sent after it"
    );
    assert!(k.rt.prompts_of(&waiting).is_empty());
    assert_eq!(
        k.db().queued(&waiting).unwrap().len(),
        1,
        "an item waiting on the user keeps its queue"
    );
    assert!(
        k.open_fails(&cut_i).is_empty(),
        "no interruption card for a resume"
    );
    assert!(
        k.timeline_texts(&cut_i)
            .contains(&"Hub 重启打断了会话，已自动继续".to_string())
    );
    /* the resumed turn ends normally: nothing special happens */
    let s = k.db().session(&cut).unwrap().unwrap();
    k.core.tool_deliver(&s, "好了", &[], &[], &[]).unwrap();
    k.rt.end_turn(&k.core, &cut, "end_turn", None);
    assert_eq!(k.open_kinds(&cut_i), vec!["accept"]);
}

#[test]
fn drain_refuses_a_call_without_the_release_token() {
    let k = Kit::new();
    assert_eq!(k.core.drain("nope", true).unwrap(), None);
    assert!(!k.rt.is_holding());
}

/// A release script that waits for `$STEVE_HOME/go`, then records its arguments and exits.
fn release_project(k: &Kit) -> String {
    let script = k.home().join("release.sh");
    std::fs::write(
        &script,
        super::fake_release_script(
            "echo \"$1 $2 $STEVE_PORT ${#STEVE_DRAIN_TOKEN}\" > \"$STEVE_HOME/args\"\n",
        ),
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
            &k.home().to_string_lossy(),
            "dir",
            false,
            Some("dev"),
            Some(&rel),
        )
        .unwrap()
        .id
}

#[test]
fn approving_a_release_runs_the_script_and_drain_holds_until_it_exits() {
    let k = Kit::new();
    let p = release_project(&k);
    let i = k.item("wait", "收尾", Some(&p));
    k.db().item_patch(&i, patch![branch: "steve/x"]).unwrap();
    let s = k.session(&i, "idle");
    let r = k.decision(&i, Some(&s), "approve", "release", &["yes", "no"]);
    k.db()
        .exec(
            "UPDATE decisions SET subject_id = 'steve/x' WHERE id = ?",
            &[&r],
        )
        .unwrap();
    k.core.resolve(&r, "yes", "", "me").unwrap();
    assert_eq!(k.status(&i).2, "正在发布");
    assert!(
        k.timeline_texts(&i)
            .iter()
            .any(|t| t.starts_with("开始发布 steve/x，日志 "))
    );

    /* a second release meanwhile is refused with a fail card */
    let other = k.item("wait", "收尾", Some(&p));
    let r2 = k.decision(&other, None, "approve", "release", &["yes", "no"]);
    k.core.resolve(&r2, "yes", "", "me").unwrap();
    assert!(k.open_fails(&other)[0].starts_with(&format!("另一个发布（{i}）正在进行")));

    /* the script asks to drain with its token (read back from the environment it got) */
    let busy_i = k.item("run", "执行", None);
    let busy = k.session(&busy_i, "idle");
    k.rt.prompt(&busy, "做事", "me", &[]).unwrap();
    let token = k.core.tx().st.releasing.as_ref().unwrap().token.clone();
    assert_eq!(k.core.hold_status(), None, "approved, not holding yet");
    let d = k.core.drain(&token, true).unwrap().unwrap();
    assert_eq!((d.idle, d.running), (false, 1));
    assert!(k.rt.is_holding());
    /* the hold is in /api/state and on SSE: which item, how many turns it waits for, since when */
    let h = k.core.hold_status().unwrap();
    assert_eq!((h.item.as_str(), h.running), (i.as_str(), 1));
    assert!(crate::util::now() - h.since < 5_000);
    let holds = || {
        k.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                crate::repo::HubEvent::ReleaseHold { hold } => Some(hold.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(holds()[0].as_ref().unwrap()["item"], i.as_str());
    assert!(
        k.timeline_texts(&i)
            .last()
            .unwrap()
            .starts_with("新版本已构建好，等 1 个正在进行的回合结束后重启")
    );
    /* new prompts queue during the hold */
    let idle_i = k.item("run", "执行", None);
    let idle = k.session(&idle_i, "idle");
    assert_eq!(
        k.rt.prompt(&idle, "排队的", "me", &[]).unwrap(),
        crate::Prompted::Queued
    );
    /* 立即送达 during the hold leaves the queue alone: no second copy, `via` kept */
    k.core.deliver_queued(&idle_i).unwrap();
    let q = k.db().queued(&idle).unwrap();
    assert_eq!((q.len(), q[0].via.as_str()), (1, "me"));
    let queued_events = |sid: &str| {
        k.db()
            .events_after(sid, 0)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "queued")
            .count()
    };
    assert_eq!(queued_events(&idle), 1);
    /* the same message again (an accept reply sent twice) is not queued a second time */
    assert_eq!(
        k.rt.prompt(&idle, "排队的", "steve", &[]).unwrap(),
        crate::Prompted::Queued
    );
    assert_eq!(
        (k.db().queued(&idle).unwrap().len(), queued_events(&idle)),
        (1, 1)
    );
    k.rt.end_turn(&k.core, &busy, "end_turn", None);
    assert_eq!(k.core.drain(&token, false).unwrap().unwrap().running, 0);
    assert!(
        k.wait(|| holds()
            .last()
            .unwrap()
            .as_ref()
            .is_some_and(|h| h["running"] == 0)),
        "the running count is published as it drops"
    );

    /* the script exits without restarting us (it failed): the hold lifts and held prompts go */
    std::fs::write(k.home().join("go"), "").unwrap();
    assert!(k.wait(|| !k.rt.is_holding()));
    assert_eq!(k.core.hold_status(), None);
    assert_eq!(
        holds().last().unwrap(),
        &None,
        "the end of the hold is published"
    );
    assert!(k.wait(|| k.rt.prompts_of(&idle) == vec!["排队的"]));
    let args = std::fs::read_to_string(k.home().join("args")).unwrap();
    assert_eq!(args.trim(), format!("steve/x {i} 8440 32"));
    assert_eq!(k.core.drain(&token, false).unwrap(), None);
}

#[test]
fn a_missing_release_script_fails_the_item() {
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
        .unwrap();
    let i = k.item("wait", "收尾", Some(&p.id));
    let r = k.decision(&i, None, "approve", "release", &["yes", "no"]);
    k.core.resolve(&r, "yes", "", "me").unwrap();
    assert_eq!(
        k.open_fails(&i),
        vec!["这个项目的发布脚本不存在（/nonexistent/release.sh），无法发布"]
    );
}

#[test]
fn a_conflicting_rebase_before_a_release_goes_back_to_the_agent() {
    let k = Kit::new();
    let r = repo(k.home());
    let rel = Release {
        kind: "script".into(),
        cmd: k.home().join("release.sh").to_string_lossy().into(),
    };
    let p = k
        .db()
        .project_create(
            WS,
            "G",
            "",
            &r.to_string_lossy(),
            "git",
            true,
            None,
            Some(&rel),
        )
        .unwrap();
    let i = k.item("wait", "收尾", Some(&p.id));
    let (name, wt) = branch_with(&r, &k.core.config().worktrees, &i, "a.txt", "mine\n");
    std::fs::write(r.join("a.txt"), "theirs\n").unwrap();
    g(&r, &["commit", "-qam", "main a"]);
    k.db()
        .item_patch(
            &i,
            patch![cwd: wt.to_string_lossy().to_string(), branch: name.as_str()],
        )
        .unwrap();
    let s = k.session(&i, "idle");
    let d = k.decision(&i, Some(&s), "approve", "release", &["yes", "no"]);
    k.db()
        .exec(
            "UPDATE decisions SET subject_id = ? WHERE id = ?",
            &[&name, &d],
        )
        .unwrap();
    k.core.resolve(&d, "yes", "", "me").unwrap();
    assert_eq!(k.status(&i).2, "要先 rebase 解决冲突");
    assert!(
        k.rt.prompts_of(&s)
            .last()
            .unwrap()
            .contains("自动 rebase 有冲突（a.txt），已经撤销，没有发布")
    );
    assert!(k.core.tx().st.releasing.is_none());
}

#[test]
fn a_clean_rebase_before_a_release_is_noted() {
    let k = Kit::new();
    let r = repo(k.home());
    let p = k
        .db()
        .project_create(WS, "G", "", &r.to_string_lossy(), "git", true, None, None)
        .unwrap();
    let i = k.item("wait", "收尾", Some(&p.id));
    let (name, wt) = branch_with(&r, &k.core.config().worktrees, &i, "b.txt", "b\n");
    std::fs::write(r.join("c.txt"), "c\n").unwrap();
    g(&r, &["add", "."]);
    g(&r, &["commit", "-qm", "main c"]);
    k.db()
        .item_patch(
            &i,
            patch![cwd: wt.to_string_lossy().to_string(), branch: name.as_str()],
        )
        .unwrap();
    let to = crate::orchestrator::To::Session("none".into());
    assert!(k.core.tx().rebase_for_release(&i, &name, &to).unwrap());
    assert!(
        k.timeline_texts(&i)
            .last()
            .unwrap()
            .contains("rebase 到最新的 main")
    );
}

#[test]
fn second_restart_before_ready_keeps_the_durable_resume() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    k.core.recover().unwrap();
    assert_eq!(k.db().session(&s).unwrap().unwrap().status, "idle");
    // Lose every in-memory scheduling field exactly as a process exit would.
    {
        let mut tx = k.core.tx();
        tx.st.resumes.clear();
        tx.st.resumed.clear();
    }
    k.core.recover().unwrap();
    assert!(k.rt.prompts_of(&s).is_empty());
    k.core.ready();
    assert_eq!(k.rt.prompts_of(&s), vec![RESUME]);
    k.core.ready();
    assert_eq!(k.rt.prompts_of(&s), vec![RESUME]);
}

#[test]
fn a_new_pause_after_recovery_preparation_still_blocks_ready() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    k.core.recover().unwrap();
    k.db().item_patch(&i, patch![status: "pause"]).unwrap();
    k.core.ready();
    assert!(k.rt.prompts_of(&s).is_empty());
    assert!(k.db().store().queued().resume_ids().unwrap().contains(&s));
}

fn notices(k: &Kit, sid: &str, needle: &str) -> usize {
    k.db()
        .events_after(sid, 0)
        .unwrap()
        .iter()
        .filter(|e| {
            e.kind == "system" && e.data["text"].as_str().is_some_and(|t| t.contains(needle))
        })
        .count()
}

#[test]
fn a_paused_interruption_is_noticed_once_and_kept_across_restarts() {
    let k = Kit::new();
    let i = k.item("pause", "执行", None);
    let s = k.session(&i, "running");
    for _ in 0..3 {
        k.core.recover().unwrap();
        k.core.ready();
    }
    assert!(
        k.rt.prompts_of(&s).is_empty(),
        "never continued while paused"
    );
    assert_eq!(
        notices(&k, &s, "事项已暂停"),
        1,
        "one notice, not one per restart"
    );
    assert!(
        k.db().store().queued().resume_ids().unwrap().contains(&s),
        "kept for when the pause ends"
    );
}

#[test]
fn an_interruption_on_a_finished_item_is_dropped_after_one_notice() {
    let k = Kit::new();
    let i = k.item("done", "收尾", None);
    let s = k.session(&i, "running");
    for _ in 0..3 {
        k.core.recover().unwrap();
        k.core.ready();
    }
    assert!(k.rt.prompts_of(&s).is_empty());
    assert_eq!(notices(&k, &s, "事项已结束"), 1);
    assert!(!k.db().store().queued().resume_ids().unwrap().contains(&s));
}

#[test]
fn a_boot_without_resuming_leaves_nothing_to_revive_later() {
    let k = Kit::with(|c| c.no_resume = true);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    k.core.recover().unwrap();
    k.core.recover().unwrap();
    k.core.ready();
    assert!(k.rt.prompts_of(&s).is_empty());
    assert!(
        !k.db().store().queued().resume_ids().unwrap().contains(&s),
        "a later boot with resuming on finds no intent"
    );
    assert_eq!(
        k.open_fails(&i).len(),
        1,
        "one interruption card, not one per boot"
    );
    assert_eq!(notices(&k, &s, "没有自动继续"), 1);
}

#[test]
fn an_interruption_older_than_a_day_is_not_continued() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    k.core.recover().unwrap();
    // the boot never reached ready; a day later the Hub starts again
    {
        let mut tx = k.core.tx();
        tx.st.resumes.clear();
    }
    k.db()
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE session_resume SET created = created - ? WHERE session_id = ?",
                rusqlite::params![crate::orchestrator::RESUME_MAX_AGE_MS + 1, s],
            )?;
            Ok(())
        })
        .unwrap();
    k.core.recover().unwrap();
    k.core.ready();
    assert!(k.rt.prompts_of(&s).is_empty());
    assert!(!k.db().store().queued().resume_ids().unwrap().contains(&s));
    assert_eq!(k.open_fails(&i).len(), 1);
}

#[test]
fn a_resume_beyond_the_window_fails_once_not_on_every_restart() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    k.core.recover().unwrap();
    k.core.ready();
    assert_eq!(k.rt.prompts_of(&s), vec![RESUME]);
    // the resumed turn was submitted (that is what counts) and the Hub stops again, twice
    k.db().store().queued().submitted(&s).unwrap();
    k.db().session_patch(&s, patch![status: "running"]).unwrap();
    for _ in 0..2 {
        k.core.recover().unwrap();
        k.core.ready();
    }
    assert_eq!(
        k.rt.prompts_of(&s).len(),
        1,
        "no second resume within the window"
    );
    assert_eq!(k.open_fails(&i).len(), 1, "one card");
    assert!(!k.db().store().queued().resume_ids().unwrap().contains(&s));
}

#[test]
fn a_finish_between_boot_and_ready_blocks_the_resume() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    k.core.recover().unwrap();
    k.db().item_patch(&i, patch![status: "done"]).unwrap();
    k.core.ready();
    assert!(k.rt.prompts_of(&s).is_empty());
}

#[test]
fn a_message_delivered_before_the_stop_comes_back_to_be_checked() {
    let (text, via) = crate::resumed_text("resumed:me", "发布一下");
    assert_eq!(via, "me");
    assert!(
        text.starts_with(crate::RESUMED_NOTE) && text.ends_with("发布一下"),
        "{text}"
    );
    assert_eq!(
        crate::resumed_text("me", "x"),
        ("x".to_string(), "me".to_string())
    );
}

#[test]
fn upgrade_maintenance_refuses_source_release_and_rollback() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    k.db().store().maintenance_set(true).unwrap();
    assert!(k.core.tx().start_release("steve/test", &i).is_err());
    assert!(k.core.tx().start_rollback(&i, "previous").is_err());
    assert!(!k.core.source_release_busy());
}
