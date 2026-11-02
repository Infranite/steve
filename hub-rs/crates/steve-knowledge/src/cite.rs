//! Citations in Steve's main-thread answers (knowledge.md §4.3): the answer marks the knowledge it
//! used with `[n]`; marks to numbers that were never given are removed (the model must not invent
//! sources), and the reply's `meta.cites` lists only what was actually cited.

use steve_modules::context::Cite;

/// The answer after [`finish`].
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    /// The answer without marks to numbers that don't exist.
    pub text: String,
    /// The cited entries, by number, each once.
    pub used: Vec<Cite>,
}

/// One `[n]` mark: byte range in the answer and its number. A mark is 1–2 ASCII digits in square
/// brackets, not followed by `(` (that is a Markdown link `[1](…)`, left alone).
fn marks(s: &str) -> Vec<(usize, usize, u32)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'[' {
            let mut j = i + 1;
            while j < b.len() && j - i <= 3 && b[j].is_ascii_digit() {
                j += 1;
            }
            let digits = j - i - 1;
            if (1..=2).contains(&digits)
                && j < b.len()
                && b[j] == b']'
                && b.get(j + 1) != Some(&b'(')
                && let Ok(n) = s[i + 1..j].parse::<u32>()
            {
                out.push((i, j + 1, n));
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// `cite::finish(answer, hits)` (§4.3): drops `[n]` marks whose `n` is not among `cites` (with the
/// spaces right before them, so `用 Rust [1] [9]` becomes `用 Rust [1]`), and returns the cited
/// entries in number order.
pub fn finish(answer: &str, cites: &[Cite]) -> Finished {
    let mut text = String::with_capacity(answer.len());
    let mut used: Vec<Cite> = Vec::new();
    let mut pos = 0;
    for (start, end, n) in marks(answer) {
        match cites.iter().find(|c| c.n == n) {
            Some(c) => {
                text.push_str(&answer[pos..end]);
                if !used.iter().any(|u| u.n == n) {
                    used.push(c.clone());
                }
            }
            None => {
                let kept = answer[pos..start].trim_end_matches([' ', '\t']);
                text.push_str(kept);
            }
        }
        pos = end;
    }
    text.push_str(&answer[pos..]);
    used.sort_by_key(|c| c.n);
    Finished { text, used }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_store::base::Level;

    fn cite(n: u32) -> Cite {
        Cite {
            n,
            label: format!("笔记 · {n}"),
            href: format!("#/knowledge/doc/kd_{n}?seq=0"),
            level: Level::Conclusion,
        }
    }

    #[test]
    fn unknown_numbers_are_removed_and_only_cited_ones_listed() {
        let cs = [cite(1), cite(2)];
        let f = finish("续签在三月 [1]。负责人是 Sarah [3]，见 [1][2]。", &cs);
        assert_eq!(f.text, "续签在三月 [1]。负责人是 Sarah，见 [1][2]。");
        assert_eq!(f.used, vec![cite(1), cite(2)]);

        let f = finish("用 Rust [1] [9]", &[cite(1)]);
        assert_eq!(f.text, "用 Rust [1]");
        assert_eq!(f.used, vec![cite(1)]);
    }

    #[test]
    fn nothing_cited() {
        let f = finish("没有用到知识。", &[cite(1)]);
        assert_eq!(f.text, "没有用到知识。");
        assert!(f.used.is_empty());
        let f = finish("全是编造 [4] [5]", &[]);
        assert_eq!(f.text, "全是编造");
        assert!(f.used.is_empty());
    }

    #[test]
    fn links_long_numbers_and_odd_brackets_stay() {
        let cs = [cite(2)];
        let s = "见 [1](https://x.y) 和 [100] 与 [a] 和 [] [2]";
        let f = finish(s, &cs);
        assert_eq!(f.text, s);
        assert_eq!(f.used, vec![cite(2)]);
        // non-ASCII right after a mark
        let f = finish("好[7]。", &cs);
        assert_eq!(f.text, "好。");
    }
}
