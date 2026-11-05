//! Built-in item templates. They
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

/// The template of connection credentials: system vault only, never listed.
pub const CONNECTION_CREDENTIAL: &str = "connection_credential";
/// Every use of a bank card is a decision and its sensitivity can't change.
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

/// A template's name as `GET /vault/templates` shows it: a built-in one still under its seeded name in the
/// request's language. Field labels stay as stored: they are copied into items and decide which fields
/// are secret.
pub fn shown_name(id: &str, source: &str, stored: &str) -> String {
    use steve_i18n::t;
    let seeded = source == "builtin" && BUILTIN.iter().any(|(i, n, ..)| *i == id && *n == stored);
    if !seeded {
        return stored.to_string();
    }
    match id {
        "login" => t!("modules.vault.template.login"),
        "bank_card" => t!("modules.vault.template.bank_card"),
        "api_credential" => t!("modules.vault.template.api_credential"),
        "id_document" => t!("modules.vault.template.id_document"),
        "secure_note" => t!("modules.vault.template.secure_note"),
        "ssh_key" => t!("modules.vault.template.ssh_key"),
        "recovery_codes" => t!("modules.vault.template.recovery_codes"),
        _ => stored.to_string(),
    }
}

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
