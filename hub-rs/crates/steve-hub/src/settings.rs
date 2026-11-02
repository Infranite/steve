//! The workspace settings schema (changes.ts `SETTINGS`), served by `GET /api/settings/schema` and used to
//! validate `PATCH /api/settings`.
//!
//! Kept as the exact `JSON.stringify(SETTINGS)` output of the TS Hub, plus `general.tz` (workspace time zone
//! for schedules, steve-rs-plan/modules/schedules.md; not in the TS Hub yet): JS orders integer-like keys first
//! (`labels` of `awayH` comes out `0, 2, 4, 8`), and copying the bytes is the only way not to drift.

use std::sync::LazyLock;

use serde_json::{Map, Value};

pub const SETTINGS_JSON: &str = r#"{"general":[{"key":"lang","label":"语言","values":["简体中文","English"],"def":"简体中文","live":false},{"key":"home","label":"打开时进入","values":["对话","待处理","事项"],"def":"对话","live":true},{"key":"awayH","label":"离开期间汇报","values":["2","4","8","0"],"def":"4","live":true,"labels":{"0":"不汇报","2":"2 小时","4":"4 小时","8":"8 小时"}},{"key":"tz","label":"时区","values":["Asia/Taipei","Asia/Shanghai","Asia/Tokyo","UTC","Europe/London","America/Los_Angeles","America/New_York"],"def":"Asia/Taipei","live":true}],"items":[{"key":"accDefault","label":"默认验收方式","values":["manual","auto"],"def":"manual","live":true,"labels":{"manual":"需要你验收","auto":"自动验收"}},{"key":"depsAfter","label":"前置完成后","values":["自动开始","提醒我开始"],"def":"自动开始","live":true},{"key":"handoff","label":"交接阈值","values":["70","80","90"],"def":"80","live":true,"labels":{"70":"70%","80":"80%","90":"90%"}},{"key":"archDays","label":"完成后自动归档","values":["7","30","90","0"],"def":"30","live":true,"labels":{"0":"不自动归档","7":"7 天","30":"30 天","90":"90 天"}}],"notify":[{"key":"quiet","label":"免打扰","values":["23:00 – 08:00","22:00 – 09:00","关闭"],"def":"23:00 – 08:00","live":false},{"key":"digestAt","label":"每天汇报","values":["08:30","09:00","不发"],"def":"08:30","live":false}],"look":[{"key":"fontSize","label":"字号","values":["标准","大"],"def":"标准","live":true},{"key":"reduceMotion","label":"减弱动态效果","values":["开","关"],"def":"关","live":true}],"data":[{"key":"keepSess","label":"会话记录保留","values":["90 天","1 年","永久"],"def":"永久","live":false},{"key":"keepRaw","label":"事件与消息原文保留","values":["7","30","90"],"def":"7","live":true,"labels":{"7":"7 天","30":"30 天","90":"90 天"}},{"key":"keepMail","label":"本地邮件正文保留","values":["30","90","365"],"def":"90","live":true,"labels":{"30":"30 天","90":"90 天","365":"1 年"}}]}"#;

/// One setting's key and allowed values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub key: String,
    pub values: Vec<String>,
    pub def: String,
}

static SPECS: LazyLock<Vec<Spec>> = LazyLock::new(|| {
    let v: Value = serde_json::from_str(SETTINGS_JSON).unwrap_or(Value::Null);
    let strs = |x: &Value| {
        x.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    v.as_object()
        .into_iter()
        .flat_map(|o| o.values())
        .filter_map(Value::as_array)
        .flatten()
        .map(|s| Spec {
            key: s["key"].as_str().unwrap_or_default().to_string(),
            values: strs(&s["values"]),
            def: s["def"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
});

/// Every spec, flattened in schema order.
pub fn specs() -> &'static [Spec] {
    &SPECS
}

/// `settingDefaults()`: `{key: def}` in schema order.
pub fn defaults() -> Map<String, Value> {
    specs()
        .iter()
        .map(|s| (s.key.clone(), Value::String(s.def.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_parses_and_defaults_match_ts() {
        assert_eq!(specs().len(), 15);
        let d = serde_json::to_string(&defaults()).expect("json");
        assert_eq!(
            d,
            r#"{"lang":"简体中文","home":"对话","awayH":"4","tz":"Asia/Taipei","accDefault":"manual","depsAfter":"自动开始","handoff":"80","archDays":"30","quiet":"23:00 – 08:00","digestAt":"08:30","fontSize":"标准","reduceMotion":"关","keepSess":"永久","keepRaw":"7","keepMail":"90"}"#
        );
        // round trip keeps the bytes (preserve_order)
        let v: Value = serde_json::from_str(SETTINGS_JSON).expect("parse");
        assert_eq!(serde_json::to_string(&v).expect("json"), SETTINGS_JSON);
    }
}
