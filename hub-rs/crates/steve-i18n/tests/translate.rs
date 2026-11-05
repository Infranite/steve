//! `t!` in the request's language, with names filled in (the keys are in this crate's own locales/).

steve_i18n::init!();

use steve_i18n::{EN, ZH_CN, scope, t};

#[tokio::test]
async fn by_language_with_names() {
    assert_eq!(t!("i18n.test.hello", name = "小王"), "你好，小王");
    assert_eq!(t!("i18n.test.count", n = 3), "共 3 个");
    scope(EN, async {
        assert_eq!(t!("i18n.test.hello", name = "Wang"), "Hello, Wang");
        assert_eq!(t!("i18n.test.count", n = 3), "3 in all");
    })
    .await;
    scope(ZH_CN, async {
        assert_eq!(t!("i18n.test.count", n = 2), "共 2 个")
    })
    .await;
}
