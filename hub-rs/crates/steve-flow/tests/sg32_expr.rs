//! SG32: expression token boundaries through the public parser/evaluator.
use serde_json::{Value, json};
use steve_flow::expr::{self, Expr, Scope};
#[test]
fn sg32_quote_delimiter_escape_and_cursor_table() {
    for (src, want) in [
        (r#""plain""#, "plain"),
        ("'single'", "single"),
        ("“curly”", "curly"),
        ("「bracket」", "bracket"),
        (r#""a\nb""#, "anb"),
        (r#"'a\'b'"#, "a'b"),
        (r#"「a\」b」"#, "a」b"),
        (r#""""#, ""),
    ] {
        assert_eq!(expr::parse(src).unwrap(), Expr::Lit(json!(want)), "{src}");
        assert!(expr::eval(
            &format!("{src} == {}", serde_json::to_string(want).unwrap()),
            &Scope::default()
        ));
    }
    for src in ["\"open", "'open", "“open", "「open", r#""tail\"#] {
        assert_eq!(
            steve_i18n::stored(|| expr::parse(src)).unwrap_err(),
            "引号没有配对",
            "{src}"
        );
    }
}
#[test]
fn sg32_numeric_word_path_and_boolean_boundaries() {
    let input = json!({"amount":-12.5,"list":[{"name":"Ada"}],"flag":true});
    let scope = Scope {
        input: Some(&input),
        ..Default::default()
    };
    for (src, want) in [
        ("amount <= -12.5 && input.list[0].name == 'Ada'", true),
        ("TRUE and not FALSE", true),
        ("false || true && false", false),
        ("null == missing", true),
        ("input.flag && (amount > -13)", true),
        ("[1,2,3] contains 2", true),
        ("input.list[0].name startswith 'ad'", false),
    ] {
        assert_eq!(expr::eval(src, &scope), want, "{src}");
    }
    assert_eq!(expr::parse("-12.5").unwrap(), Expr::Lit(json!(-12.5)));
    assert_eq!(
        steve_i18n::stored(|| expr::parse("1.2.3")).unwrap_err(),
        "数字「1.2.3」写得不对"
    );
    assert_eq!(
        steve_i18n::stored(|| expr::parse("#")).unwrap_err(),
        "不认识的符号「#」"
    );
    assert_eq!(scope.get("input.list[0].name"), json!("Ada"));
    assert_eq!(scope.get("input.list[9]"), Value::Null);
}
