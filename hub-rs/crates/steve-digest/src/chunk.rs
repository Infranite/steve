//! A big window cut into chunks by token budget (not by count), at natural boundaries, with a small overlap.
//!
//! A cut goes where a reply chain or topic ends, else at a long time gap, else where the chain changes, else
//! where the budget runs out; it is looked for in the last part of the chunk ([`SEARCH`]). Each chunk after the
//! first repeats the last few lines of the one before (`overlap`, shown as 上文). A reply chain that spans chunks
//! keeps one id in the packet's facts (`chains`), so its pieces are joined by that id, never by a model.

use crate::facts::Line;

/// A gap this long between two messages is a boundary.
pub const GAP_MS: i64 = 30 * 60 * 1000;
/// The share of a chunk, from its end, where a boundary is looked for.
const SEARCH: f64 = 0.4;

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub n: i64,
    pub first_pos: i64,
    pub last_pos: i64,
    /// Lines repeated from before (the window's context for the first chunk).
    pub overlap: i64,
    pub tokens: i64,
    pub text: String,
}

/// How good a cut before `lines[i]` is.
fn boundary(lines: &[&Line], i: usize) -> u8 {
    let (a, b) = (lines[i - 1], lines[i]);
    let ended = match &a.chain {
        Some(ch) => !lines[i..].iter().any(|l| l.chain.as_ref() == Some(ch)),
        None => true,
    };
    if b.at - a.at >= GAP_MS && ended {
        4
    } else if ended && a.chain.is_some() {
        3
    } else if b.at - a.at >= GAP_MS {
        2
    } else if a.chain != b.chain && ended {
        1
    } else {
        0
    }
}

fn render(header: &str, context: &[&Line], own: &[&Line]) -> String {
    let mut out = String::from(header);
    if !context.is_empty() {
        out.push_str("\n\n上文（只供理解，不要写回）：\n");
        out.push_str(
            &context
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    out.push_str("\n\n本块：\n");
    out.push_str(
        &own.iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    out
}

/// Cuts `lines` (context with positions ≤ 0, then the window) into chunks of at most `budget` tokens each
/// (a single longer message is a chunk of its own). `header(n, of, first, last)` heads each chunk.
pub fn split(
    lines: &[Line],
    budget: i64,
    overlap: usize,
    header: impl Fn(i64, i64, i64, i64) -> String,
) -> Vec<Chunk> {
    let context: Vec<&Line> = lines.iter().filter(|l| l.pos <= 0).collect();
    let own: Vec<&Line> = lines.iter().filter(|l| l.pos > 0).collect();
    if own.is_empty() {
        return Vec::new();
    }
    let budget = budget.max(200);
    // Reserve headers and rendering separators too; discard context before sacrificing new text.
    let widest = own.len() as i64;
    let overhead = crate::normalize::tokens(&header(widest, widest, widest, widest))
        + crate::normalize::tokens("\n\n上文（只供理解，不要写回）：\n\n\n本块：\n")
        + 8;
    let payload = (budget - overhead).max(1);
    let cost = |l: &&Line| l.tokens.max(crate::normalize::tokens(&l.text) + 1);
    let context_for = |s: usize| {
        let mut ctx = if s == 0 {
            context.clone()
        } else {
            own[s.saturating_sub(overlap)..s].to_vec()
        };
        while !ctx.is_empty() && ctx.iter().map(cost).sum::<i64>() + cost(&own[s]) > payload {
            ctx.remove(0);
        }
        ctx
    };
    // ranges [s, e) of `own`
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut s = 0;
    while s < own.len() {
        let before = context_for(s).iter().map(cost).sum::<i64>();
        let mut used = before;
        let mut e = s;
        while e < own.len() && (e == s || used + cost(&own[e]) <= payload) {
            used += cost(&own[e]);
            e += 1;
        }
        if e < own.len() {
            // the best boundary in the last part of [s, e)
            let from = s + 1 + (((e - s) as f64) * (1.0 - SEARCH)) as usize;
            let mut best = (0u8, e);
            for i in from.min(e)..e {
                let b = boundary(&own, i);
                if b >= best.0 && b > 0 {
                    best = (b, i);
                }
            }
            if best.1 > s {
                e = best.1;
            }
        }
        ranges.push((s, e));
        s = e;
    }
    let of = ranges.len() as i64;
    ranges
        .into_iter()
        .enumerate()
        .map(|(i, (s, e))| {
            let ctx = context_for(s);
            let (first, last) = (own[s].pos, own[e - 1].pos);
            let text = render(&header(i as i64 + 1, of, first, last), &ctx, &own[s..e]);
            Chunk {
                n: i as i64 + 1,
                first_pos: first,
                last_pos: last,
                overlap: ctx.len() as i64,
                tokens: crate::normalize::tokens(&text),
                text,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(pos: i64, at: i64, chain: Option<&str>, tokens: i64) -> Line {
        Line {
            pos,
            id: format!("cm_{pos}"),
            at,
            chain: chain.map(str::to_string),
            text: format!("[{pos}] x"),
            body: String::new(),
            tokens,
        }
    }

    #[test]
    fn small_windows_are_one_chunk() {
        let ls = vec![
            line(-1, 0, None, 10),
            line(1, 1, None, 10),
            line(2, 2, None, 10),
        ];
        let cs = split(&ls, 1000, 2, |n, of, a, b| format!("{n}/{of} {a}-{b}"));
        assert_eq!(cs.len(), 1);
        assert_eq!((cs[0].first_pos, cs[0].last_pos, cs[0].overlap), (1, 2, 1));
        assert!(cs[0].text.starts_with("1/1 1-2"));
    }

    #[test]
    fn cuts_where_a_chain_ends_and_overlaps() {
        // 1–4 one chain, 5–8 another; the budget fits about six lines
        let mut ls = Vec::new();
        for p in 1..=8 {
            let ch = if p <= 4 { "t:a" } else { "t:b" };
            ls.push(line(p, p * 1000, Some(ch), 100));
        }
        let cs = split(&ls, 650, 1, |n, of, a, b| format!("{n}/{of} {a}-{b}"));
        assert_eq!(cs.len(), 2, "{cs:?}");
        assert_eq!((cs[0].first_pos, cs[0].last_pos), (1, 4));
        assert_eq!((cs[1].first_pos, cs[1].last_pos, cs[1].overlap), (5, 8, 1));
        assert!(cs[1].text.contains("[4] x"));
    }

    #[test]
    fn a_long_gap_is_a_boundary() {
        let ls: Vec<Line> = (1..=6)
            .map(|p| line(p, if p <= 4 { p } else { p + GAP_MS * 2 }, None, 100))
            .collect();
        let cs = split(&ls, 500, 0, |_, _, _, _| String::new());
        assert_eq!((cs[0].first_pos, cs[0].last_pos), (1, 4), "{cs:?}");
    }
}
