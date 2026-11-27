//! Harness-native settings merged into a session's isolated state directory before launch.
//!
//! Only the session's own isolated state file is written (atomically via rename, never following
//! symlinks); the owner's ambient home settings are never modified. Control-plane isolation is
//! enforced by Steve's OS sandbox (`nested_sandbox = true`), so this module only merges Steve's
//! declared MCP allow rules (`mcp(steve/*)`) while preserving user rules.

use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::Path;

use serde_json::{Map, Value};
use steve_harness_spec::StateSettings;

use crate::attachments::publish_private;

const SETTINGS_FILE: &str = "settings.json";
const CONFIG_DIR: &str = "config";
const CONFIG_FILE: &str = "config.json";
/// Upper bound on existing settings/config files read before merging (matches `harness_login`).
const MAX_SETTINGS: u64 = 4 * 1024 * 1024;

fn kind_label(kind: StateSettings) -> &'static str {
    match kind {
        StateSettings::None => "none",
        StateSettings::AntigravitySettings => "antigravity_settings",
    }
}

fn record_metric(kind: StateSettings, file: &'static str, outcome: &'static str) {
    metrics::counter!(
        "steve_node_harness_settings_total",
        "kind" => kind_label(kind),
        "file" => file,
        "outcome" => outcome,
    )
    .increment(1);
}

enum ExistingFile {
    Absent,
    Regular(Vec<u8>),
    Skipped(String),
}

fn read_bounded_regular(f: fs::File) -> Result<Vec<u8>, std::io::Error> {
    let meta = f.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    if meta.len() > MAX_SETTINGS {
        return Err(std::io::Error::other("exceeds size limit"));
    }
    let mut buf = Vec::with_capacity(meta.len() as usize);
    f.take(MAX_SETTINGS + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_SETTINGS {
        return Err(std::io::Error::other("exceeds size limit"));
    }
    Ok(buf)
}

fn read_regular_file_bytes(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        read_bounded_regular(f)
    }
    #[cfg(not(unix))]
    {
        let f = OpenOptions::new().read(true).open(path)?;
        read_bounded_regular(f)
    }
}

fn read_existing_file(path: &Path) -> Result<ExistingFile, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ExistingFile::Absent),
        Err(e) => Err(format!("{}: {e}", path.display())),
        Ok(m) if !m.is_file() || m.len() > MAX_SETTINGS => Ok(ExistingFile::Skipped(
            "not a regular file of its own here".into(),
        )),
        Ok(_) => match read_regular_file_bytes(path) {
            Ok(bytes) => Ok(ExistingFile::Regular(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ExistingFile::Absent),
            Err(e) => {
                #[cfg(unix)]
                if e.raw_os_error() == Some(libc::ELOOP) {
                    return Ok(ExistingFile::Skipped(
                        "not a regular file of its own here".into(),
                    ));
                }
                if e.kind() == std::io::ErrorKind::Other {
                    return Ok(ExistingFile::Skipped(e.to_string()));
                }
                Err(format!("{}: {e}", path.display()))
            }
        },
    }
}

fn read_owner_config_json(owner_config_dir: &Path, notes: &mut Vec<String>) -> Map<String, Value> {
    match fs::symlink_metadata(owner_config_dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Map::new(),
        Err(e) => {
            notes.push(format!("{CONFIG_FILE}: owner config not readable ({e})"));
            return Map::new();
        }
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            notes.push(format!(
                "{CONFIG_FILE}: owner config directory is not a regular directory, not imported"
            ));
            return Map::new();
        }
        Ok(_) => {}
    }

    let src = owner_config_dir.join(CONFIG_FILE);
    match read_existing_file(&src) {
        Ok(ExistingFile::Absent) => Map::new(),
        Ok(ExistingFile::Skipped(why)) => {
            notes.push(format!("{CONFIG_FILE}: owner file skipped ({why})"));
            Map::new()
        }
        Ok(ExistingFile::Regular(raw)) => match parse_json_object(&raw) {
            Ok(obj) => obj,
            Err(why) => {
                notes.push(format!(
                    "{CONFIG_FILE}: owner file not readable here ({why}), not imported"
                ));
                Map::new()
            }
        },
        Err(why) => {
            notes.push(format!("{CONFIG_FILE}: owner file skipped ({why})"));
            Map::new()
        }
    }
}

