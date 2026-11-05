//! Paths, templates and the condition language of flow nodes.
//!
//! - Paths: `event.payload.from`, `steps.p.amount`, `sender.name`, `input.level`,
//!   `event.attachments[0].url`. A path whose first segment is not one of the roots is read from
//!   `input` (the previous node's output), so a table can say `amount`.
//! - Templates: `{{<path>}}`; strings are inserted as they are, other values as JSON, missing as
//!   nothing. `{{secrets.…}}` is left untouched: only the `http` node expands it (S3), at send time.
//! - Conditions (`filter` rules, `wait` matches): `a == b`, `!=`, `<`, `<=`, `>`, `>=`,
//!   `contains` (case-insensitive for text), `in`, `matches` (regex), `startswith`, `endswith`,
//!   `and`/`&&`, `or`/`||`, `not`/`!`, parentheses; literals `"text"`, `'text'`, numbers, `true`,
//!   `false`, `null`, lists `["a", "b"]`. A bare path is true when it is truthy.

use std::cmp::Ordering;

use serde_json::{Map, Value};

/// The roots a path can start with.
pub const ROOTS: [&str; 4] = ["event", "steps", "sender", "input"];

/// What paths are read from.
#[derive(Debug, Clone, Default)]
pub struct Scope<'a> {
    pub event: Option<&'a Value>,
    pub steps: Option<&'a Value>,
    pub sender: Option<&'a Value>,
    pub input: Option<&'a Value>,
}

/// One segment of a path.
#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Key(String),
    Index(usize),
}

fn segments(path: &str) -> Option<Vec<Seg>> {
    let mut out = Vec::new();
    for part in path.split('.') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        let (key, mut rest) = match part.find('[') {
            Some(i) => (&part[..i], &part[i..]),
            None => (part, ""),
        };
        if !key.is_empty() {
            out.push(Seg::Key(key.to_string()));
        } else if out.is_empty() {
            return None;
        }
        while !rest.is_empty() {
            let close = rest.find(']')?;
            let n: usize = rest.get(1..close)?.trim().parse().ok()?;
            out.push(Seg::Index(n));
            rest = &rest[close + 1..];
            if !rest.is_empty() && !rest.starts_with('[') {
                return None;
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Is `path` a well-formed path?
pub fn valid_path(path: &str) -> bool {
    segments(path).is_some_and(|s| {
        s.iter().all(|x| match x {
            Seg::Key(k) => k
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '$'),
            Seg::Index(_) => true,
        })
    })
}

fn walk<'v>(mut v: &'v Value, segs: &[Seg]) -> Option<&'v Value> {
    for s in segs {
        v = match (s, v) {
            (Seg::Key(k), Value::Object(m)) => m.get(k)?,
            (Seg::Index(i), Value::Array(a)) => a.get(*i)?,
            _ => return None,
        };
    }
    Some(v)
}

impl Scope<'_> {
    /// The value at `path`, or `Null`.
    pub fn get(&self, path: &str) -> Value {
        let Some(segs) = segments(path) else {
            return Value::Null;
        };
        let root = match &segs[0] {
            Seg::Key(k) => match k.as_str() {
                "event" => Some((self.event, 1)),
                "steps" => Some((self.steps, 1)),
                "sender" => Some((self.sender, 1)),
                "input" => Some((self.input, 1)),
                _ => None,
            },
            Seg::Index(_) => None,
        };
        let (base, skip) = root.unwrap_or((self.input, 0));
        base.and_then(|b| walk(b, &segs[skip..]))
            .cloned()
            .unwrap_or(Value::Null)
    }
}

/// Text of a value for templates and text comparisons.
pub fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        // 1200.0 reads as “1200” (amounts parsed from text are floats)
        Value::Number(n) if n.is_f64() => match n.as_f64() {
            Some(x) if x.fract() == 0.0 && x.abs() < 1e15 => format!("{}", x as i64),
            _ => n.to_string(),
        },
        other => other.to_string(),
    }
}

/// Expands `{{path}}` in `tpl`. `{{secrets.…}}` stays as written.
pub fn render(tpl: &str, scope: &Scope) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(j) = after.find("}}") else {
            out.push_str(&rest[i..]);
            return out;
        };
        let path = after[..j].trim();
        if path.starts_with("secrets.") {
            out.push_str(&rest[i..i + 2 + j + 2]);
        } else {
            out.push_str(&text(&scope.get(path)));
        }
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    out
}

