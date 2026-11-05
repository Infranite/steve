//! Tokenizing for the `kb_fts` index.
//!
//! SQLite's `unicode61` tokenizer treats a whole run of Han characters as one token, so a
//! search for “部署” would never find “部署脚本”. Instead of a dictionary segmenter (jieba keeps
//! tens of MB resident) both the index and the query go through [`seg`], which expands each run
//! of CJK characters into overlapping bigrams — Lucene's `CJKAnalyzer` approach. Everything
//! else is left to `unicode61`, which is why the FTS table keeps that tokenizer.

use unicode_normalization::UnicodeNormalization;

/// Han, kana and hangul: scripts written without spaces between words.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF        // CJK Extension A
        | 0x4E00..=0x9FFF      // CJK Unified Ideographs
        | 0xF900..=0xFAFF      // CJK Compatibility Ideographs
        | 0x20000..=0x2EBEF    // Extensions B–F
        | 0x2F800..=0x2FA1F    // Compatibility Supplement
        | 0x30000..=0x3134F    // Extension G
        | 0x3005..=0x3007      // 々 〆 〇
        | 0x3040..=0x309F      // Hiragana
        | 0x30A0..=0x30FF      // Katakana (incl. ー)
        | 0x31F0..=0x31FF      // Katakana phonetic extensions
        | 0x1100..=0x11FF      // Hangul Jamo
        | 0x3130..=0x318F      // Hangul compatibility Jamo
        | 0xAC00..=0xD7AF      // Hangul syllables
    )
}

/// NFKC + lowercase: full-width Latin and digits become ASCII, so “ＡＰＩ” and “api” match.
fn fold(text: &str) -> String {
    text.nfkc().flat_map(char::to_lowercase).collect()
}

/// A folded piece of text: a run of CJK characters, or anything else.
enum Run<'a> {
    Cjk(Vec<char>),
    Other(&'a str),
}

fn runs(folded: &str) -> Vec<Run<'_>> {
    let mut out = Vec::new();
    let mut cjk: Vec<char> = Vec::new();
    let mut other_start: Option<usize> = None;
    for (i, c) in folded.char_indices() {
        if is_cjk(c) {
            if let Some(s) = other_start.take() {
                out.push(Run::Other(&folded[s..i]));
            }
            cjk.push(c);
        } else {
            if !cjk.is_empty() {
                out.push(Run::Cjk(std::mem::take(&mut cjk)));
            }
            other_start.get_or_insert(i);
        }
    }
    if let Some(s) = other_start {
        out.push(Run::Other(&folded[s..]));
    }
    if !cjk.is_empty() {
        out.push(Run::Cjk(cjk));
    }
    out
}

/// Overlapping bigrams of a CJK run; a lone character stays a unigram.
fn bigrams(run: &[char]) -> Vec<String> {
    if run.len() == 1 {
        return vec![run[0].to_string()];
    }
    run.windows(2).map(|w| w.iter().collect()).collect()
}

/// Text as stored in (and matched against) `kb_fts`: “部署脚本 v2” → `部署 署脚 脚本 v2`.
///
/// Non-CJK text passes through unchanged after folding; `unicode61` splits it on punctuation
/// and drops emoji and symbols. Bigrams are separated from neighbours by spaces so `v2部署`
/// yields the tokens `v2` and `部署`.
pub fn seg(text: &str) -> String {
    let folded = fold(text);
    let mut out = String::with_capacity(folded.len() * 2);
    for run in runs(&folded) {
        match run {
            Run::Other(s) if out.ends_with(' ') => out.push_str(s.trim_start_matches(' ')),
            Run::Other(s) => out.push_str(s),
            Run::Cjk(chars) => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push_str(&bigrams(&chars).join(" "));
                out.push(' ');
            }
        }
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out
}

