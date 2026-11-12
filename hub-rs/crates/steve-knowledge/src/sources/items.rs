//! The `items` source: one document per finished item — latest goal, delivery
//! summary, outputs, and the text of text outputs inside the item's `cwd` — plus the candidate
//! extraction each completion triggers (once per completion: `kb_marks extract:item:<id>`).
//!
//! steve-core calls [`KnowledgeHooks`] after `finish` (B1 `item_hooks`); a reopened and finished
//! again item is rebuilt and extracted again. Deleting an item: [`item_deleted`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use steve_modules::hooks::ItemHooks;
use steve_modules::{ApiError, ApiResult, Modules};
use steve_store::base::Level;
use steve_store::base::mems::Mem;
use steve_store::{Item, Tx, uid};

use crate::candidates::{Origin, OriginKind};
use crate::extract::{self, MAX_INPUT, clip};
use crate::extract_text::markdown;
use crate::index::{self, BoxFuture, DocLoader, Loaded};
use crate::repo::{self, DocRow, NewDoc, SourceRow};
use crate::sources;
use crate::state::Knowledge;

pub const KIND: &str = "items";
/// Text outputs larger than this are listed, not read.
pub const MAX_OUTPUT_BYTES: u64 = 200 * 1024;

/// What the item document and the extraction are built from.
#[derive(Debug, Clone, Default)]
pub struct ItemFacts {
    pub id: String,
    pub ws: String,
    pub title: String,
    pub cwd: Option<String>,
    /// Goal versions, oldest first.
    pub goals: Vec<String>,
    /// What you wrote on the timeline (supplements), oldest first.
    pub said: Vec<String>,
    /// `(question, your answer)` of answered questions.
    pub answers: Vec<(String, String)>,
    /// Rework notes given at acceptance.
    pub reviews: Vec<String>,
    /// The summary of the latest delivery.
    pub summary: String,
    /// `(kind, title, location)`.
    pub outputs: Vec<(String, String, Option<String>)>,
    /// The user who started the item (else the workspace owner).
    pub user: String,
    /// Changes whenever the item is finished again (`items.updated`).
    pub stamp: i64,
}