/// Expands templates in every string of a JSON value.
pub fn render_value(v: &Value, scope: &Scope) -> Value {
    match v {
        Value::String(s) => Value::String(render(s, scope)),
        Value::Array(a) => Value::Array(a.iter().map(|x| render_value(x, scope)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), render_value(x, scope)))
                .collect::<Map<_, _>>(),
        ),
        other => other.clone(),
    }
}

/// The paths a template refers to (for validation).
pub fn template_paths(tpl: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = tpl;
    while let Some(i) = rest.find("{{") {
        let after = &rest[i + 2..];
        let Some(j) = after.find("}}") else { break };
        out.push(after[..j].trim().to_string());
        rest = &after[j + 2..];
    }
    out
}

/// A template whose paths are all well-formed, or the error text.
pub fn check_template(tpl: &str) -> Result<(), String> {
    for p in template_paths(tpl) {
        if !valid_path(&p) {
            return Err(steve_i18n::t!("flow.expr.bad_template", path = p));
        }
    }
    Ok(())
}

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// A number, also from numeric text (“86.50”).
pub fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Equality with numbers compared by value and number-vs-text by the number.
pub fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(_), _) | (_, Value::Number(_)) => match (number(a), number(b)) {
            (Some(x), Some(y)) => (x - y).abs() < 1e-9,
            _ => a == b,
        },
        _ => a == b,
    }
}

/// Ordering of numbers (or numeric text), else of text.
pub fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    match (number(a), number(b)) {
        (Some(x), Some(y)) => x.partial_cmp(&y),
        _ => match (a, b) {
            (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
            _ => None,
        },
    }
}

// ---------- the condition language ----------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Path(String),
    Lit(Value),
    Op(&'static str),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
}

const WORD_OPS: [&str; 8] = [
    "and",
    "or",
    "not",
    "contains",
    "in",
    "matches",
    "startswith",
    "endswith",
];

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let cs: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let two: String = cs[i..(i + 2).min(cs.len())].iter().collect();
        let op2 = match two.as_str() {
            "==" => Some("=="),
            "!=" => Some("!="),
            "<=" => Some("<="),
            ">=" => Some(">="),
            "&&" => Some("and"),
            "||" => Some("or"),
            _ => None,
        };
        if let Some(op) = op2 {
            out.push(Tok::Op(op));
            i += 2;
            continue;
        }
        match c {
            '<' => out.push(Tok::Op("<")),
            '>' => out.push(Tok::Op(">")),
            '!' => out.push(Tok::Op("not")),
            '(' => out.push(Tok::LParen),
            ')' => out.push(Tok::RParen),
            '[' => out.push(Tok::LBracket),
            ']' => out.push(Tok::RBracket),
            ',' => out.push(Tok::Comma),
            '"' | '\'' | '“' | '「' => {
                let close = match c {
                    '“' => '”',
                    '「' => '」',
                    q => q,
                };
                let mut s = String::new();
                i += 1;
                loop {
                    let Some(&ch) = cs.get(i) else {
                        return Err(steve_i18n::t!("flow.expr.unclosed_quote"));
                    };
                    if ch == '\\' && cs.get(i + 1).is_some() {
                        s.push(cs[i + 1]);
                        i += 2;
                        continue;
                    }
                    if ch == close {
                        break;
                    }
                    s.push(ch);
                    i += 1;
                }
                out.push(Tok::Lit(Value::String(s)));
            }
            _ if c.is_ascii_digit()
                || (c == '-' && cs.get(i + 1).is_some_and(|d| d.is_ascii_digit())) =>
            {
                let start = i;
                i += 1;
                while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                    i += 1;
                }
                let s: String = cs[start..i].iter().collect();
                let n: f64 = s
                    .parse()
                    .map_err(|_| steve_i18n::t!("flow.expr.bad_number", number = s))?;
                out.push(Tok::Lit(
                    serde_json::Number::from_f64(n)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                ));
                continue;
            }
            _ if c.is_alphanumeric() || c == '_' || c == '$' => {
                let start = i;
                // a path: word chars, dots and [n] indexes
                while i < cs.len() {
                    let ch = cs[i];
                    if ch.is_alphanumeric() || ch == '_' || ch == '$' || ch == '.' || ch == '-' {
                        i += 1;
                    } else if ch == '['
                        && cs.get(i + 1).is_some_and(|d| d.is_ascii_digit())
                        && i > start
                    {
                        while i < cs.len() && cs[i] != ']' {
                            i += 1;
                        }
                        i += 1;
                    } else {
                        break;
                    }
                }
                let w: String = cs[start..i.min(cs.len())].iter().collect();
                let lower = w.to_ascii_lowercase();
                if let Some(op) = WORD_OPS.iter().find(|o| **o == lower) {
                    out.push(Tok::Op(op));
                } else {
                    match lower.as_str() {
                        "true" => out.push(Tok::Lit(Value::Bool(true))),
                        "false" => out.push(Tok::Lit(Value::Bool(false))),
                        "null" => out.push(Tok::Lit(Value::Null)),
                        _ if valid_path(&w) => out.push(Tok::Path(w)),
                        _ => return Err(steve_i18n::t!("flow.expr.bad_word", word = w)),
                    }
                }
                continue;
            }
            other => return Err(steve_i18n::t!("flow.expr.bad_symbol", symbol = other)),
        }
        i += 1;
    }
    Ok(out)
}