/// Applies harness-declared [`StateSettings`] inside `state_root` / `target` (the session's
/// isolated harness root and settings directory). `owner_state_root` is the owner's ambient state
/// root (matching `state_root`, e.g. `~/.gemini`) when `import_owner` is enabled. Malformed or
/// non-regular files are skipped with informational notes without blocking session startup.
pub(crate) fn apply(
    state_root: &Path,
    target: &Path,
    kind: StateSettings,
    owner_state_root: Option<&Path>,
) -> Result<Vec<String>, String> {
    match kind {
        StateSettings::None => Ok(Vec::new()),
        StateSettings::AntigravitySettings => {
            apply_antigravity(state_root, target, kind, owner_state_root)
        }
    }
}

fn apply_antigravity(
    state_root: &Path,
    target: &Path,
    kind: StateSettings,
    owner_state_root: Option<&Path>,
) -> Result<Vec<String>, String> {
    let steve_allow = steve_vault::harness::antigravity_allow_rules();
    let mut notes = Vec::new();
    let mut carried = PermissionRules::default();

    // Part A: <target>/settings.json -> permissions.allow (CLI/TUI/print mode and carried over
    // into config/config.json for ACP).
    let dst = target.join(SETTINGS_FILE);
    let settings_state = match read_existing_file(&dst) {
        Err(e) => {
            record_metric(kind, SETTINGS_FILE, "skipped");
            notes.push(format!("{SETTINGS_FILE}: {e}, left alone"));
            None
        }
        Ok(ExistingFile::Absent) => Some((Map::new(), false)),
        Ok(ExistingFile::Skipped(why)) => {
            record_metric(kind, SETTINGS_FILE, "skipped");
            notes.push(format!("{SETTINGS_FILE}: {why}, left alone"));
            None
        }
        Ok(ExistingFile::Regular(raw)) => match parse_json_object(&raw) {
            Ok(obj) => Some((obj, true)),
            Err(why) => {
                record_metric(kind, SETTINGS_FILE, "skipped");
                notes.push(format!(
                    "{SETTINGS_FILE}: not readable here ({why}), left alone"
                ));
                None
            }
        },
    };

    if let Some((mut doc, existed)) = settings_state {
        carried = extract_permission_lists(&doc);
        match merge_top_permissions(&mut doc, &steve_allow) {
            Err(why) => {
                record_metric(kind, SETTINGS_FILE, "skipped");
                notes.push(format!("{SETTINGS_FILE}: {why}, left alone"));
            }
            Ok(changed) => {
                if !changed && existed {
                    record_metric(kind, SETTINGS_FILE, "unchanged");
                } else {
                    let bytes = serialize_json_pretty(&doc).inspect_err(|_| {
                        record_metric(kind, SETTINGS_FILE, "error");
                    })?;
                    if let Err(e) = publish_private(&dst, &bytes) {
                        record_metric(kind, SETTINGS_FILE, "error");
                        return Err(e);
                    }
                    record_metric(kind, SETTINGS_FILE, "updated");
                }
            }
        }
    }

    // Part B: <state_root>/config/config.json -> userSettings.globalPermissionGrants
    // (enforced by `agy acp`).
    let config_dir = state_root.join(CONFIG_DIR);
    if let Err(e) = crate::attachments::private_dir(&config_dir) {
        record_metric(kind, CONFIG_FILE, "skipped");
        notes.push(format!("{CONFIG_FILE}: {e}, left alone"));
        return Ok(notes);
    }
    let owner_config_dir = owner_state_root.map(|root| root.join(CONFIG_DIR));
    let config_dst = config_dir.join(CONFIG_FILE);
    let (mut cfg_doc, cfg_existed) = match read_existing_file(&config_dst) {
        Err(e) => {
            record_metric(kind, CONFIG_FILE, "skipped");
            notes.push(format!("{CONFIG_FILE}: {e}, left alone"));
            return Ok(notes);
        }
        Ok(ExistingFile::Absent) => {
            let initial = owner_config_dir
                .as_deref()
                .map(|src_dir| read_owner_config_json(src_dir, &mut notes))
                .unwrap_or_default();
            (initial, false)
        }
        Ok(ExistingFile::Skipped(why)) => {
            record_metric(kind, CONFIG_FILE, "skipped");
            notes.push(format!("{CONFIG_FILE}: {why}, left alone"));
            return Ok(notes);
        }
        Ok(ExistingFile::Regular(raw)) => match parse_json_object(&raw) {
            Ok(obj) => (obj, true),
            Err(why) => {
                record_metric(kind, CONFIG_FILE, "skipped");
                notes.push(format!(
                    "{CONFIG_FILE}: not readable here ({why}), left alone"
                ));
                return Ok(notes);
            }
        },
    };

    let mut cfg_allow = carried.allow;
    cfg_allow.extend(steve_allow);
    match merge_global_permission_grants(&mut cfg_doc, &cfg_allow, &carried.ask, &carried.deny) {
        Err(why) => {
            record_metric(kind, CONFIG_FILE, "skipped");
            notes.push(format!("{CONFIG_FILE}: {why}, left alone"));
        }
        Ok(changed) => {
            if !changed && cfg_existed {
                record_metric(kind, CONFIG_FILE, "unchanged");
                return Ok(notes);
            }
            let bytes = serialize_json_pretty(&cfg_doc).inspect_err(|_| {
                record_metric(kind, CONFIG_FILE, "error");
            })?;
            if let Err(e) = publish_private(&config_dst, &bytes) {
                record_metric(kind, CONFIG_FILE, "error");
                return Err(e);
            }
            record_metric(kind, CONFIG_FILE, "updated");
        }
    }

    Ok(notes)
}

