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

/// Fixed zh-CN text written earlier comes back in the reader's language; zh-CN and other text stay.
#[test]
fn fixed_text_is_reworded() {
    const KEYS: &[&str] = &["i18n.test.hello", "i18n.test.count"];
    let en = |s: &str| steve_i18n::with(EN, || steve_i18n::reword!(s, KEYS));
    assert_eq!(en("你好，小王").as_deref(), Some("Hello, 小王"));
    assert_eq!(en("共 12 个").as_deref(), Some("12 in all"));
    assert_eq!(en("你好，"), None, "a value is never empty");
    assert_eq!(en("随便说的话"), None);
    let zh = steve_i18n::with(ZH_CN, || steve_i18n::reword!("你好，小王", KEYS));
    assert_eq!(zh, None);
}
