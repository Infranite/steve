//! `table`: a decision table `{hit:"first"|"collect", inputs:[path], outputs:[key], rows:[[cell]]}`.
//!
//! Input cells: empty or `-` = anything; `<= 500`, `< 0`, `> 0`, `>= 1`, `== x`, `!= x`;
//! `"交通", "餐饮"` = one of the listed values; anything else = equal to that literal. Output cells
//! are literals (`"auto"`, `12`, `true`; unquoted text is taken as it is). `first` outputs the first
//! matching row as `{key: value}`; `collect` outputs `{key: [value…]}` over all matching rows. The
//! branch is the first output value of the first matching row.

use async_trait::async_trait;
use serde_json::{Map, Value};

use super::{NodeKind, NodeOutcome, RunCx, str_list};
use crate::expr::{self, compare, loose_eq};

pub struct Table;

pub const MAX_ROWS: usize = 200;
pub const MAX_COLS: usize = 20;

/// A parsed input cell.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Any,
    Cmp(&'static str, Value),
    OneOf(Vec<Value>),
}

/// A literal: quoted text, a number, `true`/`false`/`null`, else the text as it is.
pub fn literal(s: &str) -> Value {
    let s = s.trim();
    if let Some(q) = strip_quotes(s) {
        return Value::String(q.to_string());
    }
    match s {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" => return Value::Null,
        _ => {}
    }
    if let Ok(n) = s.parse::<f64>()
        && let Some(n) = serde_json::Number::from_f64(n)
    {
        return Value::Number(n);
    }
    Value::String(s.to_string())
}

fn strip_quotes(s: &str) -> Option<&str> {
    for (a, b) in [('"', '"'), ('\'', '\''), ('“', '”'), ('「', '」')] {
        if s.len() >= a.len_utf8() + b.len_utf8() && s.starts_with(a) && s.ends_with(b) {
            return Some(&s[a.len_utf8()..s.len() - b.len_utf8()]);
        }
    }
    None
}

/// Splits `"a", "b"` on commas outside quotes.
fn split_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match (quote, c) {
            (None, '"' | '\'' | '“' | '「') => {
                quote = Some(match c {
                    '“' => '”',
                    '「' => '」',
                    q => q,
                });
                cur.push(c);
            }
            (Some(q), _) if c == q => {
                quote = None;
                cur.push(c);
            }
            (None, ',' | '，') => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().map(|x| x.trim().to_string()).collect()
}

pub fn parse_cell(s: &str) -> Result<Cell, String> {
    let s = s.trim();
    if s.is_empty() || s == "-" {
        return Ok(Cell::Any);
    }
    for op in ["<=", ">=", "==", "!=", "<", ">"] {
        if let Some(rest) = s.strip_prefix(op) {
            let rest = rest.trim();
            if rest.is_empty() {
                return Err(format!("单元格「{s}」不完整"));
            }
            let v = literal(rest);
            if matches!(op, "<" | "<=" | ">" | ">=") && expr::number(&v).is_none() {
                return Err(format!("单元格「{s}」要和数字比较"));
            }
            return Ok(Cell::Cmp(op, v));
        }
    }
    let parts = split_list(s);
    if parts.len() > 1 {
        if parts.iter().any(String::is_empty) {
            return Err(format!("单元格「{s}」的列表写得不对"));
        }
        return Ok(Cell::OneOf(parts.iter().map(|p| literal(p)).collect()));
    }
    Ok(Cell::OneOf(vec![literal(s)]))
}

impl Cell {
    pub fn matches(&self, v: &Value) -> bool {
        use std::cmp::Ordering::*;
        match self {
            Cell::Any => true,
            Cell::OneOf(xs) => xs.iter().any(|x| loose_eq(x, v)),
            Cell::Cmp(op, x) => match *op {
                "==" => loose_eq(v, x),
                "!=" => !loose_eq(v, x),
                "<" => compare(v, x) == Some(Less),
                "<=" => matches!(compare(v, x), Some(Less | Equal)),
                ">" => compare(v, x) == Some(Greater),
                ">=" => matches!(compare(v, x), Some(Greater | Equal)),
                _ => false,
            },
        }
    }
}

fn cell_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Null => Some(String::new()),
        Value::Number(_) | Value::Bool(_) => Some(v.to_string()),
        _ => None,
    }
}