/// A parsed condition.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Path(String),
    Lit(Value),
    List(Vec<Expr>),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Cmp(&'static str, Box<Expr>, Box<Expr>),
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

const CMP_OPS: [&str; 11] = [
    "==",
    "!=",
    "<",
    "<=",
    ">",
    ">=",
    "contains",
    "in",
    "matches",
    "startswith",
    "endswith",
];

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut l = self.and()?;
        while self.peek() == Some(&Tok::Op("or")) {
            self.pos += 1;
            l = Expr::Or(Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut l = self.not()?;
        while self.peek() == Some(&Tok::Op("and")) {
            self.pos += 1;
            l = Expr::And(Box::new(l), Box::new(self.not()?));
        }
        Ok(l)
    }
    fn not(&mut self) -> Result<Expr, String> {
        if self.peek() == Some(&Tok::Op("not")) {
            self.pos += 1;
            // `not in` / `not contains` are not operators; `not x` negates
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let l = self.atom()?;
        if let Some(Tok::Op(op)) = self.peek().cloned()
            && CMP_OPS.contains(&op)
        {
            self.pos += 1;
            let r = self.atom()?;
            if op == "matches"
                && let Expr::Lit(Value::String(p)) = &r
            {
                regex::Regex::new(p)
                    .map_err(|_| steve_i18n::t!("flow.expr.bad_regex", regex = p))?;
            }
            return Ok(Expr::Cmp(op, Box::new(l), Box::new(r)));
        }
        Ok(l)
    }
    fn atom(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Path(p)) => Ok(Expr::Path(p)),
            Some(Tok::Lit(v)) => Ok(Expr::Lit(v)),
            Some(Tok::LParen) => {
                let e = self.or()?;
                match self.next() {
                    Some(Tok::RParen) => Ok(e),
                    _ => Err(steve_i18n::t!("flow.expr.unclosed_paren")),
                }
            }
            Some(Tok::LBracket) => {
                let mut xs = Vec::new();
                if self.peek() == Some(&Tok::RBracket) {
                    self.pos += 1;
                    return Ok(Expr::List(xs));
                }
                loop {
                    xs.push(self.atom()?);
                    match self.next() {
                        Some(Tok::Comma) => continue,
                        Some(Tok::RBracket) => break,
                        _ => return Err(steve_i18n::t!("flow.expr.unclosed_list")),
                    }
                }
                Ok(Expr::List(xs))
            }
            _ => Err(steve_i18n::t!("flow.expr.incomplete")),
        }
    }
}

/// Parses a condition; the error, in the request's language, goes into the `400` as it is.
pub fn parse(src: &str) -> Result<Expr, String> {
    if src.trim().is_empty() {
        return Err(steve_i18n::t!("flow.expr.empty"));
    }
    if src.chars().count() > 2000 {
        return Err(steve_i18n::t!("flow.expr.too_long"));
    }
    let mut p = Parser {
        toks: lex(src)?,
        pos: 0,
    };
    let e = p.or()?;
    if p.pos != p.toks.len() {
        return Err(steve_i18n::t!("flow.expr.trailing"));
    }
    Ok(e)
}

fn value(e: &Expr, scope: &Scope) -> Value {
    match e {
        Expr::Path(p) => scope.get(p),
        Expr::Lit(v) => v.clone(),
        Expr::List(xs) => Value::Array(xs.iter().map(|x| value(x, scope)).collect()),
        other => Value::Bool(eval_expr(other, scope)),
    }
}

fn contains(hay: &Value, needle: &Value) -> bool {
    match hay {
        Value::String(s) => s.to_lowercase().contains(&text(needle).to_lowercase()),
        Value::Array(a) => a.iter().any(|x| loose_eq(x, needle)),
        Value::Object(m) => m.contains_key(&text(needle)),
        _ => false,
    }
}

