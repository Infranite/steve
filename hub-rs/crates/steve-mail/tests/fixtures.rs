//! The fixture emails parse as intended with `mail-parser` (what E1 will
//! use), so a failing sync test points at the sync, not the fixture.

use mail_parser::MessageParser;
use steve_mail::fake::fixtures;

fn parse(name: &str) -> mail_parser::Message<'static> {
    MessageParser::new()
        .parse(fixtures::get(name).unwrap_or_else(|| panic!("{name}")))
        .unwrap()
}

#[test]
fn every_fixture_parses_with_crlf() {
    assert_eq!(fixtures::ALL.len(), 14);
    for (name, raw) in fixtures::ALL {
        assert!(MessageParser::new().parse(raw).is_some(), "{name}");
        let bare_lf = raw.windows(2).any(|w| w[1] == b'\n' && w[0] != b'\r');
        assert!(!bare_lf, "{name} has bare LF");
    }
}

#[test]
fn charsets_and_encoded_headers() {
    let m = parse("multipart-alternative.eml");
    assert_eq!(m.root().subject(), Some("关于下周三的会议改期到周四下午"));
    assert_eq!(
        m.root().from().unwrap().first().unwrap().name(),
        Some("王小明")
    );
    assert!(m.root().body_text(0).unwrap().contains("周四下午 3 点"));
    assert!(
        m.root()
            .body_html(0)
            .unwrap()
            .contains("<b>周四下午 3 点</b>")
    );

    let m = parse("gbk.eml");
    assert_eq!(m.root().subject(), Some("回复：报价单（修改版）"));
    assert_eq!(
        m.root().from().unwrap().first().unwrap().name(),
        Some("张伟")
    );
    let body = m.root().body_text(0).unwrap();
    assert!(body.contains("单价下调 5%"), "{body}");
    assert!(body.contains("华东销售部 | 区域经理"));

    let m = parse("gb18030.eml");
    assert_eq!(m.root().subject(), Some("会议纪要 䶮"));
    assert!(m.root().body_text(0).unwrap().contains("龑䶮项目延期一周"));

    let m = parse("big5.eml");
    assert_eq!(m.root().subject(), Some("課程地點變更"));
    assert!(m.root().body_text(0).unwrap().contains("三樓會議室"));

    let m = parse("iso-2022-jp.eml");
    assert_eq!(m.root().subject(), Some("打ち合わせ資料の送付"));
    assert!(
        m.root()
            .body_text(0)
            .unwrap()
            .contains("来週の打ち合わせ資料")
    );

    let m = parse("html-tracking.eml");
    assert_eq!(
        m.root().subject(),
        Some("Votre commande est confirmée à la caisse")
    );
    assert_eq!(
        m.root().from().unwrap().first().unwrap().name(),
        Some("Café de la Gare")
    );
    let html = m.root().body_html(0).unwrap();
    for needle in [
        "<script>",
        "onload=",
        "onclick=",
        "<iframe",
        "track.shop.example/open.gif",
        "width=\"1\"",
    ] {
        assert!(html.contains(needle), "{needle}");
    }
    assert!(html.contains("Café de la Gare"));
}

#[test]
fn attachments_inline_and_newsletter() {
    let m = parse("invoice-pdf.eml");
    let atts: Vec<_> = m.root().attachments().collect();
    assert_eq!(atts.len(), 1);
    assert_eq!(atts[0].attachment_name(), Some("发票-2026-10.pdf"));
    assert!(atts[0].decoded().starts_with(b"%PDF-1.4"));
    assert!(m.root().body_text(0).unwrap().contains("¥ 1,286.40"));

    let m = parse("inline-cid.eml");
    let img = m.root().attachments().next().unwrap();
    assert!(img.is_inline());
    assert_eq!(img.content_id(), Some("logo@acme.example"));
    assert!(img.decoded().starts_with(b"\x89PNG"));
    assert!(
        m.root()
            .body_html(0)
            .unwrap()
            .contains("cid:logo@acme.example")
    );

    let m = parse("newsletter.eml");
    let h = m.root().headers();
    assert!(h.get("List-Unsubscribe").is_some());
    assert!(h.get("List-Unsubscribe-Post").is_some());
    assert!(h.get("Precedence").is_some());
    assert!(m.root().body_html(0).unwrap().contains("o/abc123.gif"));
}

#[test]
fn threads_and_quotes() {
    let [a, b, c] = ["thread-1.eml", "thread-2.eml", "thread-3.eml"].map(parse);
    assert_eq!(a.root().message_id(), Some("thread-1.alice@acme.example"));
    assert_eq!(
        b.root().in_reply_to().unwrap().first(),
        Some("thread-1.alice@acme.example")
    );
    let refs: Vec<&str> = c.root().references().unwrap().iter().collect();
    assert_eq!(
        refs,
        ["thread-1.alice@acme.example", "thread-2.me@example.com"]
    );
    assert_eq!(
        c.root().in_reply_to().unwrap().first(),
        Some("thread-2.me@example.com")
    );
    assert!(b.root().body_text(0).unwrap().contains("wrote:\r\n> "));
    assert!(c.root().body_text(0).unwrap().contains("写道：\r\n> "));
    assert_eq!(
        c.root().cc().unwrap().first().unwrap().address(),
        Some("bob@acme.example")
    );

    let o = parse("outlook-quote.eml");
    let body = o.root().body_text(0).unwrap();
    assert!(
        body.contains("________________________________\r\nFrom: Me <me@example.com>\r\nSent: ")
    );

    let n = parse("no-message-id.eml");
    assert_eq!(n.root().message_id(), None);
    assert!(n.root().date().is_some());
}

#[test]
fn large_fixture() {
    let big = fixtures::large(28 * 1024 * 1024);
    assert!(big.len() >= 28 * 1024 * 1024 && big.len() < 28 * 1024 * 1024 + 200);
    let m = MessageParser::new().parse(&big).unwrap();
    assert_eq!(m.root().attachments().count(), 1);
    assert_eq!(m.root().subject(), Some("Scanned documents (large)"));
}
