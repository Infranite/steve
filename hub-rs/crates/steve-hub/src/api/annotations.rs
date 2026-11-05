//! `/api/items/{id}/annotations*` (ST-38): notes on an item's outputs. A note is a draft until the user
//! sends it; sending puts the drafts on the item as one message of the user's (like a supplement), with
//! each output's version and the place marked, and links the notes to that line. A send carries a key:
//! the same send repeated reaches the item once.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use serde_json::{Map, Value, json};

use super::{ApiResult, AppState, Ctx, Noun, body, ok, ok_true, own, str_of};
use crate::backend::Obj;
use crate::error::ApiError;
use crate::validate::{Check, Str};

const NOTE_MAX: usize = 4000;
const QUOTE_MAX: usize = 2000;

async fn own_item(st: &AppState, ws: &str, id: &str) -> Result<Obj, ApiError> {
    let (id, ws) = (id.to_string(), ws.to_string());
    st.read(move |r| Ok(own(r.item_get(&id)?, &ws, Noun::Item)?))
        .await
}

fn frac(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
        .filter(|f| f.is_finite() && (0.0..=1.0).contains(f))
}

/// The place a note is about, with only the keys each kind has; `None` when it is not one.
fn anchor_of(v: &Value) -> Option<Value> {
    let a = v.as_object()?;
    let text = |k: &str, max: usize| {
        a.get(k)
            .and_then(Value::as_str)
            .map(|s| s.chars().take(max).collect::<String>())
    };
    let label = text("at", 80);
    let mut out = match a.get("type")?.as_str()? {
        "region" => {
            let (x, y, w, h) = (
                frac(a.get("x"))?,
                frac(a.get("y"))?,
                frac(a.get("w"))?,
                frac(a.get("h"))?,
            );
            if w <= 0.0 || h <= 0.0 || x + w > 1.0001 || y + h > 1.0001 {
                return None;
            }
            json!({ "type": "region", "x": x, "y": y, "w": w, "h": h })
        }
        "text" => {
            let quote = text("quote", QUOTE_MAX).filter(|q| !q.trim().is_empty())?;
            let mut t = json!({ "type": "text", "quote": quote });
            for k in ["prefix", "suffix"] {
                if let Some(s) = text(k, 64) {
                    t[k] = json!(s);
                }
            }
            if let Some(n) = a.get("line").and_then(Value::as_u64) {
                t["line"] = json!(n);
            }
            t
        }
        "page" => json!({ "type": "page", "page": a.get("page")?.as_u64().filter(|n| *n >= 1)? }),
        "file" => json!({ "type": "file" }),
        _ => return None,
    };
    if let Some(l) = label.filter(|l| !l.is_empty()) {
        out["at"] = json!(l);
    }
    Some(out)
}

pub async fn add(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let art = k.str("art", Str::min(1));
    let path = k.opt_str("path", Str::max(1000)).unwrap_or_default();
    let note = k.str("note", Str::len(1, NOTE_MAX));
    k.finish()?;
    let anchor = v
        .get("anchor")
        .and_then(anchor_of)
        .ok_or_else(|| ApiError::Invalid(vec!["anchor".into()]))?;
    if note.trim().is_empty() {
        return Err(ApiError::bad_request(steve_i18n::t!(
            "hub.annotation.empty"
        )));
    }
    st.write(move |r| {
        if !r
            .artifacts_list(&id)?
            .iter()
            .any(|a| str_of(a, "id") == art)
        {
            return Err(ApiError::not_found(steve_i18n::t!("hub.not_found.output")));
        }
        ok(Value::Object(r.annotation_add(
            &id,
            &art,
            &path,
            &anchor,
            note.trim(),
        )?))
    })
    .await
}

pub async fn edit(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, ann)): Path<(String, String)>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let note = k.str("note", Str::len(1, NOTE_MAX));
    k.finish()?;
    if note.trim().is_empty() {
        return Err(ApiError::bad_request(steve_i18n::t!(
            "hub.annotation.empty"
        )));
    }
    st.write(move |r| match r.annotation_edit(&id, &ann, note.trim())? {
        Some(a) => ok(Value::Object(a)),
        None => Err(ApiError::Json(
            409,
            steve_i18n::t!("hub.annotation.sent_no_edit"),
        )),
    })
    .await
}

pub async fn remove(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, ann)): Path<(String, String)>,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    st.write(move |r| {
        if r.annotation_remove(&id, &ann)? {
            ok_true()
        } else {
            Err(ApiError::Json(
                409,
                steve_i18n::t!("hub.annotation.sent_no_delete"),
            ))
        }
    })
    .await
}