fn serialize_json_pretty(doc: &Map<String, Value>) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec_pretty(doc).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn parse_json_object(raw: &[u8]) -> Result<Map<String, Value>, String> {
    let text = std::str::from_utf8(raw).map_err(|_| "not UTF-8".to_string())?;
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(obj)) => Ok(obj),
        Ok(_) => Err("not an object".into()),
        Err(e) => Err(e.to_string()),
    }
}

fn extract_string_list(obj: &Map<String, Value>, key: &str) -> Vec<String> {
    obj.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Default)]
struct PermissionRules {
    allow: Vec<String>,
    ask: Vec<String>,
    deny: Vec<String>,
}

fn extract_permission_lists(doc: &Map<String, Value>) -> PermissionRules {
    let Some(perms) = doc.get("permissions").and_then(Value::as_object) else {
        return PermissionRules::default();
    };
    PermissionRules {
        allow: extract_string_list(perms, "allow"),
        ask: extract_string_list(perms, "ask"),
        deny: extract_string_list(perms, "deny"),
    }
}

fn ensure_object_field<'a>(
    parent: &'a mut Map<String, Value>,
    key: &str,
    label: &str,
    changed: &mut bool,
) -> Result<&'a mut Map<String, Value>, String> {
    if !parent.contains_key(key) || parent.get(key).is_some_and(Value::is_null) {
        parent.insert(key.to_string(), Value::Object(Map::new()));
        *changed = true;
    }
    parent
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| format!("{label} is not an object"))
}

