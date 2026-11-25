//! Workspace settings schema: single source of truth shared by `steve-core`, `steve-hub`,
//! and generated TypeScript bindings (`web/src/gen/settings-schema.ts`).

use std::fmt;
use std::sync::LazyLock;

use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const RAW_SCHEMA: &str = include_str!("workspace_settings.json");

/// Value labels in declaration order, serialized as a JSON object map.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SettingLabels(pub Vec<(String, String)>);

impl SettingLabels {
    pub fn entries(&self) -> &[(String, String)] {
        &self.0
    }

    pub fn get(&self, value: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == value)
            .map(|(_, v)| v.as_str())
    }
}

impl Serialize for SettingLabels {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for SettingLabels {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntriesVisitor;
        impl<'de> Visitor<'de> for EntriesVisitor {
            type Value = SettingLabels;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of setting values to display labels")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::with_capacity(access.size_hint().unwrap_or(0));
                while let Some((k, v)) = access.next_entry::<String, String>()? {
                    entries.push((k, v));
                }
                Ok(SettingLabels(entries))
            }
        }
        deserializer.deserialize_map(EntriesVisitor)
    }
}

/// One workspace setting's contract: key, allowed values, default, and optional value labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingSpec {
    pub key: String,
    pub label: String,
    pub values: Vec<String>,
    pub def: String,
    pub live: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<SettingLabels>,
}

impl SettingSpec {
    /// Display label for `value`, or `value` itself when no override label exists.
    pub fn label_of<'a>(&'a self, value: &'a str) -> &'a str {
        self.labels
            .as_ref()
            .and_then(|l| l.get(value))
            .unwrap_or(value)
    }

    /// Whether `value` is allowed for this setting.
    pub fn allows(&self, value: &str) -> bool {
        self.values.iter().any(|v| v == value)
    }
}

/// Complete workspace settings schema grouped by category, in wire order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSettingsSchema {
    pub general: Vec<SettingSpec>,
    pub items: Vec<SettingSpec>,
    pub notify: Vec<SettingSpec>,
    pub look: Vec<SettingSpec>,
    pub data: Vec<SettingSpec>,
}

impl WorkspaceSettingsSchema {
    pub fn categories(&self) -> [(&'static str, &[SettingSpec]); 5] {
        [
            ("general", &self.general),
            ("items", &self.items),
            ("notify", &self.notify),
            ("look", &self.look),
            ("data", &self.data),
        ]
    }

    pub fn category(&self, name: &str) -> Option<&[SettingSpec]> {
        match name {
            "general" => Some(&self.general),
            "items" => Some(&self.items),
            "notify" => Some(&self.notify),
            "look" => Some(&self.look),
            "data" => Some(&self.data),
            _ => None,
        }
    }
}

static SCHEMA: LazyLock<WorkspaceSettingsSchema> = LazyLock::new(|| {
    serde_json::from_str(RAW_SCHEMA).expect("valid embedded workspace_settings.json")
});

static SCHEMA_JSON: LazyLock<String> =
    LazyLock::new(|| serde_json::to_string(&*SCHEMA).expect("serialize workspace settings schema"));

static ALL_SPECS: LazyLock<Vec<SettingSpec>> = LazyLock::new(|| {
    SCHEMA
        .categories()
        .into_iter()
        .flat_map(|(_, specs)| specs.iter().cloned())
        .collect()
});

/// Parsed workspace settings schema.
pub fn schema() -> &'static WorkspaceSettingsSchema {
    &SCHEMA
}

/// Compact JSON served by `GET /api/settings/schema`.
pub fn schema_json() -> &'static str {
    &SCHEMA_JSON
}

/// All setting specs flattened in schema order.
pub fn specs() -> &'static [SettingSpec] {
    &ALL_SPECS
}

/// Specs of one category (`general | items | notify | look | data`).
pub fn category(name: &str) -> Option<&'static [SettingSpec]> {
    SCHEMA.category(name)
}

/// Look up one setting spec by key.
pub fn find(key: &str) -> Option<&'static SettingSpec> {
    ALL_SPECS.iter().find(|spec| spec.key == key)
}

/// Whether `value` is allowed for workspace setting `key`.
pub fn allowed(key: &str, value: &str) -> bool {
    find(key).is_some_and(|spec| spec.allows(value))
}

/// `(key, def)` pairs in schema order.
pub fn defaults() -> impl Iterator<Item = (&'static str, &'static str)> {
    ALL_SPECS
        .iter()
        .map(|spec| (spec.key.as_str(), spec.def.as_str()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn schema_is_well_formed_and_round_trips() {
        assert_eq!(specs().len(), 14);
        let mut keys = BTreeSet::new();
        for spec in specs() {
            assert!(keys.insert(&spec.key), "duplicate setting key {}", spec.key);
            assert!(
                spec.allows(&spec.def),
                "default {:?} of {} must be in values {:?}",
                spec.def,
                spec.key,
                spec.values
            );
        }
        assert!(allowed("tz", "UTC"));
        assert!(!allowed("tz", "Mars/Base"));
        assert!(allowed("handoff", "70"));
        assert!(!allowed("handoff", "75"));
        assert!(!allowed("nope", "x"));

        let parsed: WorkspaceSettingsSchema =
            serde_json::from_str(schema_json()).expect("reparse schema_json");
        assert_eq!(parsed, *schema());
        assert_eq!(serde_json::to_string(&parsed).unwrap(), schema_json());
    }
}