/// One FTS5 string literal; embedded quotes are doubled.
fn quote(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

/// Builds the FTS5 `MATCH` expression for a search text; `None` when nothing searchable
/// remains (empty input, only punctuation or emoji).
///
/// - CJK run of 2–4 characters → one phrase of its bigrams (adjacent bigrams = the original
///   substring); longer runs → each bigram, OR-ed, so long questions still find partial matches.
/// - A single CJK character → prefix `"部"*`, which matches every bigram starting with it.
///   A query that is *only* one character is better served by the `LIKE` fallback on titles and
///   memories (see [`is_single_cjk`]).
/// - Other words (letters/digits): ≥3 characters → prefix `"word"*`, shorter → exact.
///
/// Every term is quoted, so FTS syntax in the input (`AND`, `NEAR`, `*`, `:`, `^`) is inert.
/// Terms are OR-ed; ranking is left to `bm25`.
pub fn fts_query(text: &str) -> Option<String> {
    build_query(text, true)
}

/// Like [`fts_query`] but without phrases: every bigram is its own OR term. A typo in a short
/// word (“部暑脚本”) breaks the strict phrase but still shares bigrams with the original
/// (`脚本`), so retrieval can use this as a second pass when the strict query finds too few
/// results.
pub fn fts_query_relaxed(text: &str) -> Option<String> {
    build_query(text, false)
}

fn build_query(text: &str, phrases: bool) -> Option<String> {
    let folded = fold(text);
    let mut terms: Vec<String> = Vec::new();
    let mut push = |t: String| {
        if !terms.contains(&t) {
            terms.push(t);
        }
    };
    for run in runs(&folded) {
        match run {
            Run::Cjk(chars) => match chars.len() {
                1 => push(format!("{}*", quote(&chars[0].to_string()))),
                2..=4 if phrases => push(quote(&bigrams(&chars).join(" "))),
                _ => bigrams(&chars).into_iter().for_each(|b| push(quote(&b))),
            },
            Run::Other(s) => {
                for word in s.split(|c: char| !c.is_alphanumeric()) {
                    if word.is_empty() {
                        continue;
                    }
                    if word.chars().count() >= 3 {
                        push(format!("{}*", quote(word)));
                    } else {
                        push(quote(word));
                    }
                }
            }
        }
    }
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

/// True when the whole query is one CJK character (after trimming): bigram indexing cannot
/// answer it well, so retrieval falls back to `LIKE` on titles and memory text.
pub fn is_single_cjk(text: &str) -> bool {
    let folded = fold(text);
    let mut it = folded.trim().chars();
    matches!((it.next(), it.next()), (Some(c), None) if is_cjk(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bigrams_for_han_runs() {
        assert_eq!(seg("部署脚本"), "部署 署脚 脚本");
        assert_eq!(seg("部"), "部");
        assert_eq!(seg("部署"), "部署");
        assert_eq!(seg(""), "");
    }

    #[test]
    fn mixed_scripts_and_punctuation() {
        assert_eq!(seg("用 Rust 写部署脚本"), "用 rust 写部 部署 署脚 脚本");
        // NFKC turns the full-width comma into ASCII.
        assert_eq!(seg("v2部署，OK"), "v2 部署 ,ok");
        assert_eq!(seg("合同。续签"), "合同 。 续签");
        assert_eq!(seg("ST-12 交付"), "st-12 交付");
    }

    #[test]
    fn full_width_and_case_fold() {
        assert_eq!(seg("ＡＰＩ　网关"), "api 网关");
        assert_eq!(seg("Ｓｔｅｖｅ１２３"), "steve123");
        // Half-width katakana becomes full-width and is bigrammed.
        assert_eq!(seg("ｶﾀｶﾅ"), "カタ タカ カナ");
    }

    #[test]
    fn kana_hangul_emoji() {
        assert_eq!(seg("ひらがな"), "ひら らが がな");
        assert_eq!(seg("한국어"), "한국 국어");
        // Emoji is not CJK: it splits runs and unicode61 drops it later.
        assert_eq!(seg("部署🚀上线"), "部署 🚀 上线");
        assert_eq!(seg("👍"), "👍");
    }

    #[test]
    fn query_construction() {
        assert_eq!(fts_query("部署").as_deref(), Some("\"部署\""));
        assert_eq!(fts_query("部署脚本").as_deref(), Some("\"部署 署脚 脚本\""));
        assert_eq!(
            fts_query("合同续签时间").as_deref(),
            Some("\"合同\" OR \"同续\" OR \"续签\" OR \"签时\" OR \"时间\"")
        );
        assert_eq!(fts_query("SLA").as_deref(), Some("\"sla\"*"));
        assert_eq!(fts_query("PM 部署").as_deref(), Some("\"pm\" OR \"部署\""));
        assert_eq!(fts_query("部").as_deref(), Some("\"部\"*"));
    }

    #[test]
    fn relaxed_query_has_no_phrases() {
        assert_eq!(
            fts_query_relaxed("部暑脚本 SLA").as_deref(),
            Some("\"部暑\" OR \"暑脚\" OR \"脚本\" OR \"sla\"*")
        );
        assert_eq!(fts_query_relaxed("部署").as_deref(), Some("\"部署\""));
    }

    #[test]
    fn query_neutralizes_syntax() {
        assert_eq!(
            fts_query("a AND b NEAR(c) \"x\" title:y ^z *").as_deref(),
            Some(
                "\"a\" OR \"and\"* OR \"b\" OR \"near\"* OR \"c\" OR \"x\" OR \"title\"* OR \"y\" OR \"z\""
            )
        );
        assert_eq!(fts_query("  ，。!? 🚀 "), None);
        assert_eq!(fts_query(""), None);
        // Duplicate terms collapse.
        assert_eq!(fts_query("部署 部署").as_deref(), Some("\"部署\""));
    }

    #[test]
    fn single_char_detection() {
        assert!(is_single_cjk(" 部 "));
        assert!(!is_single_cjk("部署"));
        assert!(!is_single_cjk("a"));
        assert!(!is_single_cjk(""));
    }
}