/// Ensures `perms["allow"]` (and `perms["ask"]`/`perms["deny"]` when non-empty) include every rule
/// without removing, reordering, or duplicating existing entries.
fn merge_permission_object(
    perms: &mut Map<String, Value>,
    allow: &[String],
    ask: &[String],
    deny: &[String],
    prefix: &str,
) -> Result<bool, String> {
    let mut changed = false;
    for (key, rules, always_create) in [
        ("allow", allow, true),
        ("ask", ask, false),
        ("deny", deny, false),
    ] {
        if rules.is_empty() && !always_create {
            continue;
        }
        if !perms.contains_key(key) || perms.get(key).is_some_and(Value::is_null) {
            perms.insert(key.to_string(), Value::Array(Vec::new()));
            changed = true;
        }
        let Some(arr) = perms.get_mut(key).and_then(Value::as_array_mut) else {
            return Err(format!("{prefix}.{key} is not an array"));
        };
        for rule in rules {
            if !arr.iter().any(|v| v.as_str() == Some(rule.as_str())) {
                arr.push(Value::String(rule.clone()));
                changed = true;
            }
        }
    }
    Ok(changed)
}

fn merge_top_permissions(doc: &mut Map<String, Value>, allow: &[String]) -> Result<bool, String> {
    let mut changed = false;
    let perms = ensure_object_field(doc, "permissions", "permissions", &mut changed)?;
    let merged = merge_permission_object(perms, allow, &[], &[], "permissions")?;
    Ok(changed || merged)
}

