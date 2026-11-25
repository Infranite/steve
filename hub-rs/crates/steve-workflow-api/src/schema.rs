//! Closed JSON Schema 2020-12 subset validator with static ReDoS regex interception.
//!
//! Supported keywords (anything else, including `$ref` / `$defs` / `$dynamicRef`, is rejected):
//! - `type` (`null | boolean | integer | number | string | array | object`, or non-empty list)
//! - `properties`, `required`, `additionalProperties` (`bool` or sub-schema)
//! - `items`, `minItems`, `maxItems`
//! - `enum`, `const`
//! - `minimum`, `maximum`
//! - `minLength`, `maxLength`, `pattern` (length-capped + static ReDoS rejected + linear-time NFA)
//! - `oneOf`, `anyOf`
//! - harmless metadata annotations: `title`, `description`

use std::collections::{BTreeMap, BTreeSet};

use regex::{Regex, RegexBuilder};
use regex_syntax::ast::{self, Ast};
use regex_syntax::hir::translate::Translator;
use regex_syntax::hir::{Class, ClassBytes, ClassUnicode, Hir, HirKind, Repetition};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::canonical::{CanonicalError, validate_f64, validate_json_value};
use crate::limits::{
    MAX_RESULT_BYTES, MAX_SAFE_INTEGER, MAX_SCHEMA_DEPTH, MAX_SCHEMA_NODES, MAX_SCHEMA_REGEX_BYTES,
    MAX_SCHEMA_REGEX_COMPILED_BYTES, MAX_SCHEMA_REGEX_DFA_BYTES, MAX_SCHEMA_REGEX_REPETITION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrimitiveType {
    Null,
    Boolean,
    Integer,
    Number,
    String,
    Array,
    Object,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SchemaType {
    Single(PrimitiveType),
    Multiple(Vec<PrimitiveType>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AdditionalProperties {
    Bool(bool),
    Schema(Box<JsonSchemaSubset>),
}

/// Closed JSON Schema 2020-12 subset. Unknown keywords (including `$ref`) are rejected at
/// deserialization via `#[serde(deny_unknown_fields)]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JsonSchemaSubset {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<SchemaType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<BTreeMap<String, JsonSchemaSubset>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_properties: Option<AdditionalProperties>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<JsonSchemaSubset>>,
    #[serde(rename = "enum", default, skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<Value>>,
    #[serde(rename = "const", default, skip_serializing_if = "Option::is_none")]
    pub const_value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_of: Option<Vec<JsonSchemaSubset>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<JsonSchemaSubset>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SchemaError {
    #[error("schema nesting depth exceeds maximum of {limit} at {path}")]
    MaxDepthExceeded { path: String, limit: usize },
    #[error("schema node count exceeds maximum of {limit}")]
    MaxNodesExceeded { limit: usize },
    #[error("invalid schema at {path}: {detail}")]
    InvalidDefinition { path: String, detail: String },
    #[error("invalid numeric value in schema at {path}: {source}")]
    InvalidNumber {
        path: String,
        #[source]
        source: CanonicalError,
    },
    #[error("schema pattern at {path} is {actual} bytes, exceeding limit of {limit} bytes")]
    RegexTooLong {
        path: String,
        actual: usize,
        limit: usize,
    },
    #[error("schema pattern at {path} is invalid regex: {detail}")]
    RegexInvalid { path: String, detail: String },
    #[error("schema pattern at {path} rejected as unsafe ReDoS pattern: {reason}")]
    RegexUnsafe { path: String, reason: String },
}

impl SchemaError {
    pub const fn is_regex_safety(&self) -> bool {
        matches!(self, Self::RegexTooLong { .. } | Self::RegexUnsafe { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationViolationCode {
    TypeMismatch,
    ConstMismatch,
    EnumMismatch,
    Minimum,
    Maximum,
    MinLength,
    MaxLength,
    PatternMismatch,
    MinItems,
    MaxItems,
    MissingRequired,
    AdditionalPropertyForbidden,
    AnyOfFailed,
    OneOfFailed,
    ReviewApprovedRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationViolation {
    pub path: String,
    pub code: ValidationViolationCode,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResultValidationError {
    #[error("result size {actual} bytes exceeds maximum of {limit} bytes")]
    ResultTooLarge { actual: usize, limit: usize },
    #[error("result contains invalid number: {0}")]
    InvalidValue(#[from] CanonicalError),
    #[error("schema error: {0}")]
    Schema(#[from] SchemaError),
    #[error("result failed schema validation with {} violation(s)", .0.len())]
    Violations(Vec<ValidationViolation>),
}

/// Pre-validated and regex-compiled representation of [`JsonSchemaSubset`].
#[derive(Debug, Clone)]
pub struct CompiledSchema {
    schema_type: Option<Vec<PrimitiveType>>,
    properties: Option<BTreeMap<String, CompiledSchema>>,
    required: Option<Vec<String>>,
    additional_properties: Option<CompiledAdditionalProperties>,
    items: Option<Box<CompiledSchema>>,
    enum_values: Option<Vec<Value>>,
    const_value: Option<Value>,
    minimum: Option<f64>,
    maximum: Option<f64>,
    min_length: Option<u64>,
    max_length: Option<u64>,
    pattern: Option<Regex>,
    min_items: Option<u64>,
    max_items: Option<u64>,
    one_of: Option<Vec<CompiledSchema>>,
    any_of: Option<Vec<CompiledSchema>>,
}

#[derive(Debug, Clone)]
enum CompiledAdditionalProperties {
    Bool(bool),
    Schema(Box<CompiledSchema>),
}

impl JsonSchemaSubset {
    /// Parses a raw `serde_json::Value` into the closed subset, rejecting unknown keywords such as
    /// `$ref` or `$defs`.
    pub fn from_json_value(value: Value) -> Result<Self, SchemaError> {
        serde_json::from_value(value).map_err(|e| SchemaError::InvalidDefinition {
            path: "$".into(),
            detail: e.to_string(),
        })
    }

    /// Validates schema structure, safe numbers, depth/node budgets and compiles any `pattern`
    /// regular expressions with ReDoS rejection.
    pub fn compile(&self) -> Result<CompiledSchema, SchemaError> {
        let mut nodes = 0usize;
        compile_node(self, "$", 0, &mut nodes)
    }
}

/// Validates a structured call/workflow result against size limits, safe-number rules, an optional
/// [`JsonSchemaSubset`], and the mandatory `approved: boolean` rule for `review: true` calls.
pub fn validate_structured_result(
    value: &Value,
    schema: Option<&JsonSchemaSubset>,
    is_review: bool,
    max_result_bytes: usize,
) -> Result<(), ResultValidationError> {
    let limit = max_result_bytes.min(MAX_RESULT_BYTES);
    validate_json_value(value)?;
    let serialized = serde_json::to_vec(value).expect("JSON value serialization");
    if serialized.len() > limit {
        return Err(ResultValidationError::ResultTooLarge {
            actual: serialized.len(),
            limit,
        });
    }
    let mut violations = Vec::new();
    if let Some(schema) = schema {
        let compiled = schema.compile()?;
        compiled.validate_into(value, "$", &mut violations);
    }
    if is_review {
        let ok = value
            .as_object()
            .and_then(|o| o.get("approved"))
            .is_some_and(Value::is_boolean);
        if !ok {
            violations.push(ValidationViolation {
                path: "$.approved".into(),
                code: ValidationViolationCode::ReviewApprovedRequired,
                detail: "review result must be an object containing boolean field 'approved'"
                    .into(),
            });
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(ResultValidationError::Violations(violations))
    }
}

impl CompiledSchema {
    /// Validates a JSON value against this compiled schema.
    pub fn validate(&self, value: &Value) -> Result<(), ResultValidationError> {
        validate_json_value(value)?;
        let mut violations = Vec::new();
        self.validate_into(value, "$", &mut violations);
        if violations.is_empty() {
            Ok(())
        } else {
            Err(ResultValidationError::Violations(violations))
        }
    }

    fn matches(&self, value: &Value) -> bool {
        let mut violations = Vec::new();
        self.validate_into(value, "$", &mut violations);
        violations.is_empty()
    }

    fn validate_into(&self, value: &Value, path: &str, out: &mut Vec<ValidationViolation>) {
        if let Some(allowed) = &self.schema_type {
            if !allowed.iter().any(|t| value_matches_type(value, *t)) {
                out.push(ValidationViolation {
                    path: path.into(),
                    code: ValidationViolationCode::TypeMismatch,
                    detail: format!("value does not match expected type(s) {allowed:?}"),
                });
            }
        }

        if let Some(expected) = &self.const_value {
            if !json_schema_eq(value, expected) {
                out.push(ValidationViolation {
                    path: path.into(),
                    code: ValidationViolationCode::ConstMismatch,
                    detail: "value does not equal const".into(),
                });
            }
        }

        if let Some(allowed) = &self.enum_values {
            if !allowed.iter().any(|e| json_schema_eq(value, e)) {
                out.push(ValidationViolation {
                    path: path.into(),
                    code: ValidationViolationCode::EnumMismatch,
                    detail: "value is not in enum".into(),
                });
            }
        }

        if let Some(n) = value.as_f64() {
            if let Some(min) = self.minimum {
                if n < min {
                    out.push(ValidationViolation {
                        path: path.into(),
                        code: ValidationViolationCode::Minimum,
                        detail: format!("{n} < minimum {min}"),
                    });
                }
            }
            if let Some(max) = self.maximum {
                if n > max {
                    out.push(ValidationViolation {
                        path: path.into(),
                        code: ValidationViolationCode::Maximum,
                        detail: format!("{n} > maximum {max}"),
                    });
                }
            }
        }

        if let Some(s) = value.as_str() {
            let char_len = s.chars().count() as u64;
            if let Some(min) = self.min_length {
                if char_len < min {
                    out.push(ValidationViolation {
                        path: path.into(),
                        code: ValidationViolationCode::MinLength,
                        detail: format!("length {char_len} < minLength {min}"),
                    });
                }
            }
            if let Some(max) = self.max_length {
                if char_len > max {
                    out.push(ValidationViolation {
                        path: path.into(),
                        code: ValidationViolationCode::MaxLength,
                        detail: format!("length {char_len} > maxLength {max}"),
                    });
                }
            }
            if let Some(re) = &self.pattern {
                if !re.is_match(s) {
                    out.push(ValidationViolation {
                        path: path.into(),
                        code: ValidationViolationCode::PatternMismatch,
                        detail: "string does not match pattern".into(),
                    });
                }
            }
        }

        if let Some(arr) = value.as_array() {
            let len = arr.len() as u64;
            if let Some(min) = self.min_items {
                if len < min {
                    out.push(ValidationViolation {
                        path: path.into(),
                        code: ValidationViolationCode::MinItems,
                        detail: format!("item count {len} < minItems {min}"),
                    });
                }
            }
            if let Some(max) = self.max_items {
                if len > max {
                    out.push(ValidationViolation {
                        path: path.into(),
                        code: ValidationViolationCode::MaxItems,
                        detail: format!("item count {len} > maxItems {max}"),
                    });
                }
            }
            if let Some(item_schema) = &self.items {
                for (idx, item) in arr.iter().enumerate() {
                    let child_path = format!("{path}[{idx}]");
                    item_schema.validate_into(item, &child_path, out);
                }
            }
        }

        if let Some(obj) = value.as_object() {
            if let Some(req) = &self.required {
                for key in req {
                    if !obj.contains_key(key) {
                        out.push(ValidationViolation {
                            path: format!("{path}.{key}"),
                            code: ValidationViolationCode::MissingRequired,
                            detail: format!("missing required property '{key}'"),
                        });
                    }
                }
            }
            for (k, v) in obj {
                let child_path = format!("{path}.{k}");
                if let Some(prop_schema) = self.properties.as_ref().and_then(|p| p.get(k)) {
                    prop_schema.validate_into(v, &child_path, out);
                } else if let Some(ap) = &self.additional_properties {
                    match ap {
                        CompiledAdditionalProperties::Bool(true) => {}
                        CompiledAdditionalProperties::Bool(false) => {
                            out.push(ValidationViolation {
                                path: child_path,
                                code: ValidationViolationCode::AdditionalPropertyForbidden,
                                detail: format!("additional property '{k}' is not allowed"),
                            });
                        }
                        CompiledAdditionalProperties::Schema(sub) => {
                            sub.validate_into(v, &child_path, out);
                        }
                    }
                }
            }
        }

        if let Some(branches) = &self.any_of {
            if !branches.iter().any(|b| b.matches(value)) {
                out.push(ValidationViolation {
                    path: path.into(),
                    code: ValidationViolationCode::AnyOfFailed,
                    detail: "value matched none of anyOf branches".into(),
                });
            }
        }

        if let Some(branches) = &self.one_of {
            let count = branches.iter().filter(|b| b.matches(value)).count();
            if count != 1 {
                out.push(ValidationViolation {
                    path: path.into(),
                    code: ValidationViolationCode::OneOfFailed,
                    detail: format!("value matched {count} oneOf branches (expected exactly 1)"),
                });
            }
        }
    }
}

fn compile_node(
    s: &JsonSchemaSubset,
    path: &str,
    depth: usize,
    nodes: &mut usize,
) -> Result<CompiledSchema, SchemaError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(SchemaError::MaxDepthExceeded {
            path: path.into(),
            limit: MAX_SCHEMA_DEPTH,
        });
    }
    *nodes += 1;
    if *nodes > MAX_SCHEMA_NODES {
        return Err(SchemaError::MaxNodesExceeded {
            limit: MAX_SCHEMA_NODES,
        });
    }

    let schema_type = match &s.schema_type {
        None => None,
        Some(SchemaType::Single(t)) => Some(vec![*t]),
        Some(SchemaType::Multiple(list)) => {
            if list.is_empty() {
                return Err(SchemaError::InvalidDefinition {
                    path: format!("{path}.type"),
                    detail: "type array must not be empty".into(),
                });
            }
            let mut seen = BTreeSet::new();
            for t in list {
                if !seen.insert(*t) {
                    return Err(SchemaError::InvalidDefinition {
                        path: format!("{path}.type"),
                        detail: format!("duplicate type {t:?}"),
                    });
                }
            }
            Some(list.clone())
        }
    };

    if let Some(min) = s.minimum {
        validate_f64(min).map_err(|source| SchemaError::InvalidNumber {
            path: format!("{path}.minimum"),
            source,
        })?;
    }
    if let Some(max) = s.maximum {
        validate_f64(max).map_err(|source| SchemaError::InvalidNumber {
            path: format!("{path}.maximum"),
            source,
        })?;
    }
    if let (Some(min), Some(max)) = (s.minimum, s.maximum) {
        if min > max {
            return Err(SchemaError::InvalidDefinition {
                path: path.into(),
                detail: format!("minimum ({min}) > maximum ({max})"),
            });
        }
    }

    validate_safe_u64(s.min_length, path, "minLength")?;
    validate_safe_u64(s.max_length, path, "maxLength")?;
    if let (Some(min), Some(max)) = (s.min_length, s.max_length) {
        if min > max {
            return Err(SchemaError::InvalidDefinition {
                path: path.into(),
                detail: format!("minLength ({min}) > maxLength ({max})"),
            });
        }
    }

    validate_safe_u64(s.min_items, path, "minItems")?;
    validate_safe_u64(s.max_items, path, "maxItems")?;
    if let (Some(min), Some(max)) = (s.min_items, s.max_items) {
        if min > max {
            return Err(SchemaError::InvalidDefinition {
                path: path.into(),
                detail: format!("minItems ({min}) > maxItems ({max})"),
            });
        }
    }

    if let Some(c) = &s.const_value {
        validate_json_value(c).map_err(|source| SchemaError::InvalidNumber {
            path: format!("{path}.const"),
            source,
        })?;
    }

    if let Some(ev) = &s.enum_values {
        if ev.is_empty() {
            return Err(SchemaError::InvalidDefinition {
                path: format!("{path}.enum"),
                detail: "enum must not be empty".into(),
            });
        }
        for (i, v) in ev.iter().enumerate() {
            validate_json_value(v).map_err(|source| SchemaError::InvalidNumber {
                path: format!("{path}.enum[{i}]"),
                source,
            })?;
        }
    }

    let pattern = match &s.pattern {
        None => None,
        Some(pat) => Some(compile_safe_regex(pat, &format!("{path}.pattern"))?),
    };

    let properties = match &s.properties {
        None => None,
        Some(map) => {
            let mut compiled = BTreeMap::new();
            for (k, sub) in map {
                compiled.insert(
                    k.clone(),
                    compile_node(sub, &format!("{path}.properties.{k}"), depth + 1, nodes)?,
                );
            }
            Some(compiled)
        }
    };

    let additional_properties = match &s.additional_properties {
        None => None,
        Some(AdditionalProperties::Bool(b)) => Some(CompiledAdditionalProperties::Bool(*b)),
        Some(AdditionalProperties::Schema(sub)) => Some(CompiledAdditionalProperties::Schema(
            Box::new(compile_node(
                sub,
                &format!("{path}.additionalProperties"),
                depth + 1,
                nodes,
            )?),
        )),
    };

    let items = match &s.items {
        None => None,
        Some(sub) => Some(Box::new(compile_node(
            sub,
            &format!("{path}.items"),
            depth + 1,
            nodes,
        )?)),
    };

    let one_of = compile_branch_list(&s.one_of, path, "oneOf", depth, nodes)?;
    let any_of = compile_branch_list(&s.any_of, path, "anyOf", depth, nodes)?;

    Ok(CompiledSchema {
        schema_type,
        properties,
        required: s.required.clone(),
        additional_properties,
        items,
        enum_values: s.enum_values.clone(),
        const_value: s.const_value.clone(),
        minimum: s.minimum,
        maximum: s.maximum,
        min_length: s.min_length,
        max_length: s.max_length,
        pattern,
        min_items: s.min_items,
        max_items: s.max_items,
        one_of,
        any_of,
    })
}

fn compile_branch_list(
    list: &Option<Vec<JsonSchemaSubset>>,
    path: &str,
    field: &str,
    depth: usize,
    nodes: &mut usize,
) -> Result<Option<Vec<CompiledSchema>>, SchemaError> {
    let Some(items) = list else {
        return Ok(None);
    };
    if items.is_empty() {
        return Err(SchemaError::InvalidDefinition {
            path: format!("{path}.{field}"),
            detail: format!("{field} must not be empty"),
        });
    }
    let mut out = Vec::with_capacity(items.len());
    for (i, sub) in items.iter().enumerate() {
        out.push(compile_node(
            sub,
            &format!("{path}.{field}[{i}]"),
            depth + 1,
            nodes,
        )?);
    }
    Ok(Some(out))
}

fn validate_safe_u64(val: Option<u64>, path: &str, field: &str) -> Result<(), SchemaError> {
    if let Some(v) = val {
        if v > MAX_SAFE_INTEGER as u64 {
            return Err(SchemaError::InvalidNumber {
                path: format!("{path}.{field}"),
                source: CanonicalError::UnsafeInteger {
                    value: v.to_string(),
                },
            });
        }
    }
    Ok(())
}

/// Compiles a schema `pattern` regex after enforcing length cap, static ReDoS HIR inspection,
/// and NFA/DFA size ceilings.
pub fn compile_safe_regex(pattern: &str, path: &str) -> Result<Regex, SchemaError> {
    if pattern.len() > MAX_SCHEMA_REGEX_BYTES {
        return Err(SchemaError::RegexTooLong {
            path: path.into(),
            actual: pattern.len(),
            limit: MAX_SCHEMA_REGEX_BYTES,
        });
    }
    let ast = ast::parse::Parser::new()
        .parse(pattern)
        .map_err(|e| SchemaError::RegexInvalid {
            path: path.into(),
            detail: e.to_string(),
        })?;
    check_ast_redos(&ast, false, path)?;

    let hir =
        Translator::new()
            .translate(pattern, &ast)
            .map_err(|e| SchemaError::RegexInvalid {
                path: path.into(),
                detail: e.to_string(),
            })?;
    check_hir_redos(&hir, false, path)?;

    RegexBuilder::new(pattern)
        .size_limit(MAX_SCHEMA_REGEX_COMPILED_BYTES)
        .dfa_size_limit(MAX_SCHEMA_REGEX_DFA_BYTES)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(_) => SchemaError::RegexUnsafe {
                path: path.into(),
                reason: format!("compiled automaton exceeds {MAX_SCHEMA_REGEX_COMPILED_BYTES}B"),
            },
            other => SchemaError::RegexInvalid {
                path: path.into(),
                detail: other.to_string(),
            },
        })
}

/// Inspects the raw regex AST before HIR simplification collapses duplicate alternations such as
/// `(a|a)+` or nested quantifiers such as `(a+)+`.
fn check_ast_redos(ast: &Ast, inside_multi_rep: bool, path: &str) -> Result<(), SchemaError> {
    match ast {
        Ast::Empty(_)
        | Ast::Flags(_)
        | Ast::Literal(_)
        | Ast::Dot(_)
        | Ast::Assertion(_)
        | Ast::ClassUnicode(_)
        | Ast::ClassPerl(_)
        | Ast::ClassBracketed(_) => Ok(()),
        Ast::Group(g) => check_ast_redos(&g.ast, inside_multi_rep, path),
        Ast::Repetition(rep) => {
            let is_multi = match &rep.op.kind {
                ast::RepetitionKind::ZeroOrOne => false,
                ast::RepetitionKind::ZeroOrMore | ast::RepetitionKind::OneOrMore => true,
                ast::RepetitionKind::Range(r) => match r {
                    ast::RepetitionRange::Exactly(n) => {
                        if *n > MAX_SCHEMA_REGEX_REPETITION {
                            return Err(SchemaError::RegexUnsafe {
                                path: path.into(),
                                reason: format!(
                                    "repetition bound exceeds maximum of {MAX_SCHEMA_REGEX_REPETITION}"
                                ),
                            });
                        }
                        *n > 1
                    }
                    ast::RepetitionRange::AtLeast(n) => {
                        if *n > MAX_SCHEMA_REGEX_REPETITION {
                            return Err(SchemaError::RegexUnsafe {
                                path: path.into(),
                                reason: format!(
                                    "repetition bound exceeds maximum of {MAX_SCHEMA_REGEX_REPETITION}"
                                ),
                            });
                        }
                        true
                    }
                    ast::RepetitionRange::Bounded(n, m) => {
                        if *n > MAX_SCHEMA_REGEX_REPETITION || *m > MAX_SCHEMA_REGEX_REPETITION {
                            return Err(SchemaError::RegexUnsafe {
                                path: path.into(),
                                reason: format!(
                                    "repetition bound exceeds maximum of {MAX_SCHEMA_REGEX_REPETITION}"
                                ),
                            });
                        }
                        *m > 1
                    }
                },
            };
            if inside_multi_rep && is_multi {
                return Err(SchemaError::RegexUnsafe {
                    path: path.into(),
                    reason: "nested quantifier detected".into(),
                });
            }
            check_ast_redos(&rep.ast, inside_multi_rep || is_multi, path)
        }
        Ast::Concat(c) => {
            for sub in &c.asts {
                check_ast_redos(sub, inside_multi_rep, path)?;
            }
            Ok(())
        }
        Ast::Alternation(alt) => {
            for sub in &alt.asts {
                check_ast_redos(sub, inside_multi_rep, path)?;
            }
            if inside_multi_rep && alt.asts.len() > 1 {
                let mut branch_hirs = Vec::with_capacity(alt.asts.len());
                for branch in &alt.asts {
                    if let Ok(h) = Translator::new().translate("", branch) {
                        branch_hirs.push(h);
                    }
                }
                if alternation_branches_overlap(&branch_hirs) {
                    return Err(SchemaError::RegexUnsafe {
                        path: path.into(),
                        reason: "overlapping alternation branches inside quantifier".into(),
                    });
                }
            }
            Ok(())
        }
    }
}

fn check_hir_redos(hir: &Hir, inside_multi_rep: bool, path: &str) -> Result<(), SchemaError> {
    match hir.kind() {
        HirKind::Empty | HirKind::Literal(_) | HirKind::Class(_) | HirKind::Look(_) => Ok(()),
        HirKind::Capture(cap) => check_hir_redos(&cap.sub, inside_multi_rep, path),
        HirKind::Repetition(rep) => {
            if rep.min > MAX_SCHEMA_REGEX_REPETITION
                || rep.max.is_some_and(|m| m > MAX_SCHEMA_REGEX_REPETITION)
            {
                return Err(SchemaError::RegexUnsafe {
                    path: path.into(),
                    reason: format!(
                        "repetition bound exceeds maximum of {MAX_SCHEMA_REGEX_REPETITION}"
                    ),
                });
            }
            let is_multi = rep.max.is_none_or(|m| m > 1);
            if inside_multi_rep && is_multi {
                return Err(SchemaError::RegexUnsafe {
                    path: path.into(),
                    reason: "nested quantifier detected".into(),
                });
            }
            if is_multi && matches_empty(&rep.sub) {
                return Err(SchemaError::RegexUnsafe {
                    path: path.into(),
                    reason: "quantifier over nullable sub-expression".into(),
                });
            }
            if is_multi {
                check_repeated_alternation(&rep.sub, path)?;
            }
            check_hir_redos(&rep.sub, inside_multi_rep || is_multi, path)
        }
        HirKind::Concat(subs) => {
            for sub in subs {
                check_hir_redos(sub, inside_multi_rep, path)?;
            }
            // Reject adjacent ambiguous unbounded quantifiers such as `.*.*` or `[a-z]+[a-z]+`.
            for pair in subs.windows(2) {
                if let (HirKind::Repetition(a), HirKind::Repetition(b)) =
                    (pair[0].kind(), pair[1].kind())
                {
                    if (a.max.is_none() || b.max.is_none()) && hir_first_overlap(&a.sub, &b.sub) {
                        return Err(SchemaError::RegexUnsafe {
                            path: path.into(),
                            reason: "adjacent overlapping unbounded quantifiers".into(),
                        });
                    }
                }
            }
            Ok(())
        }
        HirKind::Alternation(branches) => {
            for b in branches {
                check_hir_redos(b, inside_multi_rep, path)?;
            }
            if inside_multi_rep && alternation_branches_overlap(branches) {
                return Err(SchemaError::RegexUnsafe {
                    path: path.into(),
                    reason: "overlapping alternation branches inside quantifier".into(),
                });
            }
            Ok(())
        }
    }
}

fn check_repeated_alternation(sub: &Hir, path: &str) -> Result<(), SchemaError> {
    match sub.kind() {
        HirKind::Capture(c) => check_repeated_alternation(&c.sub, path),
        HirKind::Alternation(branches) => {
            if alternation_branches_overlap(branches) {
                return Err(SchemaError::RegexUnsafe {
                    path: path.into(),
                    reason: "overlapping alternation branches inside quantifier".into(),
                });
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn matches_empty(hir: &Hir) -> bool {
    hir.properties().minimum_len() == Some(0)
}

fn alternation_branches_overlap(branches: &[Hir]) -> bool {
    for i in 0..branches.len() {
        for j in (i + 1)..branches.len() {
            if hir_first_overlap(&branches[i], &branches[j]) {
                return true;
            }
        }
    }
    false
}

/// Conservative first-character-class summary for static ambiguity checking.
enum FirstClass {
    Empty,
    Any,
    Unicode(ClassUnicode),
    Bytes(ClassBytes),
}

fn first_class(hir: &Hir) -> FirstClass {
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => FirstClass::Empty,
        HirKind::Literal(lit) => {
            if let Ok(s) = std::str::from_utf8(&lit.0) {
                if let Some(ch) = s.chars().next() {
                    return FirstClass::Unicode(ClassUnicode::new([
                        regex_syntax::hir::ClassUnicodeRange::new(ch, ch),
                    ]));
                }
            }
            if let Some(&b) = lit.0.first() {
                return FirstClass::Bytes(ClassBytes::new([
                    regex_syntax::hir::ClassBytesRange::new(b, b),
                ]));
            }
            FirstClass::Empty
        }
        HirKind::Class(Class::Unicode(u)) => FirstClass::Unicode(u.clone()),
        HirKind::Class(Class::Bytes(b)) => FirstClass::Bytes(b.clone()),
        HirKind::Capture(c) => first_class(&c.sub),
        HirKind::Repetition(Repetition { sub, .. }) => first_class(sub),
        HirKind::Concat(subs) => {
            let mut acc = FirstClass::Empty;
            for sub in subs {
                acc = union_first(acc, first_class(sub));
                if !matches_empty(sub) {
                    break;
                }
            }
            acc
        }
        HirKind::Alternation(branches) => {
            let mut acc = FirstClass::Empty;
            for b in branches {
                acc = union_first(acc, first_class(b));
            }
            acc
        }
    }
}

fn union_first(a: FirstClass, b: FirstClass) -> FirstClass {
    match (a, b) {
        (FirstClass::Empty, x) | (x, FirstClass::Empty) => x,
        (FirstClass::Any, _) | (_, FirstClass::Any) => FirstClass::Any,
        (FirstClass::Unicode(mut u1), FirstClass::Unicode(u2)) => {
            u1.union(&u2);
            FirstClass::Unicode(u1)
        }
        (FirstClass::Bytes(mut b1), FirstClass::Bytes(b2)) => {
            b1.union(&b2);
            FirstClass::Bytes(b1)
        }
        _ => FirstClass::Any,
    }
}

fn hir_first_overlap(a: &Hir, b: &Hir) -> bool {
    match (first_class(a), first_class(b)) {
        (FirstClass::Empty, _) | (_, FirstClass::Empty) => true,
        (FirstClass::Any, _) | (_, FirstClass::Any) => true,
        (FirstClass::Unicode(mut u1), FirstClass::Unicode(u2)) => {
            u1.intersect(&u2);
            !u1.ranges().is_empty()
        }
        (FirstClass::Bytes(mut b1), FirstClass::Bytes(b2)) => {
            b1.intersect(&b2);
            !b1.ranges().is_empty()
        }
        _ => true,
    }
}

fn value_matches_type(value: &Value, expected: PrimitiveType) -> bool {
    match expected {
        PrimitiveType::Null => value.is_null(),
        PrimitiveType::Boolean => value.is_boolean(),
        PrimitiveType::String => value.is_string(),
        PrimitiveType::Array => value.is_array(),
        PrimitiveType::Object => value.is_object(),
        PrimitiveType::Number => value.is_number(),
        PrimitiveType::Integer => match value.as_number() {
            Some(n) => {
                n.is_i64()
                    || n.is_u64()
                    || n.as_f64()
                        .is_some_and(|f| f.is_finite() && f.fract() == 0.0)
            }
            None => false,
        },
    }
}

/// JSON Schema 2020-12 structural equality (numbers compare mathematically: `1 == 1.0`).
fn json_schema_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(fx), Some(fy)) => fx == fy,
            _ => false,
        },
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(l, r)| json_schema_eq(l, r))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|yv| json_schema_eq(v, yv)))
        }
        _ => false,
    }
}
