//! What an installation seeds in zh-CN and a person may edit later: the name of the workspace `default`, the
//! role, speaking style and rules of the main Bot `steve` (and, beside Steve's own source, of the Bot `dev`
//! and the description of its project), and the introduction and speaking style of the default channel
//! persona. The seeding code writes [`text`] (zh-CN, whatever the request's language): models read these.
//!
//! The API shows a value that is still exactly the seed in the reader's language ([`shown`], [`show`]) and
//! turns one a page showed that way and sent back unchanged into the seed again ([`stored`], [`store`]), so
//! the database keeps the zh-CN seed until someone writes something else. A value someone wrote is left
//! as it is both ways. Bot names stay as stored: they are what `@` in a message matches.

use serde_json::{Map, Value};

pub const WORKSPACE_NAME: &str = "store.seed.workspace_name";
pub const STEVE_ROLE: &str = "store.seed.steve_role";
pub const STEVE_PERSONA: &str = "store.seed.steve_persona";
pub const STEVE_RULES: &str = "store.seed.steve_rules";
pub const DEV_ROLE: &str = "store.seed.dev_role";
pub const DEV_PERSONA: &str = "store.seed.dev_persona";
pub const DEV_RULES: &str = "store.seed.dev_rules";
pub const STEVE_PROJECT: &str = "store.seed.steve_project";
/// `%{owner}`: the workspace owner's name.
pub const PERSONA_INTRO: &str = "store.seed.persona_intro";

/// The seeded texts of each field, by the object they are in.
pub const WORKSPACE: &[(&str, &[&str])] = &[("name", &[WORKSPACE_NAME])];
pub const BOT: &[(&str, &[&str])] = &[
    ("role", &[STEVE_ROLE, DEV_ROLE]),
    ("persona", &[STEVE_PERSONA, DEV_PERSONA]),
    ("rules", &[STEVE_RULES, DEV_RULES]),
];
pub const PROJECT: &[(&str, &[&str])] = &[("description", &[STEVE_PROJECT])];
/// A persona's speaking style is its Bot's when it is seeded.
pub const PERSONA: &[(&str, &[&str])] = &[
    ("intro", &[PERSONA_INTRO]),
    ("soul", &[STEVE_PERSONA, DEV_PERSONA]),
];

/// The seed `key` as it is stored (zh-CN).
pub fn text(key: &str) -> String {
    rust_i18n::t!(key, locale = steve_i18n::ZH_CN).into_owned()
}

/// The default persona's introduction as it is stored.
pub fn persona_intro(owner: &str) -> String {
    rust_i18n::t!(PERSONA_INTRO, locale = steve_i18n::ZH_CN, owner = owner).into_owned()
}

/// A stored value that is still one of `keys`' seeds, in the request's language; `None` in zh-CN or for
/// anything else.
pub fn shown(stored: &str, keys: &[&str]) -> Option<String> {
    steve_i18n::reword!(stored, keys)
}

/// A value from a page that is one of `keys`' seeds in another language, as the seed is stored; `None`
/// for anything else.
pub fn stored(sent: &str, keys: &[&str]) -> Option<String> {
    steve_i18n::unword!(sent, keys)
}

/// [`shown`] over the string `fields` of `o`, in place.
pub fn show(o: &mut Map<String, Value>, fields: &[(&str, &[&str])]) {
    each(o, fields, shown)
}

/// [`stored`] over the string `fields` of `o`, in place.
pub fn store(o: &mut Map<String, Value>, fields: &[(&str, &[&str])]) {
    each(o, fields, stored)
}

fn each(
    o: &mut Map<String, Value>,
    fields: &[(&str, &[&str])],
    f: fn(&str, &[&str]) -> Option<String>,
) {
    for (field, keys) in fields {
        if let Some(Value::String(s)) = o.get_mut(*field)
            && let Some(x) = f(s, keys)
        {
            *s = x;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn en<R>(f: impl FnOnce() -> R) -> R {
        steve_i18n::with(steve_i18n::EN, f)
    }

    #[test]
    fn a_seeded_value_reads_in_the_language_and_goes_back_as_seeded() {
        let seed = json!({"name": "Steve", "role": "你的助手：理解需求、分派、汇报", "persona": "沉稳、简洁，先说结论；不客套", "rules": text(STEVE_RULES)});
        let mut zh = seed.as_object().unwrap().clone();
        show(&mut zh, BOT);
        assert_eq!(Value::Object(zh), seed, "zh-CN is byte-identical");
        let mut shown_en = seed.as_object().unwrap().clone();
        en(|| show(&mut shown_en, BOT));
        assert_eq!(shown_en["name"], "Steve");
        assert_eq!(
            shown_en["role"],
            "Your assistant: understands requests, hands out work, reports back"
        );
        assert!(!shown_en["rules"].as_str().unwrap().contains('。'));
        let mut back = shown_en.clone();
        en(|| store(&mut back, BOT));
        assert_eq!(Value::Object(back), seed);
    }

    #[test]
    fn an_edited_value_is_left_as_written() {
        for v in ["我的助手", "My helper", "你的助手：理解需求、分派、汇报。"] {
            assert_eq!(en(|| shown(v, &[STEVE_ROLE])), None);
            assert_eq!(en(|| stored(v, &[STEVE_ROLE])), None);
            assert_eq!(stored(v, &[STEVE_ROLE]), None);
        }
    }

    #[test]
    fn the_persona_intro_keeps_the_owner() {
        let zh = persona_intro("Alice");
        assert_eq!(zh, "我是 Steve，Alice 的 AI 助手，帮他接收和分派事情。");
        let e = en(|| shown(&zh, &[PERSONA_INTRO])).unwrap();
        assert!(e.contains("Alice") && e.is_ascii(), "{e}");
        assert_eq!(
            en(|| stored(&e, &[PERSONA_INTRO])).as_deref(),
            Some(zh.as_str())
        );
        assert_eq!(
            en(|| persona_intro("Alice")),
            zh,
            "stored in zh-CN whatever the request"
        );
    }

    #[test]
    fn the_seeds_are_what_installations_have() {
        assert_eq!(text(WORKSPACE_NAME), "个人");
        assert_eq!(text(STEVE_ROLE), "你的助手：理解需求、分派、汇报");
        assert_eq!(text(DEV_PERSONA), "直接、话少");
        assert_eq!(
            text(STEVE_PROJECT),
            "Steve 自己的代码：Rust Hub、Web 界面、发布与运维脚本"
        );
    }
}