fn merge_global_permission_grants(
    doc: &mut Map<String, Value>,
    allow: &[String],
    ask: &[String],
    deny: &[String],
) -> Result<bool, String> {
    let mut changed = false;
    let user_settings = ensure_object_field(doc, "userSettings", "userSettings", &mut changed)?;
    let grants = ensure_object_field(
        user_settings,
        "globalPermissionGrants",
        "userSettings.globalPermissionGrants",
        &mut changed,
    )?;
    let merged = merge_permission_object(
        grants,
        allow,
        ask,
        deny,
        "userSettings.globalPermissionGrants",
    )?;
    Ok(changed || merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merges_mcp_allow_preserves_user_rules_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("state");
        let target = state_root.join("antigravity-cli");
        let owner_state_root = dir.path().join("owner/.gemini");
        let owner_config_dir = owner_state_root.join(CONFIG_DIR);
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&owner_config_dir).unwrap();

        let owner_cfg_raw = serde_json::to_vec_pretty(&json!({
            "plugins": { "swarm": { "enabled": true } },
            "userSettings": {
                "globalPermissionGrants": {
                    "allow": ["read_url(example.com)"],
                    "deny": ["command(rm -rf /)"]
                }
            }
        }))
        .unwrap();
        let owner_cfg_path = owner_config_dir.join(CONFIG_FILE);
        fs::write(&owner_cfg_path, &owner_cfg_raw).unwrap();

        let settings_path = target.join(SETTINGS_FILE);
        fs::write(
            &settings_path,
            serde_json::to_vec_pretty(&json!({
                "theme": "dark",
                "permissions": {
                    "allow": ["command(git status)"],
                    "ask": ["mcp(steve/steve_deliver)"],
                    "deny": ["command(sudo)"]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let notes = apply(
            &state_root,
            &target,
            StateSettings::AntigravitySettings,
            Some(&owner_state_root),
        )
        .unwrap();
        assert!(notes.is_empty(), "{notes:?}");

        let cfg_path = state_root.join(CONFIG_DIR).join(CONFIG_FILE);
        #[cfg(unix)]
        for path in [&settings_path, &cfg_path] {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let settings_bytes = fs::read(&settings_path).unwrap();
        let settings_mtime = fs::metadata(&settings_path).unwrap().modified().unwrap();
        let val: Value = serde_json::from_slice(&settings_bytes).unwrap();
        assert_eq!(val["theme"], "dark");
        assert_eq!(
            val["permissions"]["allow"],
            json!(["command(git status)", "mcp(steve/*)"])
        );
        assert_eq!(
            val["permissions"]["ask"],
            json!(["mcp(steve/steve_deliver)"])
        );
        assert_eq!(val["permissions"]["deny"], json!(["command(sudo)"]));

        let cfg_bytes = fs::read(&cfg_path).unwrap();
        let cfg_mtime = fs::metadata(&cfg_path).unwrap().modified().unwrap();
        let cfg_val: Value = serde_json::from_slice(&cfg_bytes).unwrap();
        assert_eq!(cfg_val["plugins"]["swarm"]["enabled"], json!(true));
        let grants = &cfg_val["userSettings"]["globalPermissionGrants"];
        assert_eq!(
            grants["allow"],
            json!([
                "read_url(example.com)",
                "command(git status)",
                "mcp(steve/*)"
            ])
        );
        assert_eq!(grants["ask"], json!(["mcp(steve/steve_deliver)"]));
        assert_eq!(
            grants["deny"],
            json!(["command(rm -rf /)", "command(sudo)"])
        );
        assert_eq!(fs::read(&owner_cfg_path).unwrap(), owner_cfg_raw);

        // Second apply must be a complete no-op on both files.
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(
            apply(
                &state_root,
                &target,
                StateSettings::AntigravitySettings,
                Some(&owner_state_root),
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(fs::read(&settings_path).unwrap(), settings_bytes);
        assert_eq!(
            fs::metadata(&settings_path).unwrap().modified().unwrap(),
            settings_mtime
        );
        assert_eq!(fs::read(&cfg_path).unwrap(), cfg_bytes);
        assert_eq!(
            fs::metadata(&cfg_path).unwrap().modified().unwrap(),
            cfg_mtime
        );
    }

    #[cfg(unix)]
    #[test]
    fn skips_symlinks_and_malformed_files_without_blocking_session() {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("state");
        let target = state_root.join("antigravity-cli");
        let cfg_dir = state_root.join(CONFIG_DIR);
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&cfg_dir).unwrap();

        let outside = dir.path().join("outside.json");
        fs::write(&outside, r#"{"outside":true}"#).unwrap();

        let settings_link = target.join(SETTINGS_FILE);
        std::os::unix::fs::symlink(&outside, &settings_link).unwrap();
        let cfg_path = cfg_dir.join(CONFIG_FILE);

        // Symlinked settings.json is skipped without following; config.json still receives mcp(steve/*).
        let notes = apply(
            &state_root,
            &target,
            StateSettings::AntigravitySettings,
            None,
        )
        .unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            fs::symlink_metadata(&settings_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), r#"{"outside":true}"#);
        let cfg_val: Value = serde_json::from_slice(&fs::read(&cfg_path).unwrap()).unwrap();
        assert_eq!(
            cfg_val["userSettings"]["globalPermissionGrants"]["allow"],
            json!(["mcp(steve/*)"])
        );
        assert!(
            cfg_val["userSettings"]["globalPermissionGrants"]
                .get("deny")
                .is_none()
        );

        // Malformed files are left alone with notes and never fail apply.
        fs::remove_file(&settings_link).unwrap();
        fs::write(&settings_link, "{not json").unwrap();
        fs::write(&cfg_path, r#"{"userSettings":"bad"}"#).unwrap();
        let notes = apply(
            &state_root,
            &target,
            StateSettings::AntigravitySettings,
            None,
        )
        .unwrap();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!(fs::read_to_string(&settings_link).unwrap(), "{not json");
        assert_eq!(
            fs::read_to_string(&cfg_path).unwrap(),
            r#"{"userSettings":"bad"}"#
        );
    }

    #[test]
    fn none_state_settings_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("state");
        let target = state_root.join("antigravity-cli");
        fs::create_dir_all(&target).unwrap();
        let notes = apply(&state_root, &target, StateSettings::None, None).unwrap();
        assert!(notes.is_empty());
        assert!(!target.join(SETTINGS_FILE).exists());
        assert!(!state_root.join(CONFIG_DIR).join(CONFIG_FILE).exists());
    }
}
