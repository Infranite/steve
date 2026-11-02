//! `interactive` message content → readable text and a `card` attachment of whitelisted blocks
//! (CONTRACT.md §1–2 of the message-media task).
//!
//! Feishu hands a card over in four shapes, all read here:
//! - **flattened** — what `im.message.receive_v1` and the message API return by default
//!   (`{title, elements: [[{tag…}]]}`, message_content.md “卡片 interactive”);
//! - **raw** — `{json_card: "<json>", card_schema, json_attachment}`, nodes wrapped in `property`
//!   with camelCase fields (larksuite/cli `convert_lib/card.go`);
//! - **user_dsl** — the event's `{user_dsl: "<json>"}` (`card_userdsl.go`), the card as it was
//!   sent, snake_case;
//! - **schema** — the card JSON 1.0 / 2.0 itself (`card_msg_content_type=user_card_content`).
//!
//! Nothing here acts on a card: buttons and forms come out as read-only blocks (`callback` only
//! says Feishu would post something), links survive only as `http(s)`, and an unknown widget keeps
//! whatever text it has. Images inside a card are never fetched: the message resource API refuses
//! cards (code 234043).

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

/// Content larger than this is not parsed.
pub const MAX_INPUT: usize = 512 * 1024;
/// Nesting deeper than this becomes one `unknown` block.
pub const MAX_DEPTH: usize = 12;
/// Blocks in a card, nested ones included.
pub const MAX_BLOCKS: usize = 300;
/// Characters of one text field …
pub const MAX_FIELD: usize = 8_000;
/// … and of the card's whole text.
pub const MAX_TEXT: usize = 30_000;
const MAX_ROWS: usize = 100;
const MAX_COLS: usize = 20;
const MAX_URL: usize = 2_048;
const LANGS: [&str; 3] = ["zh_cn", "en_us", "ja_jp"];

/// A card read from a message.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// The readable text (title, then the body), the message's `text`.
    pub text: String,
    /// The `card` attachment.
    pub attachment: Value,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Flattened,
    Raw,
    UserDsl,
    Schema,
}

impl Format {
    fn code(self) -> &'static str {
        match self {
            Self::Flattened => "flattened",
            Self::Raw => "raw",
            Self::UserDsl => "user_dsl",
            Self::Schema => "schema",
        }
    }
}

/// Reads `interactive` content (the message's `content` string). `mentions` are `(key, name)`.
pub fn parse(content: &str, mentions: &[(String, String)]) -> Card {
    if content.len() > MAX_INPUT {
        return empty(Format::Schema, None, true, "[卡片：内容过大]");
    }
    let outer: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let mut w = Walker::new(mentions);
    let (format, card, hint) = if let Some(s) = outer.get("json_card").and_then(Value::as_str) {
        w.attachment = match &outer["json_attachment"] {
            Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
            v => v.clone(),
        };
        let hint = outer["card_schema"].as_i64();
        (Format::Raw, inner(s), hint)
    } else if let Some(s) = outer.get("user_dsl").and_then(Value::as_str) {
        (Format::UserDsl, inner(s), None)
    } else if outer["elements"].as_array().is_some_and(|a| {
        a.iter().all(Value::is_array)
            && (!a.is_empty()
                || (outer["title"].is_string()
                    && outer.get("header").is_none()
                    && outer.get("body").is_none()
                    && outer.get("schema").is_none()))
    }) {
        (Format::Flattened, outer, None)
    } else {
        (Format::Schema, outer, None)
    };
    if !card.is_object() {
        return empty(format, None, false, "[卡片]");
    }
    if format == Format::Flattened {
        return w.flattened(&card);
    }
    w.schema(format, &card, hint)
}

fn inner(s: &str) -> Value {
    if s.len() > MAX_INPUT {
        return Value::Null;
    }
    serde_json::from_str(s).unwrap_or(Value::Null)
}

fn empty(format: Format, schema: Option<i64>, truncated: bool, text: &str) -> Card {
    Card {
        text: text.into(),
        attachment: json!({
            "type": "card", "format": format.code(), "schema": schema, "title": null,
            "blocks": [], "truncated": truncated,
        }),
    }
}

// ---------------------------------------------------------------------------------------------
// Small readers.

/// A node's fields: the raw format wraps them in `property`.
fn p(v: &Value) -> &Value {
    match v.get("property") {
        Some(x) if x.is_object() => x,
        _ => v,
    }
}

/// The first present field among aliases (camelCase of the raw format, snake_case of the rest).
fn g<'a>(v: &'a Value, keys: &[&str]) -> &'a Value {
    keys.iter()
        .map(|k| &v[*k])
        .find(|x| !x.is_null())
        .unwrap_or(&Value::Null)
}

fn st<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    g(v, keys).as_str().unwrap_or("")
}

fn arr<'a>(v: &'a Value, keys: &[&str]) -> &'a [Value] {
    g(v, keys).as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn tag(v: &Value) -> &str {
    v["tag"].as_str().unwrap_or("")
}

fn cut(s: &str, max: usize) -> (String, bool) {
    match s.char_indices().nth(max) {
        Some((i, _)) => (format!("{}…", &s[..i]), true),
        None => (s.to_string(), false),
    }
}

/// `http(s)` only, ≤2048 characters; anything else is dropped.
pub fn safe_url(u: &str) -> Option<String> {
    let u = u.trim();
    if u.len() > MAX_URL {
        return None;
    }
    let parsed = url::Url::parse(u).ok()?;
    matches!(parsed.scheme(), "http" | "https")
        .then(|| parsed.host_str().is_some_and(|h| !h.is_empty()))
        .filter(|ok| *ok)
        .map(|_| u.to_string())
}

static AT_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<at\b([^>]*?)/?>(?:([^<]*)</at>)?").unwrap());
static AT_ATTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\b(id|ids|user_id|mention_key|mentions_key)=(?:"([^"]*)"|'([^']*)'|([^\s"'>/]+))"#,
    )
    .unwrap()
});
/// Formatting tags of card markdown (`<font color=…>`, `<text_tag>`, `<link>` …): their text stays.
static FORMAT_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"</?(?:font|text_tag|link|local_datetime|number_tag|u|i|b|s|strong|em|del|br)\b[^>]*>",
    )
    .unwrap()
});

// ---------------------------------------------------------------------------------------------
// The walker.

struct Walker<'m> {
    mentions: &'m [(String, String)],
    attachment: Value,
    blocks: usize,
    truncated: bool,
}

impl<'m> Walker<'m> {
    fn new(mentions: &'m [(String, String)]) -> Self {
        Self {
            mentions,
            attachment: Value::Null,
            blocks: 0,
            truncated: false,
        }
    }

    fn field(&mut self, s: &str) -> String {
        let (s, cut_) = cut(s, MAX_FIELD);
        self.truncated |= cut_;
        s
    }

    fn mention_name(&self, key: &str) -> Option<&str> {
        self.mentions
            .iter()
            .find(|(k, _)| !k.is_empty() && k == key)
            .map(|(_, n)| n.as_str())
            .filter(|n| !n.is_empty())
    }

    /// A person's name from the raw format's `json_attachment` (`persons`, `at_users`,
    /// `option_users`), or a mention.
    fn person(&self, id: &str) -> Option<String> {
        if id.is_empty() {
            return None;
        }
        for k in ["persons", "at_users", "option_users"] {
            let u = &self.attachment[k][id];
            if let Some(key) = u["mention_key"].as_str()
                && let Some(n) = self.mention_name(key)
            {
                return Some(n.to_string());
            }
            if let Some(n) = u["content"].as_str().filter(|n| !n.is_empty()) {
                return Some(n.to_string());
            }
        }
        self.mention_name(id).map(str::to_string)
    }

