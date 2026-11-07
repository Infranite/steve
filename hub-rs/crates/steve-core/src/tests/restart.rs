//! hub/test/restart.test.ts and the release hand-off: what a restart leaves of a cut turn, the drain hold, the release script.

use crate::git::tests::{branch_with, g, repo};
use crate::patch;
use crate::repo::{Release, WS};
use crate::runtime::AgentRuntime;
use crate::testkit::Kit;

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
        done_when: None,
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
        done_when: None,
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
        done_when: None,
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
fn upgrade_maintenance_refuses_source_release_and_rollback() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    k.db().store().maintenance_set(true).unwrap();
    assert!(k.core.tx().start_release("steve/test", &i).is_err());
    assert!(k.core.tx().start_rollback(&i, "previous", "").is_err());
    assert!(!k.core.source_release_busy());
}

/// A resume intent as an older Hub's boot left it (`session_resume`), `age` ms ago.
fn intent(k: &Kit, sid: &str, age: i64) {
    k.db()
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO session_resume VALUES (?1, ?2)",
                rusqlite::params![sid, crate::util::now() - age],
            )?;
            Ok(())
        })
        .unwrap();
}

fn resume_ids(k: &Kit) -> Vec<String> {
    k.db().store().queued().resume_ids().unwrap()
}

/// B7: there is no in-process runner to continue a cut turn. One no backend recovers is said
/// once (with a failure card where the item still runs) and the session is idle again; prompts
/// held for the restart go once the listener is up; nothing is ever sent as a resume.
#[test]
fn after_a_restart_a_turn_nothing_continues_is_said_once_and_held_prompts_go() {
    let k = Kit::new();
    let cut_i = k.item("run", "执行", None);
    let cut = k.session(&cut_i, "running");
    k.db().queue_add(&cut, "补充：用中文", "me", &[]).unwrap();
    let wait_i = k.item("run", "执行", None);
    let waiting = k.session(&wait_i, "running");
    k.decision(&wait_i, Some(&waiting), "ask", "question", &["a"]);
    k.db().queue_add(&waiting, "等一下再说", "me", &[]).unwrap();
    let held_i = k.item("run", "执行", None);
    let held = k.session(&held_i, "idle");
    k.db().queue_add(&held, "新的一回合", "me", &[]).unwrap();

    for _ in 0..2 {
        k.core.recover().unwrap();
        k.core.ready();
    }
    assert_eq!(k.db().session(&cut).unwrap().unwrap().status, "idle");
    assert_eq!(
        k.open_fails(&cut_i),
        vec!["Hub 重启时这个会话正在运行，这一回合被中断了"],
        "one card, not one per boot"
    );
    assert_eq!(notices(&k, &cut, "没有自动继续"), 1);
    assert!(
        k.rt.prompts_of(&cut).is_empty(),
        "its queue waits for the card"
    );
    assert_eq!(k.db().queued(&cut).unwrap().len(), 1);
    assert_eq!(notices(&k, &waiting, "事项在等你处理"), 1);
    assert!(
        k.open_fails(&wait_i).is_empty(),
        "no card on a waiting item"
    );
    assert!(k.rt.prompts_of(&waiting).is_empty());
    assert_eq!(
        k.rt.prompts_of(&held),
        vec!["新的一回合"],
        "a prompt held for the restart is sent after it"
    );
    assert!(resume_ids(&k).is_empty());
    assert!(
        !k.timeline_texts(&cut_i)
            .contains(&"Hub 重启打断了会话，已自动继续".to_string())
    );
}