/// `第 n 版` of every output: outputs delivered again under the same kind and location (or title) are
/// versions of one, numbered by delivery.
fn versions(arts: &[Obj]) -> Map<String, Value> {
    let key = |a: &Obj| {
        let loc = str_of(a, "location");
        format!(
            "{}\u{1}{}",
            str_of(a, "kind"),
            if loc.is_empty() {
                str_of(a, "title")
            } else {
                loc
            }
        )
    };
    let mut seen: std::collections::HashMap<String, u64> = Default::default();
    arts.iter()
        .map(|a| {
            let n = seen.entry(key(a)).or_default();
            *n += 1;
            (str_of(a, "id"), json!(*n))
        })
        .collect()
}

fn pct(v: &Value, k: &str) -> String {
    format!("{}%", (v[k].as_f64().unwrap_or(0.0) * 100.0).round())
}

/// Where in the output a note is, in words.
fn place(a: &Obj) -> String {
    let anchor = a.get("anchor").cloned().unwrap_or(Value::Null);
    let path = str_of(a, "path");
    let file = path.rsplit('/').next().filter(|s| !s.is_empty());
    let at = anchor["at"].as_str().filter(|s| !s.is_empty());
    let what = match anchor["type"].as_str() {
        Some("region") => format!(
            "图片区域（左 {}，上 {}，宽 {}，高 {}）",
            pct(&anchor, "x"),
            pct(&anchor, "y"),
            pct(&anchor, "w"),
            pct(&anchor, "h")
        ),
        Some("text") => {
            let q: String = anchor["quote"]
                .as_str()
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let short: String = q.chars().take(120).collect();
            let more = if q.chars().count() > 120 { "…" } else { "" };
            match anchor["line"].as_u64() {
                Some(n) => format!("选中文字「{short}{more}」（第 {n} 行）"),
                None => format!("选中文字「{short}{more}」"),
            }
        }
        Some("page") => format!("第 {} 页", anchor["page"].as_u64().unwrap_or(1)),
        _ => "整体".into(),
    };
    [
        file.map(|f| format!("`{f}`")),
        at.map(str::to_string),
        Some(what),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

/// The message the notes go out as: grouped by output, each with its version and location.
fn message(notes: &[Obj], arts: &[Obj]) -> String {
    let ver = versions(arts);
    let mut out = format!("对产出的批注（{} 条）：", notes.len());
    let mut n = 0;
    let mut order: Vec<String> = Vec::new();
    for a in notes {
        let art = str_of(a, "artifact_id");
        if !order.contains(&art) {
            order.push(art);
        }
    }
    for art in order {
        let o = arts.iter().find(|x| str_of(x, "id") == art);
        let (kind, title, loc) = o
            .map(|o| (str_of(o, "kind"), str_of(o, "title"), str_of(o, "location")))
            .unwrap_or_default();
        let v = ver.get(&art).and_then(Value::as_u64).unwrap_or(1);
        out.push_str(&format!("\n\n**{title}**（{kind} · 第 {v} 版）"));
        if !loc.is_empty() {
            out.push_str(&format!(" `{loc}`"));
        }
        for a in notes.iter().filter(|a| str_of(a, "artifact_id") == art) {
            n += 1;
            out.push_str(&format!("\n{n}. {}：{}", place(a), str_of(a, "note")));
        }
    }
    out
}

pub async fn send(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_item(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let ids = k.str_array("ids", Some(1), Some(200));
    let key = k.str("key", Str::len(8, 100));
    k.finish()?;
    // a send runs to its end even when the request goes away (a reload, a dropped connection), so its
    // drafts are never left taken but unsettled
    tokio::spawn(steve_i18n::bind(send_now(st, id, ids, key)))
        .await
        .map_err(|e| {
            ApiError::Json(
                500,
                steve_i18n::t!("hub.annotation.send_interrupted", error = e),
            )
        })?
}

fn line_id(t: &Obj) -> i64 {
    t.get("id").and_then(Value::as_i64).unwrap_or(0)
}

async fn send_now(st: AppState, id: String, ids: Vec<String>, key: String) -> ApiResult {
    let (iid, ikey) = (id.clone(), key.clone());
    let (again, taken, arts, before) = st
        .write(move |r| {
            let (again, taken) = r.annotations_claim(&iid, &ids, &ikey)?;
            let before = r.timeline_list(&iid)?.iter().map(line_id).max();
            Ok((again, taken, r.artifacts_list(&iid)?, before.unwrap_or(0)))
        })
        .await?;
    if again {
        // the same send arrived before (a second click, a retry): it is not sent twice; `pending` while
        // that first one is still on its way
        let done = taken.iter().all(|a| str_of(a, "status") == "sent");
        return ok(json!({ "ok": true, "again": true, "pending": !done, "sent": taken.len() }));
    }
    if taken.is_empty() {
        return Err(ApiError::Json(
            409,
            steve_i18n::t!("hub.annotation.none_to_send"),
        ));
    }
    let text = message(&taken, &arts);
    let res = st.domain.supplement(&id, &text, "auto", Vec::new()).await;
    st.write(move |r| {
        // the line the supplement wrote, if it got that far: the user's, with this text, after the send began
        let line = r
            .timeline_list(&id)?
            .iter()
            .rev()
            .take_while(|t| line_id(t) > before)
            .find(|t| str_of(t, "kind") == "me" && str_of(t, "text") == text)
            .map(line_id);
        match (res, line) {
            (Ok(()), line) => {
                r.annotations_settle(&id, &key, true, line)?;
                ok(json!({ "ok": true, "again": false, "sent": taken.len(), "timeline_id": line }))
            }
            // the item has the message, only reaching its session failed: sending again would repeat it
            (Err(e), Some(line)) => {
                r.annotations_settle(&id, &key, true, Some(line))?;
                ok(json!({ "ok": true, "again": false, "sent": taken.len(), "timeline_id": line, "warning": e.message }))
            }
            (Err(e), None) => {
                r.annotations_settle(&id, &key, false, None)?;
                Err(e.into())
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(v: Value) -> Obj {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn anchors_keep_only_what_their_kind_has() {
        assert_eq!(
            anchor_of(
                &json!({"type": "region", "x": 0.1, "y": 0.2, "w": 0.3, "h": 0.4, "evil": 1})
            ),
            Some(json!({"type": "region", "x": 0.1, "y": 0.2, "w": 0.3, "h": 0.4}))
        );
        assert_eq!(
            anchor_of(&json!({"type": "region", "x": 0.9, "y": 0.2, "w": 0.3, "h": 0.4})),
            None
        );
        assert_eq!(
            anchor_of(&json!({"type": "region", "x": 0.1, "y": 0.2, "w": 0, "h": 0.4})),
            None
        );
        assert_eq!(anchor_of(&json!({"type": "text", "quote": "  "})), None);
        assert_eq!(
            anchor_of(
                &json!({"type": "text", "quote": "第一段", "line": 3, "at": "工作表 Sheet1"})
            ),
            Some(json!({"type": "text", "quote": "第一段", "line": 3, "at": "工作表 Sheet1"}))
        );
        assert_eq!(anchor_of(&json!({"type": "page", "page": 0})), None);
        assert_eq!(
            anchor_of(&json!({"type": "page", "page": 2})),
            Some(json!({"type": "page", "page": 2}))
        );
        assert_eq!(anchor_of(&json!({"type": "script"})), None);
        assert_eq!(anchor_of(&json!("file")), None);
    }

    #[test]
    fn the_message_names_each_output_its_version_and_the_places() {
        let arts = vec![
            o(json!({"id": "art_1", "kind": "文件", "title": "截图", "location": "/tmp/shots"})),
            o(json!({"id": "art_2", "kind": "文档", "title": "方案", "location": "docs/a.md"})),
            o(
                json!({"id": "art_3", "kind": "文件", "title": "截图（新）", "location": "/tmp/shots"}),
            ),
        ];
        let notes = vec![
            o(
                json!({"artifact_id": "art_3", "path": "shots/a.png", "anchor": {"type": "region", "x": 0.1, "y": 0.25, "w": 0.5, "h": 0.125}, "note": "按钮被裁掉了"}),
            ),
            o(
                json!({"artifact_id": "art_2", "path": "", "anchor": {"type": "text", "quote": "旧的\n说法", "line": 12}, "note": "改成新的"}),
            ),
            o(
                json!({"artifact_id": "art_3", "path": "shots/b.png", "anchor": {"type": "file"}, "note": "整体偏暗"}),
            ),
        ];
        assert_eq!(
            message(&notes, &arts),
            "对产出的批注（3 条）：\n\n**截图（新）**（文件 · 第 2 版） `/tmp/shots`\n1. `a.png` · 图片区域（左 10%，上 25%，宽 50%，高 13%）：按钮被裁掉了\n2. `b.png` · 整体：整体偏暗\n\n**方案**（文档 · 第 1 版） `docs/a.md`\n3. 选中文字「旧的 说法」（第 12 行）：改成新的"
        );
    }
}
