//! `/api/projects*` and `/api/fs/dirs`.

use std::collections::HashMap;
use std::path::Path as FsPath;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use serde_json::{Value, json};

use super::misc::FAST;
use super::{ApiResult, AppState, Ctx, blocking, body, ok, own, own_bot};
use crate::backend::{NewProject, Obj};
use crate::config::home_dir;
use crate::error::ApiError;
use crate::js;
use crate::validate::{Check, Str};

/// `path.normalize`: folds `.`/`..` and repeated slashes, keeps a trailing slash.
pub fn normalize(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let abs = p.starts_with('/');
    let trailing = p.ends_with('/');
    let mut parts: Vec<&str> = vec![];
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else if !abs {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    let mut out = parts.join("/");
    if abs {
        out.insert(0, '/');
    }
    if out.is_empty() {
        return if abs { "/".into() } else { ".".into() };
    }
    if trailing && !out.ends_with('/') {
        out.push('/');
    }
    out
}

/// `path.join(a, b)`.
pub fn join(a: &str, b: &str) -> String {
    let joined = [a, b]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    normalize(&joined)
}

/// `~/x` → `$HOME/x`; anything else unchanged.
pub fn expand(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => join(&home_dir().to_string_lossy(), rest),
        None => p.to_string(),
    }
}

/// Node's fs error text, e.g. `ENOTDIR: not a directory, scandir '/x'`.
fn fs_error(e: &std::io::Error, syscall: &str, path: &str) -> ApiError {
    let (code, what) = match e.raw_os_error() {
        Some(20) => ("ENOTDIR", "not a directory"),
        Some(13) => ("EACCES", "permission denied"),
        Some(2) => ("ENOENT", "no such file or directory"),
        Some(21) => ("EISDIR", "illegal operation on a directory"),
        Some(1) => ("EPERM", "operation not permitted"),
        _ => return ApiError::internal(format!("{e}, {syscall} '{path}'")),
    };
    ApiError::internal(format!("{code}: {what}, {syscall} '{path}'"))
}

fn is_dir(p: &str) -> bool {
    FsPath::new(p).is_dir()
}

fn exists(p: &str) -> bool {
    FsPath::new(p).exists()
}

pub async fn create(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let name = k.str("name", Str::len(1, 40));
    let raw = k.str("path", Str::min(1));
    let description = k.opt_str("description", Str::max(1000)).unwrap_or_default();
    let worktree = k.opt_bool("worktree");
    let default_bot = match k.opt_nullable_str("default_bot", Str::ANY) {
        Some(Some(b)) => Some(b),
        _ => None,
    };
    k.finish()?;
    if let Some(bot) = default_bot.clone().filter(|b| !b.is_empty()) {
        let ws = c.ws.clone();
        st.read(move |r| Ok(own_bot(r, &bot, &ws)?)).await?;
    }
    let path = expand(&raw);
    let p = path.clone();
    let (dir_ok, git) =
        blocking(move || Ok((exists(&p) && is_dir(&p), exists(&join(&p, ".git"))))).await?;
    if !dir_ok {
        return Err(ApiError::bad_request(format!("目录不存在：{raw}")));
    }
    st.write(move |r| {
        if r.project_by_path(&c.ws, &path)?.is_some() {
            return Err(ApiError::bad_request("这个目录已经是一个项目"));
        }
        let x = r.project_create(
            &c.ws,
            &NewProject {
                name,
                description,
                device_id: "hub".into(),
                path: path.clone(),
                kind: if git { "git" } else { "dir" }.into(),
                worktree: git && worktree.unwrap_or(true),
                default_bot,
                release: None,
            },
        )?;
        let id = x
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        r.audit(
            &c.me,
            "project.create",
            &id,
            &json!({ "path": path }),
            &c.ws,
        )?;
        ok(Value::Object(x))
    })
    .await
}

pub async fn patch_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let (i, ws) = (id.clone(), c.ws.clone());
    st.read(move |r| Ok(own(r.project_get(&i)?, &ws, "项目")?))
        .await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let mut p = Obj::new();
    if let Some(x) = k.opt_str("name", Str::len(1, 40)) {
        p.insert("name".into(), json!(x));
    }
    if let Some(x) = k.opt_str("description", Str::max(1000)) {
        p.insert("description".into(), json!(x));
    }
    if let Some(x) = k.opt_bool("worktree") {
        p.insert("worktree".into(), json!(x));
    }
    if let Some(x) = k.opt_nullable_str("default_bot", Str::ANY) {
        p.insert("default_bot".into(), json!(x));
    }
    if let Some(x) = k.opt_bool("archived") {
        p.insert("archived".into(), json!(x));
    }
    k.finish()?;
    st.write(move |r| {
        if let Some(Value::String(bot)) = p.get("default_bot")
            && !bot.is_empty()
        {
            own_bot(r, bot, &c.ws)?;
        }
        ok(Value::Object(r.project_patch(&id, &p, &c.me)?))
    })
    .await
}

