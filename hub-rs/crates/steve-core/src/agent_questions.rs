//! Preserve recommendations on question cards (`steve_ask`).

/// The card's draft for the agent's leaning: `推荐：<recommended>\n理由：<why>` (none without a
/// recommendation).
pub fn hint_draft(recommended: Option<&str>, why: Option<&str>) -> Option<String> {
    let rec = recommended.map(str::trim).filter(|r| !r.is_empty())?;
    let mut d = format!("推荐：{rec}");
    if let Some(w) = why.map(str::trim).filter(|w| !w.is_empty()) {
        d.push_str(&format!("\n理由：{w}"));
    }
    Some(d)
}
