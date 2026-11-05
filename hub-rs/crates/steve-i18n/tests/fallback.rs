//! A key en lacks comes back in zh-CN (the fixture's en file leaves one out on purpose; shipped catalogs may
//! not, see catalogs.rs).

rust_i18n::i18n!("tests/fixtures/gap", fallback = "zh-CN");

use steve_i18n::{EN, scope, t};

#[tokio::test]
async fn an_english_gap_falls_back_to_chinese() {
    scope(EN, async {
        assert_eq!(t!("i18n.gap.both"), "Both");
        assert_eq!(t!("i18n.gap.only_zh", name = "x"), "只有中文，x");
    })
    .await;
    assert_eq!(t!("i18n.gap.both"), "两种都有");
}