    /// Card markdown as plain markdown: `<at>` resolved to `@名字`, formatting tags dropped.
    fn md(&mut self, s: &str) -> String {
        let s = AT_TAG.replace_all(s, |c: &regex::Captures| {
            let attrs = c.get(1).map_or("", |m| m.as_str());
            let mut ids = Vec::new();
            let mut keys = Vec::new();
            for a in AT_ATTR.captures_iter(attrs) {
                let v = a
                    .get(2)
                    .or(a.get(3))
                    .or(a.get(4))
                    .map_or("", |m| m.as_str());
                match &a[1] {
                    "id" | "ids" | "user_id" => ids.extend(v.split(',').map(str::to_string)),
                    _ => keys.extend(v.split(',').map(str::to_string)),
                }
            }
            let inner = c.get(2).map_or("", |m| m.as_str()).trim();
            if ids.iter().any(|i| i == "all") {
                return "@所有人".to_string();
            }
            let mut out = String::new();
            let n = ids.len().max(keys.len()).max(1);
            for i in 0..n {
                let id = ids.get(i).map_or("", String::as_str);
                let key = keys.get(i).map_or("", String::as_str);
                let name = self
                    .mention_name(key)
                    .map(str::to_string)
                    .or_else(|| self.person(id))
                    .or_else(|| (n == 1 && !inner.is_empty()).then(|| inner.to_string()))
                    .unwrap_or_else(|| {
                        if id.is_empty() {
                            "某人".into()
                        } else {
                            id.into()
                        }
                    });
                out.push('@');
                out.push_str(name.trim_start_matches('@'));
            }
            out
        });
        let s = FORMAT_TAG.replace_all(&s, "");
        self.field(&s)
    }