/// Evaluates a parsed condition.
pub fn eval_expr(e: &Expr, scope: &Scope) -> bool {
    match e {
        Expr::Not(x) => !eval_expr(x, scope),
        Expr::And(a, b) => eval_expr(a, scope) && eval_expr(b, scope),
        Expr::Or(a, b) => eval_expr(a, scope) || eval_expr(b, scope),
        Expr::Cmp(op, a, b) => {
            let (l, r) = (value(a, scope), value(b, scope));
            match *op {
                "==" => loose_eq(&l, &r),
                "!=" => !loose_eq(&l, &r),
                "<" => compare(&l, &r) == Some(Ordering::Less),
                "<=" => matches!(compare(&l, &r), Some(Ordering::Less | Ordering::Equal)),
                ">" => compare(&l, &r) == Some(Ordering::Greater),
                ">=" => matches!(compare(&l, &r), Some(Ordering::Greater | Ordering::Equal)),
                "contains" => contains(&l, &r),
                "in" => contains(&r, &l),
                "matches" => regex::RegexBuilder::new(&text(&r))
                    .size_limit(1 << 20)
                    .build()
                    .is_ok_and(|re| re.is_match(&text(&l))),
                "startswith" => text(&l).starts_with(&text(&r)),
                "endswith" => text(&l).ends_with(&text(&r)),
                _ => false,
            }
        }
        other => truthy(&value(other, scope)),
    }
}

/// Parses and evaluates; a condition that does not parse is false.
pub fn eval(src: &str, scope: &Scope) -> bool {
    parse(src).is_ok_and(|e| eval_expr(&e, scope))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paths_and_templates() {
        let ev = json!({"payload": {"from": "noreply@github.com", "attachments": [{"url": "u0"}]}});
        let steps = json!({"p": {"amount": 86.5, "category": "交通"}});
        let input = json!({"amount": 86.5});
        let s = Scope {
            event: Some(&ev),
            steps: Some(&steps),
            input: Some(&input),
            ..Scope::default()
        };
        assert_eq!(s.get("event.payload.attachments[0].url"), json!("u0"));
        assert_eq!(s.get("amount"), json!(86.5));
        assert_eq!(s.get("steps.p.category"), json!("交通"));
        assert_eq!(s.get("nope.x"), Value::Null);
        assert_eq!(
            render(
                "{ \"amount\": {{steps.p.amount}}, \"c\": \"{{ steps.p.category }}\", \"k\": \"{{secrets.vi_1.token}}\" }",
                &s
            ),
            "{ \"amount\": 86.5, \"c\": \"交通\", \"k\": \"{{secrets.vi_1.token}}\" }"
        );
        assert!(check_template("{{a..b}}").is_err());
    }

    #[test]
    fn conditions() {
        let ev = json!({"source": "mail", "group_key": "acme", "payload": {"from": "NoReply@GitHub.com", "to": ["a", "b"]}});
        let input = json!({"amount": "86.50", "category": "交通"});
        let s = Scope {
            event: Some(&ev),
            input: Some(&input),
            ..Scope::default()
        };
        assert!(eval("event.group_key == \"acme\"", &s));
        assert!(eval("event.payload.from contains 'noreply'", &s));
        assert!(eval(
            "event.payload.from matches \"(?i)noreply|github\" and not (amount > 500)",
            &s
        ));
        assert!(eval("category in [\"交通\", \"餐饮\"]", &s));
        assert!(eval("amount <= 500 && event.payload.to contains \"b\"", &s));
        assert!(!eval("event.missing", &s));
        assert!(eval("!event.missing || false", &s));
        assert!(parse("a ==").is_err());
        assert!(parse("(a == 1").is_err());
        assert!(parse("a matches \"(\"").is_err());
        assert!(parse("a == 1 b").is_err());
    }

    #[tokio::test]
    async fn errors_in_english() {
        let en = steve_i18n::scope(steve_i18n::EN, async {
            (
                parse("").unwrap_err(),
                check_template("{{a..b}}").unwrap_err(),
            )
        })
        .await;
        assert_eq!(en.0, "The condition is empty");
        assert_eq!(en.1, "“{{a..b}}” in the template is malformed");
        assert_eq!(parse("").unwrap_err(), "条件是空的");
        assert_eq!(
            check_template("{{a..b}}").unwrap_err(),
            "模板里的「{{a..b}}」写得不对"
        );
    }
}
