//! Harness-native settings merged into a session's isolated state directory before launch.
//!
//! Only the session's own isolated state file is written (atomically via rename, never following
//! symlinks); the owner's ambient home settings are never modified.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use steve_confine::{NODE_PROTECTED, PROTECTED, canonical_path};
use steve_harness_spec::StateSettings;

use crate::attachments::publish_private;

/// Harness settings file name inside the session's isolated state target directory.
const SETTINGS_FILE: &str = "settings.json";
/// Shared CLI/ACP config directory and file name under the session's isolated state root.
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

fn read_regular_file_bytes(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let meta = f.metadata()?;
        if !meta.is_file() {
            return Err(std::io::Error::other("not a regular file"));
        }
        if meta.len() > MAX_SETTINGS {
            return Err(std::io::Error::other("exceeds size limit"));
        }
        let mut buf = Vec::with_capacity(meta.len() as usize);
        f.read_to_end(&mut buf)?;
        if buf.len() as u64 > MAX_SETTINGS {
            return Err(std::io::Error::other("exceeds size limit"));
        }
        Ok(buf)
    }
    #[cfg(not(unix))]
    {
        fs::read(path)
    }
}

fn read_existing_file(path: &Path) -> Result<ExistingFile, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ExistingFile::Absent),
        Err(e) => Err(format!("{}: {e}", path.display())),
        Ok(m) if m.is_dir() => Err(format!("{} is a directory", path.display())),
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
        Ok(m) if !m.is_dir() => {
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
/// root (matching `state_root`, e.g. `~/.gemini`) when `import_owner` is enabled. Returns
/// informational notes when an existing file cannot be safely updated and is left alone.
pub(crate) fn apply(
    state_root: &Path,
    target: &Path,
    kind: StateSettings,
    steve_home: &Path,
    node_home: &Path,
    owner_state_root: Option<&Path>,
) -> Result<Vec<String>, String> {
    match kind {
        StateSettings::None => Ok(Vec::new()),
        StateSettings::AntigravitySettings => apply_antigravity(
            state_root,
            target,
            kind,
            steve_home,
            node_home,
            owner_state_root,
        ),
    }
}

fn apply_antigravity(
    state_root: &Path,
    target: &Path,
    kind: StateSettings,
    steve_home: &Path,
    node_home: &Path,
    owner_state_root: Option<&Path>,
) -> Result<Vec<String>, String> {
    let steve_allow = steve_vault::harness::antigravity_allow_rules();
    let mut steve_deny = steve_vault::harness::antigravity_deny_rules(steve_home, node_home);
    collect_prefix_matches(steve_home, node_home, &mut steve_deny);

    let mut notes = Vec::new();
    let mut carried = PermissionRules::default();

    // Part A: <target>/settings.json -> permissions.{allow, ask, deny} (CLI/TUI/print mode and
    // carried over into config/config.json for ACP).
    let dst = target.join(SETTINGS_FILE);
    let settings_state = match read_existing_file(&dst) {
        Err(e) => {
            record_metric(kind, SETTINGS_FILE, "error");
            return Err(e);
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
        match merge_top_permissions(&mut doc, &steve_allow, &steve_deny) {
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

    // Part B: <state_root>/config/config.json -> userSettings.globalPermissionGrants.{allow, ask, deny}
    // (enforced by `agy acp`).
    let config_dir = state_root.join(CONFIG_DIR);
    if let Err(e) = crate::attachments::private_dir(&config_dir) {
        record_metric(kind, CONFIG_FILE, "error");
        return Err(format!("{}: {e}", config_dir.display()));
    }
    let owner_config_dir = owner_state_root.map(|root| root.join(CONFIG_DIR));
    let config_dst = config_dir.join(CONFIG_FILE);
    let (mut cfg_doc, cfg_existed) = match read_existing_file(&config_dst) {
        Err(e) => {
            record_metric(kind, CONFIG_FILE, "error");
            return Err(e);
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
    let cfg_ask = carried.ask;
    let mut cfg_deny = carried.deny;
    cfg_deny.extend(steve_deny);

    match merge_global_permission_grants(&mut cfg_doc, &cfg_allow, &cfg_ask, &cfg_deny) {
        Err(why) => {
            record_metric(kind, CONFIG_FILE, "skipped");
            notes.push(format!("{CONFIG_FILE}: {why}, left alone"));
            return Ok(notes);
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

/// Ensures `perms["allow"]` and `perms["deny"]` (and `perms["ask"]` when `ask` is non-empty)
/// contain every rule without removing, reordering, or duplicating existing entries.
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
        ("deny", deny, true),
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

/// Ensures `doc["permissions"]["allow"]` and `doc["permissions"]["deny"]` include every rule in
/// `allow` / `deny` without removing, reordering, or overwriting existing entries or `ask`.
fn merge_top_permissions(
    doc: &mut Map<String, Value>,
    allow: &[String],
    deny: &[String],
) -> Result<bool, String> {
    let mut changed = false;
    let perms = ensure_object_field(doc, "permissions", "permissions", &mut changed)?;
    let merged = merge_permission_object(perms, allow, &[], deny, "permissions")?;
    Ok(changed || merged)
}

/// Ensures `doc["userSettings"]["globalPermissionGrants"]` includes every rule in `allow`, `ask`,
/// and `deny` while preserving existing user settings (`plugins`, etc.) and rules.
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

fn distinct_roots(path: &Path) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    let real = canonical_path(path);
    if real != out[0] {
        out.push(real);
    }
    out
}

/// Appends deny rules for concrete on-disk files matching wildcard-prefix entries (`steve.env*`,
/// `data/steve.sqlite3*`, `node.sqlite3*`), so custom suffixes present on disk are also blocked.
fn collect_prefix_matches(steve_home: &Path, node_home: &Path, deny: &mut Vec<String>) {
    let mut seen: HashSet<String> = deny.iter().cloned().collect();
    let mut push_unique = |rule: String| {
        if seen.insert(rule.clone()) {
            deny.push(rule);
        }
    };

    let mut scan_prefixes = |root: &Path, entries: &[&str], skip: &[&str]| {
        for entry in entries {
            if skip.contains(entry) {
                continue;
            }
            let Some(prefix_rel) = entry.strip_suffix('*') else {
                continue;
            };
            let rel = Path::new(prefix_rel);
            let Some(file_prefix) = rel.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let dir = match rel.parent() {
                Some(p) if !p.as_os_str().is_empty() => root.join(p),
                _ => root.to_path_buf(),
            };
            let Ok(rd) = fs::read_dir(&dir) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = rd
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with(file_prefix))
                })
                .map(|e| e.path())
                .collect();
            paths.sort();
            for path in paths {
                if let Some([read, write]) =
                    steve_vault::harness::antigravity_path_deny_rules(&path)
                {
                    push_unique(read);
                    push_unique(write);
                }
            }
        }
    };

    for root in distinct_roots(steve_home) {
        scan_prefixes(&root, &PROTECTED, &[]);
    }
    for root in distinct_roots(node_home) {
        scan_prefixes(
            &root,
            &NODE_PROTECTED,
            &steve_vault::harness::ANTIGRAVITY_OPEN_NODE_DIRS,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn creates_settings_and_config_with_0600_permissions_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("state");
        let target = state_root.join("antigravity-cli");
        let steve_home = dir.path().join("steve");
        let node_home = steve_home.join("data/node");
        fs::create_dir_all(&target).unwrap();

        let notes = apply(
            &state_root,
            &target,
            StateSettings::AntigravitySettings,
            &steve_home,
            &node_home,
            None,
        )
        .unwrap();
        assert!(notes.is_empty(), "{notes:?}");

        for path in [
            target.join(SETTINGS_FILE),
            state_root.join(CONFIG_DIR).join(CONFIG_FILE),
        ] {
            assert!(path.is_file(), "{}", path.display());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{}", path.display());
            }
        }

        let val: Value =
            serde_json::from_slice(&fs::read(target.join(SETTINGS_FILE)).unwrap()).unwrap();
        assert_eq!(val["permissions"]["allow"], json!(["mcp(steve/*)"]));
        assert!(val["permissions"].get("ask").is_none());
        let deny = val["permissions"]["deny"].as_array().unwrap();
        assert!(deny.contains(&json!(format!(
            "read_file({}/signing)",
            steve_home.display()
        ))));
        assert!(deny.contains(&json!(format!(
            "write_file({}/releases)",
            steve_home.display()
        ))));

        let cfg_val: Value = serde_json::from_slice(
            &fs::read(state_root.join(CONFIG_DIR).join(CONFIG_FILE)).unwrap(),
        )
        .unwrap();
        let grants = &cfg_val["userSettings"]["globalPermissionGrants"];
        assert_eq!(grants["allow"], json!(["mcp(steve/*)"]));
        assert!(grants.get("ask").is_none());
        let cfg_deny = grants["deny"].as_array().unwrap();
        assert!(cfg_deny.contains(&json!(format!(
            "read_file({}/signing)",
            steve_home.display()
        ))));
        assert!(cfg_deny.contains(&json!(format!(
            "write_file({}/releases)",
            steve_home.display()
        ))));
    }

    #[test]
    fn imports_owner_config_preserves_user_rules_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("state");
        let target = state_root.join("antigravity-cli");
        let owner_state_root = dir.path().join("owner/.gemini");
        let owner_config_dir = owner_state_root.join(CONFIG_DIR);
        let steve_home = dir.path().join("steve");
        let node_home = steve_home.join("data/node");
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

        let initial_settings = json!({
            "theme": "dark",
            "model": "gemini-4-neon",
            "permissions": {
                "allow": ["command(git status)"],
                "ask": ["mcp(steve/steve_deliver)"],
                "deny": ["command(sudo)"]
            }
        });
        let settings_path = target.join(SETTINGS_FILE);
        fs::write(
            &settings_path,
            serde_json::to_vec_pretty(&initial_settings).unwrap(),
        )
        .unwrap();

        let notes = apply(
            &state_root,
            &target,
            StateSettings::AntigravitySettings,
            &steve_home,
            &node_home,
            Some(&owner_state_root),
        )
        .unwrap();
        assert!(notes.is_empty(), "{notes:?}");

        let settings_bytes = fs::read(&settings_path).unwrap();
        let settings_mtime = fs::metadata(&settings_path).unwrap().modified().unwrap();
        let val: Value = serde_json::from_slice(&settings_bytes).unwrap();
        assert_eq!(val["theme"], "dark");
        assert_eq!(val["model"], "gemini-4-neon");
        assert_eq!(
            val["permissions"]["ask"],
            json!(["mcp(steve/steve_deliver)"])
        );
        assert_eq!(
            val["permissions"]["allow"],
            json!(["command(git status)", "mcp(steve/*)"])
        );
        let deny = val["permissions"]["deny"].as_array().unwrap();
        assert_eq!(deny[0], "command(sudo)");
        assert!(deny.contains(&json!(format!(
            "read_file({}/steve.env)",
            steve_home.display()
        ))));

        let cfg_path = state_root.join(CONFIG_DIR).join(CONFIG_FILE);
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
        let grants_deny = grants["deny"].as_array().unwrap();
        assert_eq!(grants_deny[0], "command(rm -rf /)");
        assert_eq!(grants_deny[1], "command(sudo)");
        assert!(grants_deny.contains(&json!(format!(
            "read_file({}/steve.env)",
            steve_home.display()
        ))));

        // Ambient owner config was never touched.
        assert_eq!(fs::read(&owner_cfg_path).unwrap(), owner_cfg_raw);

        // Second apply must be a complete no-op on both files: no duplicates and no rewrite.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let notes2 = apply(
            &state_root,
            &target,
            StateSettings::AntigravitySettings,
            &steve_home,
            &node_home,
            Some(&owner_state_root),
        )
        .unwrap();
        assert!(notes2.is_empty(), "{notes2:?}");
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
    fn leaves_symlinks_untouched_without_following() {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("state");
        let target = state_root.join("antigravity-cli");
        let cfg_dir = state_root.join(CONFIG_DIR);
        let owner_state_root = dir.path().join("owner/.gemini");
        let owner_config_dir = owner_state_root.join(CONFIG_DIR);
        let steve_home = dir.path().join("steve");
        let node_home = steve_home.join("data/node");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&cfg_dir).unwrap();
        fs::create_dir_all(&owner_config_dir).unwrap();

        let outside = dir.path().join("outside.json");
        fs::write(&outside, r#"{"outside":true}"#).unwrap();

        let settings_link = target.join(SETTINGS_FILE);
        std::os::unix::fs::symlink(&outside, &settings_link).unwrap();
        let cfg_link = cfg_dir.join(CONFIG_FILE);
        std::os::unix::fs::symlink(&outside, &cfg_link).unwrap();

        let notes = apply(
            &state_root,
            &target,
            StateSettings::AntigravitySettings,
            &steve_home,
            &node_home,
            Some(&owner_state_root),
        )
        .unwrap();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            fs::symlink_metadata(&settings_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::symlink_metadata(&cfg_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), r#"{"outside":true}"#);

        // Also refuse symlinked owner config.json and symlinked owner config directory.
        let state_root2 = dir.path().join("state2");
        let target2 = state_root2.join("antigravity-cli");
        fs::create_dir_all(&target2).unwrap();
        std::os::unix::fs::symlink(&outside, owner_config_dir.join(CONFIG_FILE)).unwrap();
        let notes_owner = apply(
            &state_root2,
            &target2,
            StateSettings::AntigravitySettings,
            &steve_home,
            &node_home,
            Some(&owner_state_root),
        )
        .unwrap();
        assert_eq!(notes_owner.len(), 1, "{notes_owner:?}");
        let cfg2: Value = serde_json::from_slice(
            &fs::read(state_root2.join(CONFIG_DIR).join(CONFIG_FILE)).unwrap(),
        )
        .unwrap();
        assert!(cfg2.get("outside").is_none());
        assert_eq!(
            cfg2["userSettings"]["globalPermissionGrants"]["allow"],
            json!(["mcp(steve/*)"])
        );

        let state_root3 = dir.path().join("state3");
        let target3 = state_root3.join("antigravity-cli");
        fs::create_dir_all(&target3).unwrap();
        let owner_root_with_link = dir.path().join("owner-linked");
        fs::create_dir_all(&owner_root_with_link).unwrap();
        std::os::unix::fs::symlink(&owner_config_dir, owner_root_with_link.join(CONFIG_DIR))
            .unwrap();
        let notes_dir = apply(
            &state_root3,
            &target3,
            StateSettings::AntigravitySettings,
            &steve_home,
            &node_home,
            Some(&owner_root_with_link),
        )
        .unwrap();
        assert_eq!(notes_dir.len(), 1, "{notes_dir:?}");
        assert!(state_root3.join(CONFIG_DIR).join(CONFIG_FILE).is_file());
    }

    #[test]
    fn leaves_malformed_or_bad_shape_files_untouched_without_blocking_config_json() {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("state");
        let target = state_root.join("antigravity-cli");
        let steve_home = dir.path().join("steve");
        let node_home = steve_home.join("data/node");
        fs::create_dir_all(&target).unwrap();
        let settings_path = target.join(SETTINGS_FILE);

        // Malformed settings.json is left untouched, while config/config.json is still populated.
        fs::write(&settings_path, "{not json").unwrap();
        let notes = apply(
            &state_root,
            &target,
            StateSettings::AntigravitySettings,
            &steve_home,
            &node_home,
            None,
        )
        .unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(fs::read_to_string(&settings_path).unwrap(), "{not json");
        let cfg_path = state_root.join(CONFIG_DIR).join(CONFIG_FILE);
        let cfg_val: Value = serde_json::from_slice(&fs::read(&cfg_path).unwrap()).unwrap();
        assert_eq!(
            cfg_val["userSettings"]["globalPermissionGrants"]["allow"],
            json!(["mcp(steve/*)"])
        );

        // Non-array permissions.allow in settings.json leaves settings.json untouched.
        let bad_shape = r#"{"permissions":{"allow":"not-an-array"}}"#;
        fs::write(&settings_path, bad_shape).unwrap();
        let notes = apply(
            &state_root,
            &target,
            StateSettings::AntigravitySettings,
            &steve_home,
            &node_home,
            None,
        )
        .unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(fs::read_to_string(&settings_path).unwrap(), bad_shape);

        // Non-object userSettings or malformed config.json leaves config.json untouched.
        fs::write(&settings_path, "{}").unwrap();
        for bad_cfg in [r#"{"userSettings":"bad"}"#, "{not json"] {
            fs::write(&cfg_path, bad_cfg).unwrap();
            let notes = apply(
                &state_root,
                &target,
                StateSettings::AntigravitySettings,
                &steve_home,
                &node_home,
                None,
            )
            .unwrap();
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert_eq!(fs::read_to_string(&cfg_path).unwrap(), bad_cfg);
        }
    }

    #[test]
    fn scans_disk_prefix_matches_for_extra_protected_files_in_both_files() {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("state");
        let target = state_root.join("antigravity-cli");
        let steve_home = dir.path().join("steve");
        let node_home = steve_home.join("data/node");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(steve_home.join("data")).unwrap();
        fs::write(steve_home.join("steve.env.custom"), "SECRET=1").unwrap();
        fs::write(steve_home.join("steve.env*glob"), "SECRET=2").unwrap();
        fs::write(steve_home.join("data/steve.sqlite3-extra"), "db").unwrap();

        apply(
            &state_root,
            &target,
            StateSettings::AntigravitySettings,
            &steve_home,
            &node_home,
            None,
        )
        .unwrap();
        let val: Value =
            serde_json::from_slice(&fs::read(target.join(SETTINGS_FILE)).unwrap()).unwrap();
        let deny = val["permissions"]["deny"].as_array().unwrap();
        assert!(deny.contains(&json!(format!(
            "read_file({}/steve.env.custom)",
            steve_home.display()
        ))));
        assert!(deny.contains(&json!(format!(
            "write_file({}/data/steve.sqlite3-extra)",
            steve_home.display()
        ))));
        // Glob metacharacters must never enter deny rules (agy's sandbox rejects glob deny paths).
        assert!(
            deny.iter()
                .all(|v| !v.as_str().unwrap_or_default().contains('*')),
            "glob path leaked into settings deny: {deny:?}"
        );

        let cfg_val: Value = serde_json::from_slice(
            &fs::read(state_root.join(CONFIG_DIR).join(CONFIG_FILE)).unwrap(),
        )
        .unwrap();
        let cfg_deny = cfg_val["userSettings"]["globalPermissionGrants"]["deny"]
            .as_array()
            .unwrap();
        assert!(cfg_deny.contains(&json!(format!(
            "read_file({}/steve.env.custom)",
            steve_home.display()
        ))));
        assert!(cfg_deny.contains(&json!(format!(
            "write_file({}/data/steve.sqlite3-extra)",
            steve_home.display()
        ))));
        assert!(
            cfg_deny
                .iter()
                .all(|v| !v.as_str().unwrap_or_default().contains('*')),
            "glob path leaked into config deny: {cfg_deny:?}"
        );
    }

    #[test]
    fn none_state_settings_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("state");
        let target = state_root.join("antigravity-cli");
        fs::create_dir_all(&target).unwrap();
        let notes = apply(
            &state_root,
            &target,
            StateSettings::None,
            &dir.path().join("steve"),
            &dir.path().join("node"),
            None,
        )
        .unwrap();
        assert!(notes.is_empty());
        assert!(!target.join(SETTINGS_FILE).exists());
        assert!(!state_root.join(CONFIG_DIR).join(CONFIG_FILE).exists());
    }
}
