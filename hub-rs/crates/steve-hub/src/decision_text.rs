//! Decision texts the Hub composes from fixed templates and stores in zh-CN (a memory candidate's question
//! and the lines of its draft, why a card closed by itself), in the reader's language on the way out.
//! The templates are this crate's keys whose zh-CN text is what the producers write
//! (`steve_modules::contacts::memory::confirm_question`, the knowledge candidates' draft, the
//! `superseded` reasons); a text that is none of them (a question or draft a model wrote) is shown as
//! stored, and zh-CN gets the stored text unchanged.

use serde_json::Value;

use crate::backend::Obj;

const QUESTIONS: &[&str] = &[
    "hub.decision_q.save_memory",
    "hub.decision_q.save_record",
    "hub.decision_q.remember",
];

/// The lines of a memory candidate's draft.
const DRAFT_LINES: &[&str] = &[
    "hub.mem_draft.type",
    "hub.mem_draft.why",
    "hub.mem_draft.scope",
    "hub.mem_draft.source",
    "hub.mem_draft.words",
    "hub.mem_draft.replaces",
];

/// What a line's values may be: the record, the kind, a chat day.
const WORDS: &[&str] = &[
    "hub.mem_word.memory",
    "hub.mem_word.record",
    "hub.mem_word.preference",
    "hub.mem_word.rule",
    "hub.mem_word.event",
    "hub.mem_word.summary",
    "hub.mem_word.fact",
    "hub.mem_word.chat_day",
];

/// Why a card closed by itself (`result.superseded`).
const SUPERSEDED: &[&str] = &[
    "hub.superseded.working_again",
    "hub.superseded.bot_delivered_or_asked",
    "hub.superseded.dep_accept_undone",
    "hub.superseded.bot_delivered",
    "hub.superseded.node_not_waiting",
    "hub.superseded.deps_changed",
    "hub.superseded.dep_wait_registered",
    "hub.superseded.you_added",
    "hub.superseded.you_dropped",
    "hub.superseded.still_working",
    "hub.superseded.unit_stopped",
];

fn word(v: &str) -> String {
    steve_i18n::reword!(v, WORDS).unwrap_or_else(|| v.to_owned())
}

/// One draft line, its values read as words where they are ones.
fn draft_line(l: &str) -> Option<String> {
    let text = |k: &str, l: &str| String::from(rust_i18n::t!(k, locale = l));
    DRAFT_LINES.iter().find_map(|k| {
        let vals = steve_i18n::template::read(&text(k, steve_i18n::ZH_CN), l)?;
        let vals: Vec<(String, String)> = vals.into_iter().map(|(n, v)| (n, word(&v))).collect();
        Some(steve_i18n::template::fill(
            &text(k, steve_i18n::current()),
            &vals,
        ))
    })
}

/// `d` (a decision as the API shows it) with the texts above in the request's language.
pub fn shown(mut d: Obj) -> Obj {
    if steve_i18n::current() == steve_i18n::ZH_CN {
        return d;
    }
    let text = |d: &Obj, k: &str| d.get(k).and_then(Value::as_str).map(str::to_owned);
    if let Some(q) = text(&d, "question")
        && let Some(s) = steve_i18n::reword!(&q, QUESTIONS)
    {
        d.insert("question".into(), Value::String(s));
        // only a memory candidate's draft is read line by line: a model's draft is left alone
        if let Some(draft) = text(&d, "draft") {
            let lines: Vec<String> = draft
                .split('\n')
                .map(|l| draft_line(l).unwrap_or_else(|| l.to_owned()))
                .collect();
            d.insert("draft".into(), Value::String(lines.join("\n")));
        }
    }
    if let Some(Value::Object(r)) = d.get_mut("result")
        && let Some(Value::String(why)) = r.get("superseded")
        && let Some(s) = steve_i18n::reword!(why, SUPERSEDED)
    {
        r.insert("superseded".into(), Value::String(s));
    }
    d
}

/// [`shown`] over a list.
pub fn all(ds: Vec<Obj>) -> Vec<Obj> {
    ds.into_iter().map(shown).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dec(v: Value) -> Obj {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn a_memory_candidate_reads_in_english() {
        let q = steve_modules::contacts::memory::confirm_question("fact", "CI 不检查格式");
        let d = dec(json!({
            "question": q,
            "draft": "类型：知识条目 · 事实\n为什么值得保留：免得误判\n适用：提交前\n来源：11-12 的对话\n原话：不检查",
            "result": {"superseded": "它又开始工作了"},
        }));
        let en = steve_i18n::with(steve_i18n::EN, || shown(d.clone()));
        assert_eq!(en["question"], "Save as a knowledge entry? CI 不检查格式");
        assert_eq!(
            en["draft"],
            "Type: knowledge entry · fact\nWhy keep it: 免得误判\nApplies when: 提交前\nSource: Conversation on 11-12\nOriginal words: 不检查"
        );
        assert_eq!(en["result"]["superseded"], "It started working again");
        let zh = steve_i18n::with(steve_i18n::ZH_CN, || shown(d.clone()));
        assert_eq!(zh, d);
    }

    #[test]
    fn a_models_text_stays_as_written() {
        let d = dec(json!({
            "question": "要不要先迁移？",
            "draft": "来源：主线",
            "result": {"superseded": "评审不通过"},
        }));
        assert_eq!(steve_i18n::with(steve_i18n::EN, || shown(d.clone())), d);
    }
}
