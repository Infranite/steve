use super::*;

impl Walker<'_> {
    pub(super) fn element_markdown(
        &mut self,
        e: &Value,
        q: &Value,
        depth: usize,
        out: &mut Vec<Value>,
    ) {
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

    pub(super) fn element_div(&mut self, q: &Value, depth: usize, out: &mut Vec<Value>) {
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

    pub(super) fn element_note(&mut self, q: &Value, depth: usize, out: &mut Vec<Value>) {
        let parts: Vec<String> = arr(q, &["elements"])
            .iter()
            .map(|x| self.inline(x, depth + 1))
            .filter(|s| !s.is_empty())
            .collect();
        if !parts.is_empty() {
            self.push(out, json!({"t": "note", "text": parts.join(" ")}));
        }
    }

    pub(super) fn element_heading(&mut self, q: &Value, depth: usize, out: &mut Vec<Value>) {
        let level = q["level"].as_i64().unwrap_or(1).clamp(1, 6);
        let s = match q["content"].as_str() {
            Some(c) => self.md(c),
            None => self.txt(q, depth),
        };
        if !s.is_empty() {
            self.push(out, json!({"t": "heading", "level": level, "text": s}));
        }
    }

    pub(super) fn element_columns(&mut self, q: &Value, depth: usize, out: &mut Vec<Value>) {
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

    pub(super) fn element_actions(&mut self, q: &Value, depth: usize, out: &mut Vec<Value>) {
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

    pub(super) fn element_chart(&mut self, q: &Value, out: &mut Vec<Value>) {
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

    pub(super) fn element_panel(&mut self, q: &Value, depth: usize, out: &mut Vec<Value>) {
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

    pub(super) fn element_person(&mut self, q: &Value, depth: usize, out: &mut Vec<Value>) {
        let id = st(q, &["userID", "user_id"]);
        let name = self
            .person(id)
            .or_else(|| Some(self.txt(&q["notation"], depth + 1)).filter(|s| !s.is_empty()))
            .unwrap_or_else(|| id.to_string());
        self.md_block(out, name);
    }

    pub(super) fn element_people(&mut self, q: &Value, out: &mut Vec<Value>) {
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

    pub(super) fn element_list(&mut self, q: &Value, depth: usize, out: &mut Vec<Value>) {
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

    pub(super) fn element_quote(&mut self, q: &Value, depth: usize, out: &mut Vec<Value>) {
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

    pub(super) fn element_code(&mut self, q: &Value, out: &mut Vec<Value>) {
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

    pub(super) fn element_fallback(&mut self, q: &Value, depth: usize, out: &mut Vec<Value>) {
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

    pub(super) fn element_unknown(
        &mut self,
        other: &str,
        q: &Value,
        depth: usize,
        out: &mut Vec<Value>,
    ) {
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

    pub(super) fn select_control(
        &mut self,
        t: &str,
        q: &Value,
        depth: usize,
    ) -> (&'static str, String) {
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
}
