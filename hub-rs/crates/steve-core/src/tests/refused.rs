//! A new session no backend takes is refused where the user sees it.

use crate::CreateItem;
use crate::repo::WS;
use crate::testkit::Kit;

/// A new session no backend takes is ended at once, said in it and on its item, and the start
/// fails with why; it never waits for a runner that is not there.
#[test]
fn a_new_session_no_backend_takes_is_refused_visibly() {
    let k = Kit::new();
    k.rt.st().refuse = Some("本机 Node 已关闭".into());
    let e = k.core.create_item(CreateItem {
        ws: WS.into(),
        goal: "g".into(),
        title: "标题".into(),
        ..Default::default()
    });
    let i = match e {
        Err(e) => {
            assert!(e.to_string().contains("本机 Node 已关闭"), "{e}");
            k.db()
                .items(WS)
                .unwrap()
                .into_iter()
                .find(|i| i.title == "标题")
                .expect("the item is there")
        }
        Ok(i) => i,
    };
    let s = k.db().sessions_of_item(&i.id).unwrap().pop().unwrap();
    assert_eq!(s.status, "ended");
    assert!(k.rt.prompts_of(&s.id).is_empty(), "never prompted");
    assert!(
        k.db()
            .events_after(&s.id, 0)
            .unwrap()
            .iter()
            .any(|e| e.kind == "system" && e.data["refused"] == true)
    );
    let fails = k.open_fails(&i.id);
    assert_eq!(fails.len(), 1, "one card: {fails:?}");
    assert!(fails[0].contains("本机 Node 已关闭"), "{fails:?}");
}

/// #222: a Bot whose item could not run at all (its principal's model access is not configured) is
/// stopped before anything is made: no session, one clear error on the item, and nothing tries
/// again by itself; giving the item to such a Bot is refused and leaves it as it was.
#[test]
fn a_bot_that_cannot_run_gets_no_session_and_one_clear_error() {
    let k = Kit::new();
    let why =
        "定时任务的模型接入还没配置（codex）：请到「设置 · 模型 · 服务会话的模型接入」里配置。";
    k.rt.st().cannot_run = Some(why.into());
    let e = k.core.create_item(CreateItem {
        ws: WS.into(),
        goal: "g".into(),
        title: "整理记忆".into(),
        source: Some(("定时".into(), "sched".into())),
        ..Default::default()
    });
    let i = match e {
        Err(e) => {
            assert!(e.to_string().contains("模型接入"), "{e}");
            k.db()
                .items(WS)
                .unwrap()
                .into_iter()
                .find(|i| i.title == "整理记忆")
                .expect("the item is there")
        }
        Ok(i) => i,
    };
    assert!(
        k.db().sessions_of_item(&i.id).unwrap().is_empty(),
        "no session"
    );
    let fails = k.open_fails(&i.id);
    assert_eq!(fails.len(), 1, "one card: {fails:?}");
    assert!(fails[0].contains(why), "{fails:?}");
    // asked again with the same trouble: still one card, still no session
    let _ = k.core.item_action(&i.id, crate::ItemAction::Resume, "me");
    assert_eq!(k.open_fails(&i.id).len(), 1);
    assert!(k.db().sessions_of_item(&i.id).unwrap().is_empty());
    // an item that runs is not given to a Bot that cannot: refused, nothing stopped or changed
    k.rt.st().cannot_run = None;
    let ok = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            goal: "g".into(),
            title: "能跑".into(),
            ..Default::default()
        })
        .unwrap();
    let before = k.db().sessions_of_item(&ok.id).unwrap();
    assert_eq!(before.len(), 1);
    k.rt.st().cannot_run = Some(why.into());
    let e = k.core.reassign(&ok.id, "dev", "me").unwrap_err();
    assert!(e.to_string().contains("模型接入"), "{e}");
    let after = k.db().sessions_of_item(&ok.id).unwrap();
    assert_eq!(after.len(), 1);
    assert_ne!(after[0].status, "ended");
    assert_eq!(
        k.db().item(&ok.id).unwrap().unwrap().bot_id,
        before[0].bot_id.clone().or(Some("steve".into()))
    );
}
