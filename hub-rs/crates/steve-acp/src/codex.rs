//! Codex adapter compatibility (codex.ts). codex-acp embeds its own (older) Codex core but reads the
//! user's `~/.codex/config.toml`. A model catalog written for a newer Codex CLI (`model_catalog_json`)
//! does not parse there, and its `code_mode_only` tool mode starts an embedded V8 that crashes on this
//! macOS. We hand the adapter a sanitized copy instead; the user's own files are never changed.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use regex::Regex;
use serde_json::Value;

const EFFORTS: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];

struct Cached {
    src: PathBuf,
    mtime: Option<SystemTime>,
    out: PathBuf,
}
static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

static TABLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*\[").expect("static regex"));
static CATALOG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^\s*model_catalog_json\s*=\s*["']([^"']+)["']"#).expect("static regex")
});

/// The catalog path set at the top level of config.toml (before the first `[table]`), if any.
fn catalog_path(codex_home: &Path, home: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(codex_home.join("config.toml")).ok()?;
    let top = match TABLE.find(&text) {
        Some(m) => &text[..m.start()],
        None => &text[..],
    };
    let p = CATALOG.captures(top)?.get(1)?.as_str();
    Some(match p.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(p),
    })
}

fn is_effort(v: Option<&Value>) -> bool {
    v.and_then(Value::as_str)
        .is_some_and(|e| EFFORTS.contains(&e))
}

/// Rewrites a model catalog the way codex.ts `sanitize` does; returns the compact JSON text.
pub fn sanitize_catalog(mut d: Value) -> String {
    let models: Option<&mut Vec<Value>> = if d.is_array() {
        d.as_array_mut()
    } else {
        d.get_mut("models").and_then(Value::as_array_mut)
    };
    if let Some(models) = models {
        let fallback = models
            .iter()
            .filter_map(|m| m.get("base_instructions").and_then(Value::as_str))
            .find(|s| !s.is_empty())
            .unwrap_or_default()
            .to_owned();
        for m in models.iter_mut() {
            let Some(m) = m.as_object_mut() else { continue };
            if !m.get("base_instructions").is_some_and(Value::is_string) {
                m.insert("base_instructions".into(), Value::String(fallback.clone()));
            }
            m.insert("tool_mode".into(), Value::String("direct".into()));
            if let Some(Value::Array(levels)) = m.get_mut("supported_reasoning_levels") {
                levels.retain(|l| is_effort(l.get("effort")));
            }
            if m.get("default_reasoning_level")
                .is_some_and(|v| !v.is_null())
                && !is_effort(m.get("default_reasoning_level"))
            {
                m.insert(
                    "default_reasoning_level".into(),
                    Value::String("medium".into()),
                );
            }
        }
    }
    d.to_string()
}

fn write_sanitized(src: &Path, out: &Path) -> Result<(), String> {
    let text = fs::read_to_string(src).map_err(|e| e.to_string())?;
    let d: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let json = sanitize_catalog(d);
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(out)
        .map_err(|e| e.to_string())?;
    f.write_all(json.as_bytes()).map_err(|e| e.to_string())
}

/// Extra `-c key=value` arguments for codex-acp; empty when the user's config needs no help.
/// `codex_home` is `$CODEX_HOME` (default `~/.codex`), `steve_home` is `STEVE_HOME`, `home` expands `~/`.
pub fn codex_args(codex_home: &Path, steve_home: &Path, home: &Path) -> Vec<String> {
    let Some(src) = catalog_path(codex_home, home) else {
        return vec![];
    };
    let Ok(meta) = fs::metadata(&src) else {
        return vec![];
    };
    let out = steve_home.join("codex").join("models.json");
    let mtime = meta.modified().ok();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let fresh = cache
        .as_ref()
        .is_some_and(|c| c.src == src && c.mtime == mtime && c.out == out)
        && out.exists();
    if !fresh {
        if let Err(e) = write_sanitized(&src, &out) {
            tracing::error!("codex catalog {e}");
            return vec![];
        }
        *cache = Some(Cached {
            src,
            mtime,
            out: out.clone(),
        });
    }
    // JSON.stringify(out): a quoted, escaped string
    vec![
        "-c".into(),
        format!(
            "model_catalog_json={}",
            Value::String(out.display().to_string())
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sanitizes_models() {
        let d = json!({"models": [
            {"slug": "a", "tool_mode": "code_mode_only", "supported_reasoning_levels": [{"effort": "low"}, {"effort": "ultra"}], "default_reasoning_level": "ultra"},
            {"slug": "b", "base_instructions": "be good", "default_reasoning_level": "high"},
            {"slug": "c", "base_instructions": 3, "default_reasoning_level": null}
        ]});
        let out: Value = serde_json::from_str(&sanitize_catalog(d)).unwrap();
        let m = &out["models"];
        assert_eq!(m[0]["base_instructions"], "be good");
        assert_eq!(m[0]["tool_mode"], "direct");
        assert_eq!(
            m[0]["supported_reasoning_levels"],
            json!([{"effort": "low"}])
        );
        assert_eq!(m[0]["default_reasoning_level"], "medium");
        assert_eq!(m[1]["default_reasoning_level"], "high");
        assert_eq!(m[2]["base_instructions"], "be good");
        assert_eq!(m[2]["default_reasoning_level"], Value::Null);
        // array root, no instructions anywhere
        let out: Value = serde_json::from_str(&sanitize_catalog(json!([{"slug": "x"}]))).unwrap();
        assert_eq!(out[0]["base_instructions"], "");
    }

    #[test]
    fn args_from_config() {
        let t = tempfile::tempdir().unwrap();
        let (codex, steve, home) = (
            t.path().join("codex"),
            t.path().join("steve"),
            t.path().join("home"),
        );
        fs::create_dir_all(&codex).unwrap();
        fs::create_dir_all(&home).unwrap();
        assert!(codex_args(&codex, &steve, &home).is_empty());
        // only the top level counts
        fs::write(
            codex.join("config.toml"),
            "model = \"x\"\n[profiles.a]\nmodel_catalog_json = \"~/cat.json\"\n",
        )
        .unwrap();
        assert!(codex_args(&codex, &steve, &home).is_empty());
        fs::write(
            codex.join("config.toml"),
            "model_catalog_json = \"~/cat.json\"\n[x]\n",
        )
        .unwrap();
        assert!(
            codex_args(&codex, &steve, &home).is_empty(),
            "catalog file missing"
        );
        fs::write(
            home.join("cat.json"),
            r#"{"models":[{"slug":"m","tool_mode":"code_mode_only"}]}"#,
        )
        .unwrap();
        let out = steve.join("codex/models.json");
        assert_eq!(
            codex_args(&codex, &steve, &home),
            vec![
                "-c".to_string(),
                format!("model_catalog_json=\"{}\"", out.display())
            ]
        );
        let written: Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(written["models"][0]["tool_mode"], "direct");
        assert_eq!(
            fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // a broken catalog gives no args
        fs::write(home.join("cat.json"), "{not json").unwrap();
        let t2 = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(home.join("cat.json"))
            .unwrap()
            .set_modified(t2)
            .unwrap();
        assert!(codex_args(&codex, &steve, &home).is_empty());
    }

    use std::os::unix::fs::PermissionsExt;
}
