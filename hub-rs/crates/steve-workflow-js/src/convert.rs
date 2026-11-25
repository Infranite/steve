//! Strict bidirectional conversion between QuickJS values and canonical JSON.
//!
//! Unlike `JSON.stringify` (which silently turns `NaN`/`Infinity` into `null` and loses integer
//! precision above `2^53 - 1`), [`js_to_json`] walks QuickJS values directly and rejects
//! non-finite numbers, unsafe integers, `BigInt`, functions, symbols, and non-plain objects.

use std::collections::BTreeMap;

use rquickjs::{Ctx, Object, Type, Value as JsValue};
use serde_json::{Map, Number, Value};
use steve_workflow_api::{
    CanonicalError, MAX_JSON_DEPTH, WorkflowFault, WorkflowFaultCode, to_canonical_json,
    validate_f64, validate_json_value,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertError {
    NonFiniteNumber,
    UnsafeInteger(String),
    MaxDepthExceeded,
    UnsupportedValue(String),
    JsException(String),
}

impl ConvertError {
    pub fn into_fault(self, context: &str) -> WorkflowFault {
        match self {
            Self::NonFiniteNumber => WorkflowFault::new(
                WorkflowFaultCode::NonFiniteNumber,
                format!("{context}: NaN and Infinity are not permitted"),
            ),
            Self::UnsafeInteger(v) => WorkflowFault::new(
                WorkflowFaultCode::UnsafeInteger,
                format!("{context}: integer {v} is outside JS safe integer range"),
            ),
            Self::MaxDepthExceeded => WorkflowFault::new(
                WorkflowFaultCode::UnsupportedValue,
                format!("{context}: nesting depth exceeds {MAX_JSON_DEPTH}"),
            ),
            Self::UnsupportedValue(detail) => WorkflowFault::new(
                WorkflowFaultCode::UnsupportedValue,
                format!("{context}: {detail}"),
            ),
            Self::JsException(detail) => WorkflowFault::new(
                WorkflowFaultCode::RuntimeError,
                format!("{context}: {detail}"),
            ),
        }
    }
}

/// Converts a QuickJS value into a validated, UTF-8 key-sorted `serde_json::Value`.
///
/// When `skip_undefined_props` is `true`, own object properties with value `undefined` are
/// omitted (standard JS optional property semantics); `undefined` as a root value or array
/// element is always rejected unless handled by the caller first.
pub fn js_to_json<'js>(
    ctx: &Ctx<'js>,
    val: &JsValue<'js>,
    skip_undefined_props: bool,
) -> Result<Value, ConvertError> {
    let obj_proto: Object<'js> = ctx
        .globals()
        .get::<_, Object<'js>>("Object")
        .and_then(|o| o.get("prototype"))
        .map_err(|e| ConvertError::JsException(e.to_string()))?;
    convert_at(val, &obj_proto, 0, skip_undefined_props)
}

fn convert_at<'js>(
    val: &JsValue<'js>,
    obj_proto: &Object<'js>,
    depth: usize,
    skip_undefined_props: bool,
) -> Result<Value, ConvertError> {
    if depth > MAX_JSON_DEPTH {
        return Err(ConvertError::MaxDepthExceeded);
    }
    match val.type_of() {
        Type::Null => Ok(Value::Null),
        Type::Bool => Ok(Value::Bool(val.as_bool().unwrap_or(false))),
        Type::Int => Ok(Value::Number(Number::from(i64::from(
            val.as_int().unwrap_or(0),
        )))),
        Type::Float => {
            let f = val.as_float().unwrap_or(f64::NAN);
            match validate_f64(f) {
                Ok(n) => Ok(Value::Number(n)),
                Err(CanonicalError::NonFiniteNumber) => Err(ConvertError::NonFiniteNumber),
                Err(CanonicalError::UnsafeInteger { value }) => {
                    Err(ConvertError::UnsafeInteger(value))
                }
                Err(CanonicalError::MaxDepthExceeded { .. }) => Err(ConvertError::MaxDepthExceeded),
            }
        }
        Type::BigInt => Err(ConvertError::UnsafeInteger("BigInt".into())),
        Type::String => {
            let s = val
                .as_string()
                .ok_or_else(|| ConvertError::UnsupportedValue("invalid string".into()))?
                .to_string()
                .map_err(|e| ConvertError::JsException(e.to_string()))?;
            Ok(Value::String(s))
        }
        Type::Array => {
            let arr = val
                .as_array()
                .ok_or_else(|| ConvertError::UnsupportedValue("invalid array".into()))?;
            let len = arr.len();
            let mut out = Vec::with_capacity(len);
            for i in 0..len {
                let elem: JsValue<'js> = arr
                    .get(i)
                    .map_err(|e| ConvertError::JsException(e.to_string()))?;
                out.push(convert_at(
                    &elem,
                    obj_proto,
                    depth + 1,
                    skip_undefined_props,
                )?);
            }
            Ok(Value::Array(out))
        }
        Type::Object => {
            let obj = val
                .as_object()
                .ok_or_else(|| ConvertError::UnsupportedValue("invalid object".into()))?;
            if !is_plain_object(obj, obj_proto) {
                return Err(ConvertError::UnsupportedValue(
                    "only plain JSON objects are permitted".into(),
                ));
            }
            let mut sorted = BTreeMap::new();
            for entry in obj.props::<String, JsValue<'js>>() {
                let (k, v) = entry.map_err(|e| ConvertError::JsException(e.to_string()))?;
                if skip_undefined_props && v.type_of() == Type::Undefined {
                    continue;
                }
                let converted = convert_at(&v, obj_proto, depth + 1, skip_undefined_props)?;
                sorted.insert(k, converted);
            }
            let mut map = Map::with_capacity(sorted.len());
            for (k, v) in sorted {
                map.insert(k, v);
            }
            Ok(Value::Object(map))
        }
        other => Err(ConvertError::UnsupportedValue(format!(
            "value of JS type {other:?} cannot be serialized canonically"
        ))),
    }
}

fn is_plain_object<'js>(obj: &Object<'js>, obj_proto: &Object<'js>) -> bool {
    match obj.get_prototype() {
        None => true, // Object.create(null)
        Some(proto) => proto == *obj_proto,
    }
}

/// Converts a validated canonical `serde_json::Value` into a fresh QuickJS value.
pub fn json_to_js<'js>(ctx: &Ctx<'js>, value: &Value) -> Result<JsValue<'js>, ConvertError> {
    validate_json_value(value).map_err(|e| match e {
        CanonicalError::NonFiniteNumber => ConvertError::NonFiniteNumber,
        CanonicalError::UnsafeInteger { value } => ConvertError::UnsafeInteger(value),
        CanonicalError::MaxDepthExceeded { .. } => ConvertError::MaxDepthExceeded,
    })?;
    let json = to_canonical_json(value).map_err(|e| match e {
        CanonicalError::NonFiniteNumber => ConvertError::NonFiniteNumber,
        CanonicalError::UnsafeInteger { value } => ConvertError::UnsafeInteger(value),
        CanonicalError::MaxDepthExceeded { .. } => ConvertError::MaxDepthExceeded,
    })?;
    ctx.json_parse(json)
        .map_err(|e| ConvertError::JsException(e.to_string()))
}