#[test]
fn an_interruption_on_a_paused_or_finished_item_is_noticed_once_without_a_card() {
    let k = Kit::new();
    let p = k.item("pause", "执行", None);
    let ps = k.session(&p, "running");
    let d = k.item("done", "收尾", None);
    let ds = k.session(&d, "running");
    intent(&k, &ds, 0);
    for _ in 0..3 {
        k.core.recover().unwrap();
        k.core.ready();
    }
    assert!(k.rt.prompts_of(&ps).is_empty() && k.rt.prompts_of(&ds).is_empty());
    assert_eq!(notices(&k, &ps, "事项已暂停"), 1);
    assert_eq!(notices(&k, &ds, "事项已结束"), 1);
    assert!(k.open_fails(&p).is_empty() && k.open_fails(&d).is_empty());
    assert!(resume_ids(&k).is_empty(), "no intent is kept");
}

#[test]
fn a_boot_without_resuming_flushes_nothing() {
    let k = Kit::with(|c| c.no_resume = true);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    let h = k.item("run", "执行", None);
    let held = k.session(&h, "idle");
    k.db().queue_add(&held, "排着的", "me", &[]).unwrap();
    k.core.recover().unwrap();
    k.core.recover().unwrap();
    k.core.ready();
    assert!(k.rt.prompts_of(&s).is_empty() && k.rt.prompts_of(&held).is_empty());
    assert_eq!(k.open_fails(&i).len(), 1);
    assert_eq!(notices(&k, &s, "没有自动继续"), 1);
}

#[test]
fn a_finish_after_boot_closes_at_once() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    intent(&k, &s, 0);
    k.core.recover().unwrap();
    // nothing will continue the cut turn: no turn holds the close
    k.finish(&i).unwrap();
    k.core.ready();
    assert!(k.rt.prompts_of(&s).is_empty());
    assert_eq!(k.phase(&i), "closed");
}

#[test]
fn a_session_that_recovers_itself_is_left_to_its_backend() {
    let k = Kit::new();
    // a Node-owned session as D5 left it: never moved past 'starting', a resume intent from an
    // earlier boot that is now over a day old, an open tool permission and a queued message
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "starting");
    k.rt.st().recovers_itself.insert(s.clone());
    intent(&k, &s, crate::orchestrator::RESUME_MAX_AGE_MS + 1);
    let card = k.decision(&i, Some(&s), "approve", "tool_call", &["a|允许|allow_once"]);
    k.db().queue_add(&s, "排队的话", "me", &[]).unwrap();
    // a session no backend holds in the same database is said to be cut, once
    let li = k.item("run", "执行", None);
    let legacy = k.session(&li, "running");

    for _ in 0..2 {
        k.core.recover().unwrap();
        k.core.ready();
    }

    assert!(k.rt.prompts_of(&s).is_empty(), "no RESUME, no flush");
    assert_eq!(
        k.db().queued(&s).unwrap().len(),
        1,
        "the queue waits for the open card"
    );
    let system: Vec<_> = k
        .db()
        .events_after(&s, 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "system")
        .collect();
    assert!(system.is_empty(), "no system event: {system:?}");
    assert!(k.open_fails(&i).is_empty(), "no failure card");
    assert!(resume_ids(&k).is_empty(), "the leftover intent is dropped");
    assert_eq!(
        k.db().decision(&card).unwrap().unwrap().status,
        "open",
        "the Node expires its own cards"
    );
    assert!(
        !k.timeline_texts(&i).iter().any(|t| t.contains("Hub 重启")),
        "nothing on the timeline"
    );
    assert!(k.rt.prompts_of(&legacy).is_empty(), "never resumed");
    assert_eq!(notices(&k, &legacy, "没有自动继续"), 1);
}

#[test]
fn a_leftover_intent_never_holds_the_queue_of_a_session_that_recovers_itself() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    intent(&k, &s, 0);
    k.rt.st().recovers_itself.insert(s.clone());
    k.db().queue_add(&s, "后来的话", "me", &[]).unwrap();
    k.core.recover().unwrap();
    k.core.ready();
    assert_eq!(k.rt.prompts_of(&s), vec!["后来的话"], "no RESUME");
    assert!(resume_ids(&k).is_empty());
}
