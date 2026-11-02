//! Built-in item templates (accounts-vault.md §1 seeds, fields as the prototype's `TPL_FIELDS`). They
//! are seeded once into `vault_templates` under workspace `*`; a template's `secret` flags are the
//! authority when an item is created (a client can't store a template's secret field in the clear).

use serde::Serialize;

/// A field of a template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct FieldDef {
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub secret: bool,
}

/// One row of `vault_templates`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub fields: Vec<FieldDef>,
    pub default_sensitivity: String,
    pub source: String,
}

impl Template {
    /// The template's own field with this label, if any.
    pub fn field(&self, label: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.label == label)
    }
}

/// The template of connection credentials: system vault only, never listed (§1, §2.2).
pub const CONNECTION_CREDENTIAL: &str = "connection_credential";
/// Every use of a bank card is a decision and its sensitivity can't change (§4.3).
pub const BANK_CARD: &str = "bank_card";

pub const FIELD_TYPES: &[&str] = &[
    "text",
    "password",
    "otp",
    "card_number",
    "date",
    "email",
    "url",
    "file",
];
pub const SENSITIVITIES: &[&str] = &["normal", "sensitive", "high"];

type Seed = (
    &'static str,
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str, bool)],
);

/// `(id, name, default sensitivity, [(label, type, secret)])`.
const BUILTIN: &[Seed] = &[
    (
        "login",
        "登录",
        "sensitive",
        &[
            ("用户名", "text", false),
            ("密码", "password", true),
            ("网站", "url", false),
        ],
    ),
    (
        "bank_card",
        "银行卡",
        "high",
        &[
            ("持卡人", "text", false),
            ("卡号", "card_number", true),
            ("有效期", "date", false),
            ("CVV", "password", true),
        ],
    ),
    (
        "api_credential",
        "API 凭据",
        "sensitive",
        &[("名称", "text", false), ("令牌", "password", true)],
    ),
    (
        "id_document",
        "证件",
        "high",
        &[
            ("姓名", "text", false),
            ("证件号", "password", true),
            ("有效期", "date", false),
        ],
    ),
    (
        "secure_note",
        "安全笔记",
        "sensitive",
        &[("内容", "password", true)],
    ),
    (
        "ssh_key",
        "SSH 密钥",
        "sensitive",
        &[("公钥", "text", false), ("私钥", "password", true)],
    ),
    (
        "recovery_codes",
        "恢复码",
        "high",
        &[("恢复码", "password", true)],
    ),
    // fields come from the adapter (A2); every one is stored secret
    (CONNECTION_CREDENTIAL, "连接凭据", "sensitive", &[]),
];

/// The built-in templates, in display order.
pub fn builtin() -> Vec<Template> {
    BUILTIN
        .iter()
        .map(|(id, name, sens, fields)| Template {
            id: (*id).into(),
            name: (*name).into(),
            fields: fields
                .iter()
                .map(|(l, t, s)| FieldDef {
                    label: (*l).into(),
                    kind: (*t).into(),
                    secret: *s,
                })
                .collect(),
            default_sensitivity: (*sens).into(),
            source: "builtin".into(),
        })
        .collect()
}