    /// The text of a text node: a string, `i18nContent` / `i18n`, `content`, inline `elements`,
    /// or `text`.
    fn txt(&mut self, v: &Value, depth: usize) -> String {
        if depth > MAX_DEPTH {
            self.truncated = true;
            return String::new();
        }
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Object(_) => {
                let q = p(v);
                let i18n = g(q, &["i18nContent", "i18n", "i18n_content"]);
                if let Some(t) = LANGS
                    .iter()
                    .find_map(|l| i18n[*l].as_str().filter(|t| !t.is_empty()))
                {
                    t.to_string()
                } else if let Some(t) = q["content"].as_str() {
                    t.to_string()
                } else if let Some(es) = q["elements"].as_array().filter(|a| !a.is_empty()) {
                    es.iter().map(|e| self.inline(e, depth + 1)).collect()
                } else if let Some(t) = q.get("text") {
                    return self.txt(t, depth + 1);
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        };
        if matches!(tag(v), "lark_md" | "markdown") || s.contains('<') {
            self.md(&s)
        } else {
            self.field(&s)
        }
    }

    /// One inline node of raw markdown (`property.elements`) or a flattened paragraph.
    fn inline(&mut self, e: &Value, depth: usize) -> String {
        let q = p(e);
        match tag(e) {
            "text" | "plain_text" | "lark_md" | "markdown" => {
                let t = if q["content"].is_string() {
                    self.txt(q, depth)
                } else {
                    st(q, &["text"]).to_string()
                };
                styled(&t, q)
            }
            "a" | "link" => {
                let t = st(q, &["content", "text"]).to_string();
                let u = [st(q, &["href"]), st(&q["url"], &["url"]), st(q, &["url"])]
                    .into_iter()
                    .find(|u| !u.is_empty())
                    .unwrap_or("");
                match safe_url(u) {
                    Some(u) if !t.is_empty() => format!("[{}]({u})", md_escape(&t)),
                    Some(u) => u,
                    None => t,
                }
            }
            "at" => {
                let id = st(q, &["userID", "user_id"]);
                if id == "all" || id == "@_all" {
                    return "@所有人".into();
                }
                let own = st(q, &["user_name"]);
                let name = self
                    .mention_name(id)
                    .map(str::to_string)
                    .or_else(|| self.person(id))
                    .or_else(|| (!own.is_empty()).then(|| own.to_string()))
                    .unwrap_or_else(|| id.to_string());
                format!("@{name}")
            }
            "at_all" => "@所有人".into(),
            "emoji" | "emotion" => format!("[{}]", st(q, &["key", "emoji_type"])),
            "code_span" => format!("`{}`", st(q, &["content"])),
            "img" | "image" => "[图片]".into(),
            "local_datetime" => st(q, &["fallbackText", "fallback_text"]).to_string(),
            "text_tag" | "number_tag" => {
                let t = self.txt(&q["text"], depth + 1);
                if t.is_empty() {
                    t
                } else {
                    format!("「{t}」")
                }
            }
            "br" => "\n".into(),
            "button" => {
                let t = self.txt(&q["text"], depth + 1);
                format!("[{}]", if t.is_empty() { "按钮" } else { &t })
            }
            _ => self.txt(e, depth + 1),
        }
    }

    // ------------------------------------------------------------------ flattened

    fn flattened(&mut self, card: &Value) -> Card {
        let title = card["title"].as_str().map(|t| self.field(t));
        let mut blocks = Vec::new();
        for para in card["elements"].as_array().into_iter().flatten() {
            if self.blocks >= MAX_BLOCKS {
                self.truncated = true;
                break;
            }
            let mut line = String::new();
            let mut buttons = Vec::new();
            for e in para.as_array().into_iter().flatten() {
                match tag(e) {
                    "button" => {
                        let text = self.field(st(e, &["text"]));
                        buttons.push(json!({
                            "text": text, "style": style_of(e), "disabled": false, "callback": true,
                        }));
                    }
                    "hr" => {
                        self.flush(&mut line, &mut blocks);
                        self.push(&mut blocks, json!({"t": "hr"}));
                    }
                    "img" => {
                        self.flush(&mut line, &mut blocks);
                        self.push(&mut blocks, json!({"t": "image", "alt": "图片"}));
                    }
                    "note" => {
                        self.flush(&mut line, &mut blocks);
                        let t: Vec<String> = arr(e, &["elements"])
                            .iter()
                            .map(|x| self.inline(x, 1))
                            .filter(|t| !t.is_empty())
                            .collect();
                        let t = self.field(&t.join(" "));
                        self.push(&mut blocks, json!({"t": "note", "text": t}));
                    }
                    "select_static"
                    | "select_person"
                    | "overflow"
                    | "date_picker"
                    | "picker_time"
                    | "picker_datetime"
                    | "multi_select_static"
                    | "multi_select_person"
                    | "input"
                    | "checker" => {
                        self.flush(&mut line, &mut blocks);
                        let c = self.control(e, 1);
                        self.push(&mut blocks, c);
                    }
                    _ => {
                        let t = self.inline(e, 1);
                        // a runaway line stops growing at the field limit
                        if line.len() <= MAX_FIELD * 4 {
                            line.push_str(&t);
                        } else {
                            self.truncated = true;
                        }
                    }
                }
            }
            self.flush(&mut line, &mut blocks);
            if !buttons.is_empty() {
                self.push(&mut blocks, json!({"t": "buttons", "items": buttons}));
            }
        }
        self.finish(
            Format::Flattened,
            None,
            title,
            None,
            None,
            Vec::new(),
            blocks,
        )
    }

    fn flush(&mut self, line: &mut String, blocks: &mut Vec<Value>) {
        let t = std::mem::take(line);
        if !t.trim().is_empty() {
            let t = self.field(&t);
            self.push(blocks, json!({"t": "markdown", "text": t}));
        }
    }

    // ------------------------------------------------------------------ schema / raw / user_dsl

    fn schema(&mut self, format: Format, card: &Value, hint: Option<i64>) -> Card {
        // `card_schema`, else the card's own `schema` (`"2.0"`, or a number in the raw format),
        // else a `body` means 2.0
        let declared = hint.or_else(|| match &card["schema"] {
            Value::Number(n) => n.as_f64().map(|f| f as i64),
            Value::String(s) => s.split('.').next().and_then(|x| x.trim().parse().ok()),
            _ => None,
        });
        let schema = match declared {
            Some(n) if n >= 2 => Some(2),
            Some(_) => Some(1),
            None if card["body"].is_object() => Some(2),
            None => Some(1),
        };
        let header = {
            let h = &card["header"];
            if h.is_object() {
                h
            } else {
                LANGS
                    .iter()
                    .map(|l| &card["i18n_header"][*l])
                    .find(|x| x.is_object())
                    .unwrap_or(&Value::Null)
            }
        };
        let hp = p(header);
        let title = Some(self.txt(&hp["title"], 0)).filter(|t| !t.is_empty());
        let subtitle = Some(self.txt(&hp["subtitle"], 0)).filter(|t| !t.is_empty());
        let template = hp["template"].as_str().map(str::to_string);
        let tags: Vec<String> = arr(hp, &["textTagList", "text_tag_list"])
            .iter()
            .map(|t| self.txt(&p(t)["text"], 1))
            .filter(|t| !t.is_empty())
            .collect();
        let elements = {
            let body = p(&card["body"]);
            let e = arr(body, &["elements"]);
            if !e.is_empty() {
                e
            } else if let Some(e) = card["elements"].as_array().filter(|a| !a.is_empty()) {
                e.as_slice()
            } else {
                LANGS
                    .iter()
                    .find_map(|l| {
                        card["i18n_elements"][*l]
                            .as_array()
                            .filter(|a| !a.is_empty())
                    })
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
            }
        };
        let blocks = self.elements(elements, 1);
        self.finish(format, schema, title, subtitle, template, tags, blocks)
    }

    fn elements(&mut self, es: &[Value], depth: usize) -> Vec<Value> {
        let mut out = Vec::new();
        for e in es {
            if !e.is_object() {
                continue;
            }
            if self.blocks >= MAX_BLOCKS {
                self.truncated = true;
                break;
            }
            self.element(e, depth, &mut out);
        }
        out
    }

    /// Every block goes out through here: within [`MAX_BLOCKS`], every string in it (nested ones
    /// too, table cells, button texts) within [`MAX_FIELD`].
    fn push(&mut self, out: &mut Vec<Value>, mut b: Value) {
        if self.blocks >= MAX_BLOCKS {
            self.truncated = true;
            return;
        }
        self.clamp(&mut b);
        self.blocks += 1;
        out.push(b);
    }

    fn clamp(&mut self, v: &mut Value) {
        match v {
            Value::String(s) => {
                if s.chars().nth(MAX_FIELD).is_some() {
                    *s = cut(s, MAX_FIELD).0;
                    self.truncated = true;
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| self.clamp(x)),
            Value::Object(o) => o.values_mut().for_each(|x| self.clamp(x)),
            _ => {}
        }
    }

    fn md_block(&mut self, out: &mut Vec<Value>, t: String) {
        if !t.trim().is_empty() {
            let t = self.field(&t);
            self.push(out, json!({"t": "markdown", "text": t}));
        }
    }

    fn element(&mut self, e: &Value, depth: usize, out: &mut Vec<Value>) {
        let t = tag(e);
        if depth > MAX_DEPTH {
            self.truncated = true;
            self.push(out, json!({"t": "unknown", "tag": t, "text": ""}));
            return;
        }
        let q = p(e);
        match t {
            "plain_text" | "text" | "lark_md" => {
                let s = self.txt(e, depth);
                let s = styled(&s, q);
                self.md_block(out, s);
            }
            "markdown" | "markdown_v1" => {
                let s = if let Some(es) = q["elements"].as_array().filter(|a| !a.is_empty()) {
                    es.iter().map(|x| self.inline(x, depth + 1)).collect()
                } else if let Some(c) = q["content"].as_str() {
                    self.md(c)
                } else if e["fallback"].is_object() {
                    self.txt(&e["fallback"], depth + 1)
                } else {
                    self.txt(q, depth)
                };
                self.md_block(out, s);
            }
            "div" => {
                let mut lines = Vec::new();
                if !q["text"].is_null() {
                    let s = self.txt(&q["text"], depth + 1);
                    let tp = p(&q["text"]);
                    let notation = st(tp, &["text_size"]) == "notation"
                        || st(&tp["textStyle"], &["size"]) == "notation";
                    if !s.is_empty() {
                        if notation {
                            self.push(out, json!({"t": "note", "text": s}));
                        } else {
                            lines.push(s);
                        }
                    }
                }
                for f in arr(q, &["fields"]) {
                    let s = self.txt(&f["text"], depth + 1);
                    if !s.is_empty() {
                        lines.push(s);
                    }
                }
                self.md_block(out, lines.join("\n"));
                if q["extra"].is_object() {
                    self.element(&q["extra"], depth + 1, out);
                }
            }
            "note" => {
                let parts: Vec<String> = arr(q, &["elements"])
                    .iter()
                    .map(|x| self.inline(x, depth + 1))
                    .filter(|s| !s.is_empty())
                    .collect();
                if !parts.is_empty() {
                    self.push(out, json!({"t": "note", "text": parts.join(" ")}));
                }
            }
            "hr" => self.push(out, json!({"t": "hr"})),
            "br" | "card_header" | "custom_icon" | "standard_icon" => {}
            "heading" => {
                let level = q["level"].as_i64().unwrap_or(1).clamp(1, 6);
                let s = match q["content"].as_str() {
                    Some(c) => self.md(c),
                    None => self.txt(q, depth),
                };
                if !s.is_empty() {
                    self.push(out, json!({"t": "heading", "level": level, "text": s}));
                }
            }
            "column_set" => {
                let mut cols = Vec::new();
                for c in arr(q, &["columns"]) {
                    let es = arr(p(c), &["elements"]);
                    let bs = self.elements(es, depth + 1);
                    if !bs.is_empty() {
                        cols.push(bs);
                    }
                }
                let all_buttons =
                    !cols.is_empty() && cols.iter().all(|c| c.iter().all(|b| b["t"] == "buttons"));
                if all_buttons {
                    let items: Vec<Value> = cols
                        .into_iter()
                        .flatten()
                        .flat_map(|b| b["items"].as_array().cloned().unwrap_or_default())
                        .collect();
                    self.push(out, json!({"t": "buttons", "items": items}));
                } else if cols.len() == 1 {
                    out.extend(cols.pop().unwrap_or_default());
                } else if !cols.is_empty() {
                    self.push(out, json!({"t": "columns", "columns": cols}));
                }
            }
            "column" | "repeat" => {
                let bs = self.elements(arr(q, &["elements"]), depth + 1);
                out.extend(bs);
            }
            "button" => {
                let b = self.button(e, depth);
                self.push(out, json!({"t": "buttons", "items": [b]}));
            }
            "actions" | "action" => {
                let mut items = Vec::new();
                for a in arr(q, &["actions"]) {
                    if tag(a) == "button" {
                        items.push(self.button(a, depth + 1));
                    } else {
                        if !items.is_empty() {
                            let it = std::mem::take(&mut items);
                            self.push(out, json!({"t": "buttons", "items": it}));
                        }
                        self.element(a, depth + 1, out);
                    }
                }
                if !items.is_empty() {
                    self.push(out, json!({"t": "buttons", "items": items}));
                }
            }
            "overflow"
            | "select_static"
            | "select_person"
            | "multi_select_static"
            | "multi_select_person"
            | "select_img"
            | "input"
            | "date_picker"
            | "picker_date"
            | "picker_time"
            | "picker_datetime"
            | "checker" => {
                let c = self.control(e, depth);
                self.push(out, c);
            }
            "img" | "image" => {
                let alt = self.image_alt(q, depth);
                self.push(out, json!({"t": "image", "alt": alt}));
            }
            "img_combination" => {
                let n = arr(q, &["imgList", "img_list"]).len();
                self.push(out, json!({"t": "image", "alt": format!("{n} 张图片")}));
            }
            "table" => {
                let b = self.table(q, depth);
                self.push(out, b);
            }
            "chart" => {
                let spec = g(q, &["chartSpec", "chart_spec"]);
                let title = spec["title"]["text"].as_str().unwrap_or("图表");
                let title = self.field(title);
                let n = match &spec["data"] {
                    Value::Array(a) => a.iter().map(|s| arr(s, &["values"]).len()).sum(),
                    d => arr(d, &["values"]).len(),
                };
                let mut b = json!({"t": "chart", "title": title});
                if n > 0 {
                    b["summary"] = json!(format!("{n} 个数据点"));
                }
                self.push(out, b);
            }
            "audio" => self.push(out, json!({"t": "media", "kind": "audio"})),
            "video" => self.push(out, json!({"t": "media", "kind": "video"})),
            "collapsible_panel" => {
                let title = self.txt(&p(&q["header"])["title"], depth + 1);
                let title = if title.is_empty() {
                    "详情".into()
                } else {
                    title
                };
                let expanded = q["expanded"].as_bool().unwrap_or(false);
                let bs = self.elements(arr(q, &["elements"]), depth + 1);
                self.push(
                    out,
                    json!({"t": "panel", "title": title, "expanded": expanded, "blocks": bs}),
                );
            }
            "form" => {
                let bs = self.elements(arr(q, &["elements"]), depth + 1);
                self.push(out, json!({"t": "group", "kind": "form", "blocks": bs}));
            }
            "interactive_container" => {
                let url = open_url(q);
                let bs = self.elements(arr(q, &["elements"]), depth + 1);
                let mut b = json!({"t": "group", "kind": "clickable", "blocks": bs});
                if let Some(u) = url {
                    b["url"] = json!(u);
                }
                self.push(out, b);
            }
            "person" | "person_v1" | "avatar" => {
                let id = st(q, &["userID", "user_id"]);
                let name = self
                    .person(id)
                    .or_else(|| Some(self.txt(&q["notation"], depth + 1)).filter(|s| !s.is_empty()))
                    .unwrap_or_else(|| id.to_string());
                self.md_block(out, name);
            }
            "person_list" => {
                let names: Vec<String> = arr(q, &["persons"])
                    .iter()
                    .map(|x| {
                        let id = st(x, &["id", "user_id"]);
                        self.person(id).unwrap_or_else(|| id.to_string())
                    })
                    .filter(|s| !s.is_empty())
                    .collect();
                self.md_block(out, names.join("、"));
            }
            "at" | "at_all" | "link" | "emoji" | "local_datetime" | "code_span" | "text_tag"
            | "number_tag" => {
                let s = self.inline(e, depth);
                self.md_block(out, s);
            }
            "list" => {
                let mut lines = Vec::new();
                for it in arr(q, &["items"]) {
                    let level = it["level"].as_u64().unwrap_or(0).min(6) as usize;
                    let mark = if st(it, &["type"]) == "ol" {
                        format!("{}.", it["order"].as_u64().unwrap_or(1))
                    } else {
                        "-".into()
                    };
                    let s: String = arr(it, &["elements"])
                        .iter()
                        .map(|x| self.inline(x, depth + 1))
                        .collect();
                    lines.push(format!("{}{mark} {s}", "  ".repeat(level)));
                }
                self.md_block(out, lines.join("\n"));
            }
            "blockquote" => {
                let s = match q["content"].as_str() {
                    Some(c) => self.md(c),
                    None => arr(q, &["elements"])
                        .iter()
                        .map(|x| self.inline(x, depth + 1))
                        .collect(),
                };
                let s = s
                    .lines()
                    .map(|l| format!("> {l}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.md_block(out, s);
            }
            "code_block" => {
                let lang = st(q, &["language"]).to_lowercase();
                let mut code = String::new();
                for line in arr(q, &["contents"]) {
                    for c in arr(line, &["contents"]) {
                        code.push_str(st(c, &["content"]));
                    }
                }
                let code = self.field(&code);
                self.md_block(out, format!("```{lang}\n{code}\n```"));
            }
            "fallback_text" => {
                let s = if q["text"].is_null() {
                    arr(q, &["elements"])
                        .iter()
                        .map(|x| self.inline(x, depth + 1))
                        .collect()
                } else {
                    self.txt(&q["text"], depth + 1)
                };
                self.md_block(out, s);
            }
            other => {
                let mut s = String::new();
                for k in ["content", "text", "title", "label", "placeholder"] {
                    if !q[k].is_null() {
                        s = self.txt(&q[k], depth + 1);
                        if !s.is_empty() {
                            break;
                        }
                    }
                }
                if s.is_empty()
                    && let Some(es) = q["elements"].as_array().filter(|a| !a.is_empty())
                {
                    let bs = self.elements(es, depth + 1);
                    if !bs.is_empty() {
                        out.extend(bs);
                        return;
                    }
                }
                self.push(
                    out,
                    json!({"t": "unknown", "tag": cut(other, 40).0, "text": s}),
                );
            }
        }
    }

    fn image_alt(&mut self, q: &Value, depth: usize) -> String {
        let title = self.txt(&q["title"], depth + 1);
        if !title.is_empty() {
            return title;
        }
        let alt = self.txt(&q["alt"], depth + 1);
        if alt.is_empty() { "图片".into() } else { alt }
    }

    fn button(&mut self, e: &Value, depth: usize) -> Value {
        let q = p(e);
        let text = self.txt(&q["text"], depth + 1);
        let text = if text.is_empty() {
            "按钮".into()
        } else {
            text
        };
        let disabled = q["disabled"].as_bool().unwrap_or(false);
        let url = open_url(q);
        // anything a click would post back: a callback behaviour, a value, a form action
        let callback = q["behaviors"]
            .as_array()
            .is_some_and(|b| b.iter().any(|x| x["type"] != "open_url"))
            || q["actions"]
                .as_array()
                .is_some_and(|b| b.iter().any(|x| x["type"] != "open_url"))
            || !q["value"].is_null()
            || !q["form_action_type"].is_null()
            || !q["formActionType"].is_null()
            || !q["action_type"].is_null()
            || !q["complex_interaction"].is_null()
            || (url.is_none() && !disabled);
        let mut b = json!({
            "text": text, "style": style_of(q), "disabled": disabled, "callback": callback,
        });
        if let Some(u) = url {
            b["url"] = json!(u);
        }
        b
    }

    fn control(&mut self, e: &Value, depth: usize) -> Value {
        let q = p(e);
        let t = tag(e);
        let (kind, text) = match t {
            "overflow" => {
                let opts: Vec<String> = arr(q, &["options"])
                    .iter()
                    .map(|o| self.option_text(o, depth))
                    .filter(|s| !s.is_empty())
                    .collect();
                ("overflow", format!("⋮ {}", opts.join("、")))
            }
            "select_static" | "select_person" | "multi_select_static" | "multi_select_person" => {
                let multi = t.starts_with("multi");
                let mut sel: Vec<String> = arr(q, &["selectedValues", "selected_values"])
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                if let Some(s) = g(q, &["initialOption", "initial_option"]).as_str() {
                    sel.push(s.to_string());
                }
                let opts = arr(q, &["options"]);
                if let Some(i) = g(q, &["initialIndex", "initial_index"]).as_u64()
                    && let Some(v) = opts.get(i as usize).and_then(|o| o["value"].as_str())
                {
                    sel.push(v.to_string());
                }
                let mut items: Vec<String> = opts
                    .iter()
                    .map(|o| {
                        let v = st(o, &["value"]);
                        let mut s = self.option_text(o, depth);
                        if s.is_empty() {
                            s = self.person(v).unwrap_or_else(|| v.to_string());
                        }
                        if !v.is_empty() && sel.iter().any(|x| x == v) {
                            s = format!("✓{s}");
                        }
                        s
                    })
                    .filter(|s| !s.is_empty())
                    .collect();
                if items.is_empty() {
                    if t.ends_with("person") && !sel.is_empty() {
                        items = sel
                            .iter()
                            .map(|v| format!("✓{}", self.person(v).unwrap_or_else(|| v.clone())))
                            .collect();
                    } else {
                        let ph = self.txt(&q["placeholder"], depth + 1);
                        items.push(if ph.is_empty() {
                            "请选择".into()
                        } else {
                            ph
                        });
                    }
                }
                (
                    if multi { "multi_select" } else { "select" },
                    format!("{{{}}}", items.join(" / ")),
                )
            }
            "select_img" => {
                let n = arr(q, &["options"]).len();
                ("select_img", format!("{{{n} 张图片可选}}"))
            }
            "input" => {
                let label = self.txt(&q["label"], depth + 1);
                let dv = st(q, &["defaultValue", "default_value"]).to_string();
                let ph = self.txt(&q["placeholder"], depth + 1);
                let v = if !dv.is_empty() {
                    dv
                } else if !ph.is_empty() {
                    format!("{ph}＿＿")
                } else {
                    "＿＿＿＿".into()
                };
                let v = self.field(&v);
                (
                    "input",
                    if label.is_empty() {
                        v
                    } else {
                        format!("{label}：{v}")
                    },
                )
            }
            "date_picker" | "picker_date" | "picker_time" | "picker_datetime" => {
                let (kind, keys): (&str, &[&str]) = match t {
                    "picker_time" => ("time", &["initialTime", "initial_time"]),
                    "picker_datetime" => ("datetime", &["initialDatetime", "initial_datetime"]),
                    _ => ("date", &["initialDate", "initial_date"]),
                };
                let mut v = st(q, keys).to_string();
                if v.is_empty() {
                    v = self.txt(&q["placeholder"], depth + 1);
                }
                if v.is_empty() {
                    v = "请选择".into();
                }
                let v = self.field(&v);
                (
                    kind,
                    format!("{} {v}", if kind == "time" { "🕐" } else { "📅" }),
                )
            }
            _ => {
                let checked = q["checked"].as_bool().unwrap_or(false);
                let s = self.txt(&q["text"], depth + 1);
                (
                    "checker",
                    format!("[{}] {s}", if checked { "x" } else { " " }),
                )
            }
        };
        json!({"t": "control", "kind": kind, "text": text})
    }

    fn option_text(&mut self, o: &Value, depth: usize) -> String {
        match o {
            Value::String(s) => self.field(s),
            _ => self.txt(&o["text"], depth + 1),
        }
    }

    fn table(&mut self, q: &Value, depth: usize) -> Value {
        let cols = arr(q, &["columns"]);
        if cols.len() > MAX_COLS {
            self.truncated = true;
        }
        let cols = &cols[..cols.len().min(MAX_COLS)];
        let names: Vec<String> = cols
            .iter()
            .map(|c| {
                let d = st(c, &["displayName", "display_name", "name"]);
                self.field(d)
            })
            .collect();
        let keys: Vec<&str> = cols.iter().map(|c| st(c, &["name"])).collect();
        let rows = arr(q, &["rows"]);
        if rows.len() > MAX_ROWS {
            self.truncated = true;
        }
        let rows: Vec<Vec<String>> = rows
            .iter()
            .take(MAX_ROWS)
            .map(|r| {
                keys.iter()
                    .map(|k| {
                        let c = &r[*k];
                        // the raw format wraps a cell: `{data: …}`
                        let c = if c.is_object() && !c["data"].is_null() {
                            &c["data"]
                        } else {
                            c
                        };
                        self.cell(c, depth)
                    })
                    .collect()
            })
            .collect();
        json!({"t": "table", "columns": names, "rows": rows})
    }

    fn cell(&mut self, c: &Value, depth: usize) -> String {
        match c {
            Value::String(s) => self.field(s),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Array(a) => a
                .iter()
                .filter_map(|x| x["text"].as_str().map(|t| format!("「{t}」")))
                .collect::<Vec<_>>()
                .join(" "),
            Value::Object(_) => self.txt(c, depth + 1),
            Value::Null => String::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &mut self,
        format: Format,
        schema: Option<i64>,
        title: Option<String>,
        subtitle: Option<String>,
        template: Option<String>,
        tags: Vec<String>,
        blocks: Vec<Value>,
    ) -> Card {
        let mut lines = Vec::new();
        if let Some(t) = &title {
            lines.push(t.clone());
        }
        if let Some(t) = &subtitle {
            lines.push(t.clone());
        }
        if !tags.is_empty() {
            lines.push(tags.iter().map(|t| format!("「{t}」")).collect::<String>());
        }
        for b in &blocks {
            block_lines(b, &mut lines);
        }
        let text = lines
            .into_iter()
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let (text, cut_) = cut(&text, MAX_TEXT);
        self.truncated |= cut_;
        let text = if text.is_empty() {
            "[卡片]".to_string()
        } else {
            text
        };
        let mut a = Map::new();
        a.insert("type".into(), json!("card"));
        a.insert("format".into(), json!(format.code()));
        a.insert("schema".into(), json!(schema));
        a.insert("title".into(), json!(title));
        if subtitle.is_some() {
            a.insert("subtitle".into(), json!(subtitle));
        }
        if template.is_some() {
            a.insert("template".into(), json!(template));
        }
        if !tags.is_empty() {
            a.insert("tags".into(), json!(tags));
        }
        a.insert("blocks".into(), Value::Array(blocks));
        a.insert("truncated".into(), json!(self.truncated));
        Card {
            text,
            attachment: Value::Object(a),
        }
    }
}

fn styled(s: &str, q: &Value) -> String {
    if s.is_empty() {
        return String::new();
    }
    let attrs: Vec<&str> = q["textStyle"]["attributes"]
        .as_array()
        .or(q["style"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut s = s.to_string();
    if attrs
        .iter()
        .any(|a| matches!(*a, "strikethrough" | "lineThrough"))
    {
        s = format!("~~{s}~~");
    }
    if attrs.contains(&"italic") {
        s = format!("*{s}*");
    }
    if attrs.contains(&"bold") {
        s = format!("**{s}**");
    }
    s
}

fn md_escape(s: &str) -> String {
    s.replace('[', "\\[").replace(']', "\\]")
}

fn style_of(q: &Value) -> Value {
    match st(q, &["type"]) {
        "primary" | "primary_filled" | "primary_text" => json!("primary"),
        "danger" | "danger_filled" | "danger_text" => json!("danger"),
        "" => Value::Null,
        _ => json!("default"),
    }
}

/// An `open_url` target: `url`, `multi_url.url`, `behaviors[type=open_url].default_url`,
/// the raw format's `actions[type=open_url].action.url`.
fn open_url(q: &Value) -> Option<String> {
    let mut c: Vec<&str> = vec![st(q, &["url"]), st(&q["multi_url"], &["url"])];
    for b in arr(q, &["behaviors"]) {
        if b["type"] == "open_url" {
            c.push(st(b, &["default_url", "url"]));
        }
    }
    for a in arr(q, &["actions"]) {
        if a["type"] == "open_url" {
            c.push(st(&a["action"], &["url"]));
        }
    }
    c.into_iter().find_map(safe_url)
}

/// The readable lines of a block.
fn block_lines(b: &Value, out: &mut Vec<String>) {
    let s = |k: &str| b[k].as_str().unwrap_or("").to_string();
    match b["t"].as_str().unwrap_or("") {
        "markdown" | "note" | "control" => out.push(s("text")),
        "heading" => out.push(s("text")),
        "hr" => out.push("---".into()),
        "image" => out.push(format!("[图片：{}]", s("alt"))),
        "buttons" => out.push(
            b["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| match i["url"].as_str() {
                    Some(u) => format!("[{}]({u})", i["text"].as_str().unwrap_or("")),
                    None => format!("[{}]", i["text"].as_str().unwrap_or("")),
                })
                .collect::<Vec<_>>()
                .join(" "),
        ),
        "columns" => {
            for c in b["columns"].as_array().into_iter().flatten() {
                for x in c.as_array().into_iter().flatten() {
                    block_lines(x, out);
                }
            }
        }
        "table" => {
            let cols: Vec<&str> = b["columns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            out.push(format!("| {} |", cols.join(" | ")));
            out.push(format!("|{}", "---|".repeat(cols.len())));
            for r in b["rows"].as_array().into_iter().flatten() {
                let cells: Vec<&str> = r
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                out.push(format!("| {} |", cells.join(" | ")));
            }
        }
        "panel" => {
            out.push(s("title"));
            for x in b["blocks"].as_array().into_iter().flatten() {
                block_lines(x, out);
            }
        }
        "group" => {
            for x in b["blocks"].as_array().into_iter().flatten() {
                block_lines(x, out);
            }
        }
        "chart" => match b["summary"].as_str() {
            Some(sm) => out.push(format!("[图表：{}，{sm}]", s("title"))),
            None => out.push(format!("[图表：{}]", s("title"))),
        },
        "media" => out.push(
            if b["kind"] == "audio" {
                "[音频]"
            } else {
                "[视频]"
            }
            .into(),
        ),
        "unknown" => {
            let t = s("text");
            out.push(if t.is_empty() {
                "[不支持的卡片内容]".into()
            } else {
                t
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(c: &Card) -> &Vec<Value> {
        c.attachment["blocks"].as_array().unwrap()
    }

    fn ts(c: &Card) -> Vec<&str> {
        blocks(c).iter().map(|b| b["t"].as_str().unwrap()).collect()
    }

    fn m() -> Vec<(String, String)> {
        vec![("@_user_1".into(), "李鹏翔".into())]
    }

    /// message_content.md “卡片 interactive”: what the receive event and the message API give
    /// by default.
    #[test]
    fn flattened_default_shape() {
        let content = json!({
            "title": "卡片标题",
            "elements": [
                [{"tag": "button", "text": "主按钮", "type": "primary"}, {"tag": "button", "text": "次按钮", "type": "default"}],
                [{"tag": "a", "href": "https://www.feishu.cn", "text": "飞书"}, {"tag": "text", "text": "整合，"},
                 {"tag": "at", "user_id": "@_user_1", "user_name": ""}, {"tag": "text", "text": "更高效。"}],
                [{"tag": "hr"}],
                [{"tag": "text", "text": "图片标题"}, {"tag": "img", "image_key": "img_acd8a194"}],
                [{"tag": "note", "elements": [{"tag": "img", "image_key": "img_x"}, {"tag": "text", "text": "备注信息"}]}],
                [{"tag": "text", "text": "选一个"}, {"tag": "select_static", "options": ["选项1", "选项2"], "placeholder": "默认提示文本"}],
                [{"tag": "overflow", "options": ["打开目录", "打开文档"]}],
                [{"tag": "date_picker", "placeholder": "请选择日期", "initial_date": "2021-1-1"}],
                [{"tag": "a", "href": "javascript:alert(1)", "text": "坏链接"}]
            ]
        })
        .to_string();
        let c = parse(&content, &m());
        assert_eq!(c.attachment["format"], "flattened");
        assert_eq!(c.attachment["schema"], Value::Null);
        assert_eq!(c.attachment["title"], "卡片标题");
        assert_eq!(
            ts(&c),
            [
                "buttons", "markdown", "hr", "markdown", "image", "note", "markdown", "control",
                "control", "control", "markdown"
            ]
        );
        let b = blocks(&c);
        assert_eq!(b[0]["items"][0]["text"], "主按钮");
        assert_eq!(b[0]["items"][0]["style"], "primary");
        assert_eq!(b[0]["items"][0]["callback"], true);
        assert!(b[0]["items"][0].get("url").is_none());
        assert_eq!(
            b[1]["text"],
            "[飞书](https://www.feishu.cn)整合，@李鹏翔更高效。"
        );
        assert_eq!(b[5]["text"], "[图片] 备注信息");
        assert_eq!(
            b[7]["text"],
            "{默认提示文本}".replace("默认提示文本", "选项1 / 选项2")
        );
        assert_eq!(b[8]["text"], "⋮ 打开目录、打开文档");
        assert_eq!(b[9]["text"], "📅 2021-1-1");
        assert_eq!(b[10]["text"], "坏链接", "javascript: dropped, text kept");
        assert!(c.text.starts_with("卡片标题\n[主按钮] [次按钮]\n"));
        assert!(!c.text.contains("javascript"));
        assert_eq!(c.attachment["truncated"], false);
    }

    #[test]
    fn flattened_with_a_title_and_no_body() {
        let c = parse(r#"{"title":"标题","elements":[]}"#, &[]);
        assert_eq!(c.attachment["format"], "flattened");
        assert_eq!(c.attachment["title"], "标题");
        assert_eq!(c.text, "标题");
        assert!(blocks(&c).is_empty());
    }

    /// larksuite/cli card_test.go `TestConvertCard`: the API wrapper, `property` nodes, the
    /// attachment as a string and as an object, `card_schema`.
    #[test]
    fn raw_wrapper_from_the_official_tests() {
        let raw = r#"{"json_card":"{\"schema\":1,\"header\":{\"title\":{\"content\":\"Card Title\"}},\"body\":{\"elements\":[{\"tag\":\"text\",\"property\":{\"content\":\"hello\"}},{\"tag\":\"button\",\"property\":{\"text\":{\"content\":\"Open\"},\"actions\":[{\"type\":\"open_url\",\"action\":{\"url\":\"https://example.com\"}}]}}]}}","json_attachment":"{\"persons\":{\"ou_1\":{\"content\":\"Alice\"}}}"}"#;
        let c = parse(raw, &[]);
        assert_eq!(c.attachment["format"], "raw");
        assert_eq!(c.attachment["schema"], 1);
        assert_eq!(c.attachment["title"], "Card Title");
        assert_eq!(c.text, "Card Title\nhello\n[Open](https://example.com)");
        let b = blocks(&c);
        assert_eq!(b[1]["items"][0]["url"], "https://example.com");
        assert_eq!(b[1]["items"][0]["callback"], false);

        let obj = r#"{"json_card":"{\"schema\":1,\"header\":{\"title\":{\"content\":\"Title\"}},\"body\":{\"elements\":[{\"tag\":\"person\",\"property\":{\"userID\":\"ou_1\"}}]}}","json_attachment":{"persons":{"ou_1":{"content":"Alice"}}}}"#;
        assert!(parse(obj, &[]).text.contains("Alice"));

        let hinted = r#"{"json_card":"{\"body\":{\"elements\":[{\"tag\":\"text\",\"property\":{\"content\":\"hi\"}}]}}","card_schema":2}"#;
        let c = parse(hinted, &[]);
        assert_eq!(c.attachment["schema"], 2);
        assert_eq!(c.text, "hi");

        let empty = r#"{"json_card":"{\"body\":{\"elements\":[]}}"}"#;
        assert_eq!(parse(empty, &[]).text, "[卡片]");
    }

    /// The raw format's at, markdown inline elements, images by `imageID`, tables, header tags.
    #[test]
    fn raw_property_nodes() {
        let card = json!({
            "header": {"property": {"title": {"tag": "plain_text", "property": {"i18nContent": {"en_us": "Title EN", "zh_cn": "标题"}}},
                                    "template": "blue",
                                    "textTagList": [{"tag": "text_tag", "property": {"text": {"property": {"content": "进行中"}}}}]}},
            "body": {"property": {"elements": [
                {"tag": "markdown", "property": {"elements": [
                    {"tag": "text", "property": {"content": "请 ", "textStyle": {"attributes": ["bold"]}}},
                    {"tag": "at", "property": {"userID": "xxxxx"}},
                    {"tag": "link", "property": {"content": "看文档", "url": {"url": "https://open.feishu.cn/doc"}}},
                    {"tag": "code_span", "property": {"content": "x=1"}}
                ]}},
                {"tag": "img", "property": {"imageID": "i1", "alt": {"property": {"content": "架构图"}}}},
                {"tag": "table", "property": {"columns": [{"name": "a", "displayName": "名称"}, {"name": "b", "displayName": "值"}],
                                              "rows": [{"a": {"data": "CPU"}, "b": {"data": 93.5}}, {"a": {"data": [{"text": "VIP"}]}, "b": {"data": null}}]}}
            ]}}
        });
        let raw = json!({
            "json_card": card.to_string(),
            "json_attachment": {"at_users": {"xxxxx": {"content": "旧名", "mention_key": "@_user_1"}}, "images": {"i1": {"origin_key": "img_v3_origin"}}}
        })
        .to_string();
        let c = parse(&raw, &m());
        assert_eq!(c.attachment["title"], "标题", "zh_cn first");
        assert_eq!(c.attachment["template"], "blue");
        assert_eq!(c.attachment["tags"], json!(["进行中"]));
        let b = blocks(&c);
        assert_eq!(
            b[0]["text"],
            "**请 **@李鹏翔[看文档](https://open.feishu.cn/doc)`x=1`"
        );
        assert_eq!(b[1], json!({"t": "image", "alt": "架构图"}));
        assert!(
            !c.attachment.to_string().contains("img_v3_origin"),
            "no image keys out of a card"
        );
        assert_eq!(b[2]["columns"], json!(["名称", "值"]));
        assert_eq!(b[2]["rows"], json!([["CPU", "93.5"], ["「VIP」", ""]]));
    }

    /// The event's `{user_dsl}` (card_userdsl.go), schema 2.0: header at the root, `body.elements`.
    #[test]
    fn user_dsl_2() {
        let dsl = json!({
            "schema": "2.0",
            "header": {"title": {"tag": "plain_text", "content": "发布审批"}, "subtitle": {"tag": "plain_text", "content": "v0.2.3"}, "template": "orange"},
            "body": {"elements": [
                {"tag": "markdown", "content": "<at id=ou_1 mention_key=@_user_1></at> 请看 <font color='red'>紧急</font> <at id=all></at>"},
                {"tag": "column_set", "columns": [
                    {"tag": "column", "elements": [{"tag": "button", "text": {"tag": "plain_text", "content": "同意"}, "type": "primary_filled",
                        "behaviors": [{"type": "callback", "value": {"ok": 1}}]}]},
                    {"tag": "column", "elements": [{"tag": "button", "text": {"tag": "plain_text", "content": "详情"},
                        "behaviors": [{"type": "open_url", "default_url": "https://example.com/r/1"}]}]}
                ]},
                {"tag": "collapsible_panel", "expanded": false, "header": {"title": {"tag": "markdown", "content": "更多"}},
                    "elements": [{"tag": "div", "text": {"tag": "lark_md", "content": "细节"}}]},
                {"tag": "form", "name": "f", "elements": [
                    {"tag": "input", "label": {"tag": "plain_text", "content": "理由"}, "placeholder": {"tag": "plain_text", "content": "请输入"}},
                    {"tag": "button", "text": {"tag": "plain_text", "content": "提交"}, "form_action_type": "submit"}
                ]},
                {"tag": "fancy_new_widget", "content": "新组件文字"},
                {"tag": "mystery"},
                {"tag": "button", "text": {"tag": "plain_text", "content": "坏"}, "url": "lark://x"}
            ]}
        });
        let content = json!({"user_dsl": dsl.to_string()}).to_string();
        let c = parse(&content, &m());
        assert_eq!(c.attachment["format"], "user_dsl");
        assert_eq!(c.attachment["schema"], 2);
        assert_eq!(c.attachment["subtitle"], "v0.2.3");
        assert_eq!(
            ts(&c),
            [
                "markdown", "buttons", "panel", "group", "unknown", "unknown", "buttons"
            ]
        );
        let b = blocks(&c);
        assert_eq!(b[0]["text"], "@李鹏翔 请看 紧急 @所有人");
        let items = b[1]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2, "a row of button columns is one button row");
        assert_eq!(items[0]["callback"], true);
        assert_eq!(items[0]["style"], "primary");
        assert_eq!(items[1]["url"], "https://example.com/r/1");
        assert_eq!(b[2]["title"], "更多");
        assert_eq!(b[2]["blocks"][0]["text"], "细节");
        assert_eq!(b[3]["kind"], "form");
        assert_eq!(b[3]["blocks"][0]["text"], "理由：请输入＿＿");
        assert_eq!(b[3]["blocks"][1]["items"][0]["callback"], true);
        assert_eq!(
            b[4],
            json!({"t": "unknown", "tag": "fancy_new_widget", "text": "新组件文字"})
        );
        assert_eq!(b[5]["text"], "");
        assert!(b[6]["items"][0].get("url").is_none(), "lark:// dropped");
        assert!(c.text.contains("[不支持的卡片内容]"));
        assert!(c.text.starts_with("发布审批\nv0.2.3\n@李鹏翔"));
    }

    /// user DSL 1.0: `i18n_header`, root `elements`, div fields, action buttons with `multi_url`.
    #[test]
    fn user_dsl_1() {
        let dsl = json!({
            "config": {"wide_screen_mode": true},
            "i18n_header": {"zh_cn": {"title": {"tag": "plain_text", "content": "告警"}, "template": "red"}},
            "elements": [
                {"tag": "div", "text": {"tag": "lark_md", "content": "**服务** api"}, "fields": [
                    {"is_short": true, "text": {"tag": "lark_md", "content": "级别：P1"}},
                    {"is_short": true, "text": {"tag": "lark_md", "content": "时间：03:00"}}]},
                {"tag": "action", "actions": [
                    {"tag": "button", "text": {"tag": "plain_text", "content": "看板"}, "multi_url": {"url": "https://grafana.example.com/d/1"}},
                    {"tag": "select_static", "placeholder": {"tag": "plain_text", "content": "指派"}, "options": [
                        {"text": {"tag": "plain_text", "content": "甲"}, "value": "a"}, {"text": {"tag": "plain_text", "content": "乙"}, "value": "b"}],
                        "initial_option": "b"}]},
                {"tag": "note", "elements": [{"tag": "plain_text", "content": "来自监控"}]},
                {"tag": "hr"},
                {"tag": "img", "img_key": "img_v2_x", "alt": {"tag": "plain_text", "content": "曲线"}}
            ]
        });
        let c = parse(&json!({"user_dsl": dsl.to_string()}).to_string(), &[]);
        assert_eq!(c.attachment["schema"], 1);
        assert_eq!(c.attachment["title"], "告警");
        assert_eq!(c.attachment["template"], "red");
        assert_eq!(
            ts(&c),
            ["markdown", "buttons", "control", "note", "hr", "image"]
        );
        let b = blocks(&c);
        assert_eq!(b[0]["text"], "**服务** api\n级别：P1\n时间：03:00");
        assert_eq!(b[1]["items"][0]["url"], "https://grafana.example.com/d/1");
        assert_eq!(b[1]["items"][0]["callback"], false);
        assert_eq!(
            b[2],
            json!({"t": "control", "kind": "select", "text": "{甲 / ✓乙}"})
        );
        assert_eq!(b[5]["alt"], "曲线");
        assert!(!c.attachment.to_string().contains("img_v2_x"));
    }

    /// `card_msg_content_type=user_card_content`: the card JSON itself.
    #[test]
    fn direct_schema() {
        let c = parse(
            r#"{"schema":"2.0","body":{"elements":[{"tag":"markdown","content":"直接 2.0"},{"tag":"chart","chart_spec":{"title":{"text":"流量"},"data":[{"values":[{"x":1},{"x":2}]}]}},{"tag":"video"}]}}"#,
            &[],
        );
        assert_eq!(c.attachment["format"], "schema");
        assert_eq!(c.attachment["schema"], 2);
        assert_eq!(c.text, "直接 2.0\n[图表：流量，2 个数据点]\n[视频]");
        let c = parse(
            r#"{"header":{"title":{"tag":"plain_text","content":"旧"}},"elements":[{"tag":"markdown","content":"1.0 正文"}]}"#,
            &[],
        );
        assert_eq!(
            (c.attachment["schema"].clone(), c.text.as_str()),
            (json!(1), "旧\n1.0 正文")
        );
    }

    #[test]
    fn malformed_and_odd_input() {
        for bad in [
            "",
            "not json",
            "[]",
            "null",
            r#"{"json_card":"{oops"}"#,
            r#"{"user_dsl":""}"#,
            r#"{"elements":"x"}"#,
        ] {
            let c = parse(bad, &[]);
            assert_eq!(c.attachment["type"], "card", "{bad}");
            assert!(!c.text.is_empty(), "{bad}");
        }
        let c = parse(
            r#"{"elements":[{"tag":"markdown","content":5},7,"s",{"tag":"table","columns":"x"}],"header":{"title":3}}"#,
            &[],
        );
        assert_eq!(c.attachment["format"], "schema");
    }

    #[test]
    fn limits_hold_and_say_so() {
        // too big to parse
        let big = format!(
            r#"{{"elements":[[{{"tag":"text","text":"{}"}}]]}}"#,
            "x".repeat(MAX_INPUT)
        );
        let c = parse(&big, &[]);
        assert_eq!(c.attachment["truncated"], true);
        assert_eq!(c.text, "[卡片：内容过大]");

        // deep nesting
        let mut e = json!({"tag": "markdown", "content": "底"});
        for _ in 0..25 {
            e = json!({"tag": "column_set", "columns": [{"tag": "column", "elements": [e, {"tag": "markdown", "content": "旁"}]}]});
        }
        let c = parse(
            &json!({"schema": "2.0", "body": {"elements": [e]}}).to_string(),
            &[],
        );
        assert_eq!(c.attachment["truncated"], true);
        assert!(c.attachment.to_string().len() < 200_000);
        // past serde's own nesting limit it does not parse at all: still a card, nothing leaks
        let mut deep = String::from("1");
        for _ in 0..200 {
            deep = format!("[{deep}]");
        }
        let c = parse(&format!(r#"{{"elements":{deep}}}"#), &[]);
        assert_eq!((c.text.as_str(), blocks(&c).len()), ("[卡片]", 0));

        // thousands of flattened paragraphs, each block counted
        let paras: Vec<Value> = (0..2000)
            .map(|i| json!([{"tag": "text", "text": format!("第{i}段")}, {"tag": "hr"}]))
            .collect();
        let c = parse(&json!({"title": "多", "elements": paras}).to_string(), &[]);
        assert_eq!(blocks(&c).len(), MAX_BLOCKS);
        assert_eq!(c.attachment["truncated"], true);

        // one flattened line far past the field limit
        let pieces: Vec<Value> = (0..400)
            .map(|_| json!({"tag": "text", "text": "长".repeat(100)}))
            .collect();
        let c = parse(&json!({"elements": [pieces]}).to_string(), &[]);
        assert_eq!(c.attachment["truncated"], true);
        assert!(blocks(&c)[0]["text"].as_str().unwrap().chars().count() <= MAX_FIELD + 1);

        // schema blocks: div fields, list, blockquote, table cells, each one past the limit
        let long = "字".repeat(MAX_FIELD + 50);
        let card = json!({"schema": "2.0", "body": {"elements": [
            {"tag": "div", "fields": [{"text": {"tag": "lark_md", "content": long}}, {"text": {"tag": "lark_md", "content": long}}]},
            {"tag": "list", "items": [{"elements": [{"tag": "text", "content": long}]}]},
            {"tag": "blockquote", "content": long},
            {"tag": "table", "columns": [{"name": "a"}], "rows": [{"a": [{"text": long}, {"text": long}]}]},
            {"tag": "note", "elements": [{"tag": "plain_text", "content": long}, {"tag": "plain_text", "content": long}]}
        ]}});
        let c = parse(&card.to_string(), &[]);
        assert_eq!(c.attachment["truncated"], true);
        fn longest(v: &Value) -> usize {
            match v {
                Value::String(s) => s.chars().count(),
                Value::Array(a) => a.iter().map(longest).max().unwrap_or(0),
                Value::Object(o) => o.values().map(longest).max().unwrap_or(0),
                _ => 0,
            }
        }
        assert!(longest(&c.attachment) <= MAX_FIELD + 1);
        assert!(c.text.chars().count() <= MAX_TEXT + 1);
    }

    #[test]
    fn urls() {
        assert_eq!(
            safe_url("https://a.b/c?d=1").as_deref(),
            Some("https://a.b/c?d=1")
        );
        assert_eq!(safe_url("http://a.b").as_deref(), Some("http://a.b"));
        for bad in [
            "javascript:alert(1)",
            "data:text/html,x",
            "/relative",
            "lark://open",
            "https://",
            "file:///etc/passwd",
        ] {
            assert_eq!(safe_url(bad), None, "{bad}");
        }
        assert_eq!(safe_url(&format!("https://a.b/{}", "x".repeat(3000))), None);
    }
}