fn rows(cfg: &Value) -> Result<Vec<Vec<String>>, String> {
    match cfg.get("rows") {
        Some(Value::Array(rs)) => rs
            .iter()
            .map(|r| {
                r.as_array()
                    .ok_or_else(|| "每一行要是列表".to_string())?
                    .iter()
                    .map(|c| cell_text(c).ok_or_else(|| "单元格只能是文字".to_string()))
                    .collect()
            })
            .collect(),
        _ => Err("要有至少一行".into()),
    }
}

#[async_trait]
impl NodeKind for Table {
    fn kind(&self) -> &'static str {
        "table"
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        match cfg["hit"].as_str().unwrap_or("first") {
            "first" | "collect" => {}
            _ => return Err("hit 只能是 first 或 collect".into()),
        }
        let inputs = str_list(cfg, "inputs")?;
        let outputs = str_list(cfg, "outputs")?;
        if inputs.is_empty() || outputs.is_empty() {
            return Err("要有输入列和输出列".into());
        }
        if inputs.len() + outputs.len() > MAX_COLS {
            return Err(format!("最多 {MAX_COLS} 列"));
        }
        if let Some(p) = inputs.iter().find(|p| !expr::valid_path(p)) {
            return Err(format!("输入列「{p}」写得不对"));
        }
        if outputs.iter().any(|k| k.trim().is_empty()) {
            return Err("输出列要有名字".into());
        }
        let rs = rows(cfg)?;
        if rs.is_empty() {
            return Err("要有至少一行".into());
        }
        if rs.len() > MAX_ROWS {
            return Err(format!("最多 {MAX_ROWS} 行"));
        }
        for (i, r) in rs.iter().enumerate() {
            if r.len() != inputs.len() + outputs.len() {
                return Err(format!("第 {} 行的列数不对", i + 1));
            }
            for c in &r[..inputs.len()] {
                parse_cell(c).map_err(|e| format!("第 {} 行：{e}", i + 1))?;
            }
        }
        Ok(())
    }

    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let (Ok(inputs), Ok(outputs), Ok(rs)) =
            (str_list(cfg, "inputs"), str_list(cfg, "outputs"), rows(cfg))
        else {
            return NodeOutcome::Fail("决策表的配置不对".into());
        };
        let scope = cx.scope(input);
        let values: Vec<Value> = inputs.iter().map(|p| scope.get(p)).collect();
        let collect = cfg["hit"] == "collect";
        let mut out = Map::new();
        let mut branch: Option<String> = None;
        for r in &rs {
            let hit = r[..inputs.len()]
                .iter()
                .zip(&values)
                .all(|(c, v)| parse_cell(c).is_ok_and(|c| c.matches(v)));
            if !hit {
                continue;
            }
            let outs: Vec<Value> = r[inputs.len()..].iter().map(|c| literal(c)).collect();
            if branch.is_none() {
                branch = Some(expr::text(&outs[0]));
            }
            for (k, v) in outputs.iter().zip(outs) {
                if collect {
                    let e = out
                        .entry(k.clone())
                        .or_insert_with(|| Value::Array(Vec::new()));
                    if let Value::Array(a) = e {
                        a.push(v);
                    }
                } else {
                    out.insert(k.clone(), v);
                }
            }
            if !collect {
                break;
            }
        }
        match branch {
            Some(b) => NodeOutcome::branch(Value::Object(out), b),
            None => NodeOutcome::Fail("决策表没有命中任何一行".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cells() {
        assert_eq!(parse_cell("").unwrap(), Cell::Any);
        assert!(parse_cell("<= 500").unwrap().matches(&json!(86.5)));
        assert!(!parse_cell("<= 500").unwrap().matches(&json!(501)));
        assert!(
            parse_cell("\"交通\", \"餐饮\"")
                .unwrap()
                .matches(&json!("餐饮"))
        );
        assert!(
            !parse_cell("\"交通\", \"餐饮\"")
                .unwrap()
                .matches(&json!("住宿"))
        );
        assert!(parse_cell("auto").unwrap().matches(&json!("auto")));
        assert!(parse_cell("< x").is_err());
        assert_eq!(literal("\"auto\""), json!("auto"));
        assert_eq!(literal("12"), json!(12.0));
    }
}