fn rows<T>(
    c: &Connection,
    sql: &str,
    id: &str,
    f: impl FnMut(&rusqlite::Row) -> rusqlite::Result<T>,
) -> ApiResult<Vec<T>> {
    Ok(c.prepare_cached(sql)?
        .query_map([id], f)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Reads an item's facts; `None` if there is no such item.
pub fn facts(c: &Connection, id: &str) -> ApiResult<Option<ItemFacts>> {
    let head: Option<(String, String, Option<String>, i64)> = c
        .prepare_cached("SELECT workspace_id, title, cwd, updated FROM items WHERE id = ?")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    let Some((ws, title, cwd, stamp)) = head else {
        return Ok(None);
    };
    let goals = rows(
        c,
        "SELECT text FROM item_goals WHERE item_id = ? ORDER BY version",
        id,
        |r| r.get(0),
    )?;
    let said: Vec<String> = rows(
        c,
        "SELECT text FROM timeline WHERE item_id = ? AND kind = 'me' ORDER BY id",
        id,
        |r| r.get(0),
    )?
    .into_iter()
    // answers and reviews come from their decisions below; bare actions say nothing to remember
    .filter(|t: &String| {
        !t.starts_with("回答：")
            && !t.starts_with("返工：")
            && !matches!(
                t.as_str(),
                "验收通过" | "暂停" | "继续" | "放弃" | "重新打开" | "归档" | "恢复" | "撤销了决定"
            )
    })
    .collect();
    let decs: Vec<(String, String, String, String, Option<String>)> = rows(
        c,
        "SELECT kind, question, draft, status, result FROM decisions WHERE item_id = ? ORDER BY created, id",
        id,
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let mut f = ItemFacts {
        id: id.into(),
        ws: ws.clone(),
        title,
        cwd,
        goals,
        said,
        stamp,
        ..Default::default()
    };
    for (kind, question, draft, status, result) in decs {
        let result: Value = result
            .as_deref()
            .and_then(|r| serde_json::from_str(r).ok())
            .unwrap_or(Value::Null);
        let s = |k: &str| {
            result
                .get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
        match kind.as_str() {
            "ask" if status == "resolved" => {
                if let Some(a) = s("answer") {
                    f.answers.push((question, a));
                }
            }
            "accept" => {
                f.summary = draft;
                // a rework Steve sent back on its own (YOLO) is its check, not something you said
                if s("choice").as_deref() == Some("rework")
                    && s("decided_by").as_deref() != Some("yolo")
                    && let Some(t) = s("text")
                {
                    f.reviews.push(t);
                }
            }
            _ => {}
        }
    }
    f.outputs = rows(
        c,
        "SELECT kind, title, location FROM artifacts WHERE item_id = ? ORDER BY created, id",
        id,
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    f.user = initiator(c, &ws, id)?;
    Ok(Some(f))
}

/// The workspace user who created the item (audit `item.create`), else the workspace owner.
pub fn initiator(c: &Connection, ws: &str, item: &str) -> ApiResult<String> {
    let actor: Option<String> = c
        .prepare_cached(
            "SELECT a.actor FROM audit a JOIN workspace_members m ON m.user_id = a.actor AND m.workspace_id = a.workspace_id
             WHERE a.workspace_id = ? AND a.action = 'item.create' AND a.target = ? ORDER BY a.id LIMIT 1",
        )?
        .query_row([ws, item], |r| r.get(0))
        .optional()?;
    match actor {
        Some(a) => Ok(a),
        None => extract::owner(c, ws),
    }
}

/// Is `loc` a text file inside `cwd`, small enough to read? Then its text.
fn read_output(cwd: &Path, loc: &str) -> Option<String> {
    let root = cwd.canonicalize().ok()?;
    let p = PathBuf::from(loc);
    let p = if p.is_absolute() { p } else { root.join(p) };
    let p = p.canonicalize().ok()?;
    if !p.starts_with(&root) {
        return None;
    }
    let meta = std::fs::metadata(&p).ok()?;
    if !meta.is_file() || meta.len() > MAX_OUTPUT_BYTES {
        return None;
    }
    let bytes = std::fs::read(&p).ok()?;
    // the attachments sniff in short: text is valid UTF-8 without NUL bytes
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// The document text (Markdown). Reads text outputs from disk: call it off the runtime.
pub fn doc_text(f: &ItemFacts) -> String {
    let mut s = format!("# {} {}\n", f.id, f.title);
    if let Some(g) = f.goals.last() {
        s.push_str(&format!("\n## 目标\n\n{g}\n"));
    }
    if !f.summary.trim().is_empty() {
        s.push_str(&format!("\n## 交付摘要\n\n{}\n", f.summary.trim()));
    }
    if !f.outputs.is_empty() {
        s.push_str("\n## 产出\n\n");
        for (_, title, loc) in &f.outputs {
            match loc {
                Some(l) if !l.is_empty() => s.push_str(&format!("- {title}：{l}\n")),
                _ => s.push_str(&format!("- {title}\n")),
            }
        }
        if let Some(cwd) = f.cwd.as_deref().filter(|c| !c.is_empty()) {
            for (_, title, loc) in &f.outputs {
                if let Some(body) = loc.as_deref().and_then(|l| read_output(Path::new(cwd), l)) {
                    s.push_str(&format!("\n## {title}\n\n{}\n", body.trim()));
                }
            }
        }
    }
    s
}

/// The extraction input: goal versions, what you said, answered questions, review notes and the
/// delivery summary; at most 12000 characters. The goals are marked as this item's own business: the
/// model is told restating them is no memory, and [`extract::check`] drops what does.
pub fn extract_text(f: &ItemFacts) -> String {
    let mut s = format!("事项 {}：{}\n", f.id, f.title);
    if !f.goals.is_empty() {
        s.push_str("\n（下面的目标是这件事要做的事，属于事项记录）\n");
    }
    for (i, g) in f.goals.iter().enumerate() {
        s.push_str(&format!("\n目标 v{}：{g}\n", i + 1));
    }
    for t in &f.said {
        s.push_str(&format!("\n你补充：{t}\n"));
    }
    for (q, a) in &f.answers {
        s.push_str(&format!("\n问题：{q}\n你的回答：{a}\n"));
    }
    for r in &f.reviews {
        s.push_str(&format!("\n验收意见：{r}\n"));
    }
    if !f.summary.trim().is_empty() {
        s.push_str(&format!("\n交付摘要：{}\n", f.summary.trim()));
    }
    clip(&s, MAX_INPUT)
}

/// Writes (or rewrites) the item's document and indexes it, in one transaction. Returns its id.
pub fn write_doc(tx: &Tx, f: &ItemFacts, text: &str, now: i64) -> ApiResult<String> {
    let src = sources::builtin(tx, &f.ws, KIND, &f.user, now)?;
    let title = format!("{} {}", f.id, f.title);
    let id = match repo::doc_by_ref(tx, &src.id, &f.id)? {
        Some(d) => {
            tx.prepare_cached(
                "UPDATE kb_docs SET title = ?, size = ?, status = 'active', error = NULL, updated = ? WHERE id = ?",
            )?
            .execute(params![title, text.len() as i64, now, d.id])?;
            d.id
        }
        None => {
            let id = uid("kd");
            repo::insert_doc(
                tx,
                &f.ws,
                &NewDoc {
                    id: id.clone(),
                    source_id: src.id.clone(),
                    r#ref: f.id.clone(),
                    title,
                    size: text.len() as i64,
                    created_by: f.user.clone(),
                    ..Default::default()
                },
                now,
            )?;
            id
        }
    };
    let doc = repo::doc(tx, &id)?.ok_or_else(|| ApiError::not_found("文档"))?;
    index::index_doc(tx, &doc, &src, &markdown(text), now, false)?;
    Ok(id)
}

/// Reads the facts and the outputs off the runtime.
async fn load(m: &Modules, id: &str) -> ApiResult<Option<(ItemFacts, String)>> {
    let id = id.to_string();
    m.blocking(move |m| {
        let Some(f) = m.store.reader(|c| facts(c, &id))? else {
            return Ok(None);
        };
        let text = doc_text(&f);
        Ok(Some((f, text)))
    })
    .await
}

/// After `finish`: (re)builds the item's document, then extracts candidates once for this completion.
/// Returns the candidates written (none if this completion was already extracted).
pub async fn finished(m: &Modules, kb: &Knowledge, item_id: &str) -> ApiResult<Vec<Mem>> {
    let Some((f, text)) = load(m, item_id).await? else {
        return Ok(Vec::new());
    };
    let now = m.clock.now();
    let mark = format!("extract:item:{}", f.id);
    let stamp = f.stamp.to_string();
    // chunking the document happens inside the write: off the runtime
    let (f, doc, fresh) = m
        .blocking(move |m| {
            let (doc, fresh) = m.store.transaction(|tx| {
                let doc = write_doc(tx, &f, &text, now)?;
                let fresh = repo::mark(tx, &f.ws, &mark)?.as_deref() != Some(stamp.as_str());
                if fresh {
                    repo::set_mark(tx, &f.ws, &mark, &stamp, now)?;
                }
                Ok::<_, ApiError>((doc, fresh))
            })?;
            Ok((f, doc, fresh))
        })
        .await?;
    kb.notify(&f.ws, &doc);
    if !fresh {
        return Ok(Vec::new());
    }
    let origin = Origin {
        ws: f.ws.clone(),
        kind: OriginKind::Item,
        id: f.id.clone(),
        label: format!("{} 交付时", f.id),
        user: f.user.clone(),
        writer: "steve".into(),
        default_level: Level::Conclusion,
        item_id: Some(f.id.clone()),
        session_id: None,
    };
    let task = extract::Task {
        goals: &f.goals,
        events: false,
    };
    extract::run(m, kb, &origin, &extract_text(&f), Some(&f.id), task).await
}

/// The item was deleted: its document and index rows go.
pub fn item_deleted(tx: &Tx, ws: &str, item_id: &str) -> ApiResult<bool> {
    let Some(src) = repo::source_of_kind(tx, ws, KIND)? else {
        return Ok(false);
    };
    match repo::doc_by_ref(tx, &src.id, item_id)? {
        Some(d) => {
            index::delete_doc(tx, &d.id)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Re-reads an item document for the index queue (reindexing, scope changes).
pub struct ItemsLoader;

impl DocLoader for ItemsLoader {
    fn kinds(&self) -> &[&str] {
        &[KIND]
    }
    fn load<'a>(
        &'a self,
        m: &'a Modules,
        doc: &'a DocRow,
        _: &'a SourceRow,
    ) -> BoxFuture<'a, Loaded> {
        Box::pin(async move {
            match load(m, &doc.r#ref).await {
                Ok(Some((_, text))) => Loaded::Text(markdown(&text)),
                Ok(None) => Loaded::Gone,
                Err(e) => Loaded::Failed(format!("读不到这个事项：{e}")),
            }
        })
    }
}

/// The knowledge module's item hooks: `on_finished` builds the document and extracts candidates in a
/// spawned task. Register with `registry.item_hooks(Box::new(hooks.clone()))` and call
/// [`KnowledgeHooks::bind`] once the `Modules` exist (the registry is built before them).
#[derive(Clone)]
pub struct KnowledgeHooks {
    kb: Knowledge,
    m: Arc<OnceLock<Modules>>,
}

impl KnowledgeHooks {
    pub fn new(kb: Knowledge) -> Self {
        Self {
            kb,
            m: Arc::default(),
        }
    }

    /// Connects the hooks to the module state. Later calls are ignored.
    pub fn bind(&self, m: Modules) {
        let _ = self.m.set(m);
    }
}

#[async_trait]
impl ItemHooks for KnowledgeHooks {
    fn name(&self) -> &'static str {
        "knowledge"
    }

    async fn on_finished(&self, item: &Item) {
        let Some(m) = self.m.get().cloned() else {
            tracing::warn!(item = %item.id, "knowledge hooks not bound");
            return;
        };
        let kb = self.kb.clone();
        let id = item.id.clone();
        tokio::spawn(async move {
            if let Err(e) = finished(&m, &kb, &id).await {
                tracing::warn!(item = %id, "kb items: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_small_text_outputs_inside_cwd() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(cwd.join("notes.md"), "交付的说明文字").unwrap();
        std::fs::write(cwd.join("bin.dat"), [0u8, 1, 2]).unwrap();
        std::fs::write(
            cwd.join("big.txt"),
            "x".repeat(MAX_OUTPUT_BYTES as usize + 1),
        )
        .unwrap();
        std::fs::write(root.path().join("outside.md"), "不该读到").unwrap();
        let f = ItemFacts {
            id: "ST-1".into(),
            title: "测试".into(),
            cwd: Some(cwd.to_string_lossy().into()),
            goals: vec!["旧目标".into(), "新目标".into()],
            summary: "做完了".into(),
            outputs: ["notes.md", "bin.dat", "big.txt", "../outside.md"]
                .iter()
                .map(|l| ("doc".to_string(), format!("产出 {l}"), Some(l.to_string())))
                .collect(),
            ..Default::default()
        };
        let t = doc_text(&f);
        assert!(t.starts_with("# ST-1 测试\n"));
        assert!(t.contains("## 目标\n\n新目标") && !t.contains("旧目标"));
        assert!(t.contains("## 交付摘要\n\n做完了"));
        assert!(t.contains("- 产出 ../outside.md：../outside.md"));
        assert!(t.contains("## 产出 notes.md\n\n交付的说明文字"));
        assert!(!t.contains("## 产出 bin.dat") && !t.contains("## 产出 big.txt"));
        assert!(!t.contains("不该读到"));
    }
}