/// What `describe` shows the model: the file list and the start of the usual readme files.
fn describe_input(path: &str) -> Result<(String, String), ApiError> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .map_err(|e| fs_error(&e, "scandir", path))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let list = names
        .into_iter()
        .filter(|f| !f.starts_with('.'))
        .take(60)
        .collect::<Vec<_>>()
        .join(", ");
    let mut readme = vec![];
    for f in ["README.md", "readme.md", "AGENTS.md", "package.json"] {
        let p = join(path, f);
        if !exists(&p) {
            continue;
        }
        let text = std::fs::read(&p).map_err(|e| fs_error(&e, "open", &p))?;
        readme.push(format!(
            "{f}:\n{}",
            js::slice(&String::from_utf8_lossy(&text), 2500)
        ));
    }
    Ok((list, readme.join("\n\n")))
}

/// Let Steve draft a description from what is in the directory.
pub async fn describe(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let raw = k.str("path", Str::min(1));
    k.finish()?;
    let path = expand(&raw);
    let p = path.clone();
    if !blocking(move || Ok(exists(&p))).await? {
        return Err(ApiError::bad_request(format!("目录不存在：{raw}")));
    }
    let p = path.clone();
    let (list, readme) = blocking(move || describe_input(&p)).await?;
    let prompt = format!(
        "Describe this project directory for an assistant that routes tasks to projects. Chinese, one or two sentences: what it is and what kind of work happens there. ONLY JSON {{\"name\":\"<= 12 chars\",\"description\":\"\"}}\nPath: {path}\nFiles: {list}\n\n{readme}"
    );
    let reply = st
        .domain
        .claude_print("title", &c.ws, &prompt, FAST, 60_000)
        .await?;
    let r = js::first_json(&reply)?.unwrap_or_else(|| json!({}));
    let get = |k: &str| r.get(k).filter(|x| !x.is_null()).map(js::string);
    let base = FsPath::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let p = path.clone();
    let git = blocking(move || Ok(exists(&join(&p, ".git")))).await?;
    ok(
        json!({ "name": get("name").unwrap_or(base), "description": get("description").unwrap_or_default(), "git": git }),
    )
}

pub async fn dirs(Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let raw = q
        .get("path")
        .filter(|p| !p.is_empty())
        .cloned()
        .unwrap_or_else(|| "~/".into());
    let base = expand(&raw);
    let out = blocking(move || {
        if !exists(&base) {
            return Ok(vec![]);
        }
        let mut ents: Vec<(String, bool)> = std::fs::read_dir(&base)
            .map_err(|e| fs_error(&e, "scandir", &base))?
            .filter_map(|e| e.ok())
            // Dirent.isDirectory(): symlinks are not followed
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    e.file_type().is_ok_and(|t| t.is_dir()),
                )
            })
            .collect();
        ents.sort();
        let home = home_dir().to_string_lossy().into_owned();
        Ok(ents
            .into_iter()
            .filter(|(n, d)| *d && !n.starts_with('.'))
            .take(200)
            .map(|(n, _)| join(&base, &n).replacen(&home, "~", 1))
            .collect::<Vec<_>>())
    })
    .await?;
    ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_paths() {
        assert_eq!(normalize("/a//b/../c/"), "/a/c/");
        assert_eq!(normalize("/a/./b"), "/a/b");
        assert_eq!(normalize("/.."), "/");
        assert_eq!(join("/home/user", ""), "/home/user");
        assert_eq!(join("/home/user", "x/"), "/home/user/x/");
        assert_eq!(join("/home/user/", "x"), "/home/user/x");
        assert_eq!(expand("/abs"), "/abs");
        assert_eq!(expand("~x"), "~x");
    }

    #[test]
    fn describe_reads_list_and_readme() {
        let d = tempfile::tempdir().expect("tmp");
        std::fs::write(d.path().join("README.md"), "# Hi").expect("w");
        std::fs::write(d.path().join(".hidden"), "").expect("w");
        std::fs::create_dir(d.path().join("src")).expect("d");
        let (list, readme) = describe_input(&d.path().to_string_lossy()).expect("ok");
        assert_eq!(list, "README.md, src");
        assert!(readme.starts_with("README.md:\n# Hi"));
        let f = d.path().join("README.md");
        let e = describe_input(&f.to_string_lossy()).unwrap_err();
        assert_eq!(
            e,
            ApiError::internal(format!(
                "ENOTDIR: not a directory, scandir '{}'",
                f.display()
            ))
        );
    }
}
