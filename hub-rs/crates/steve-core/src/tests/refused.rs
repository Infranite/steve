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
