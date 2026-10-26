//! The frozen 1.3.0 Ajv schema and its validation vocabulary/error ordering.
//! No coercion, defaults, property removal or format validation is enabled.
use regex::Regex;
use serde_json::{Value, json};
use std::sync::OnceLock;

pub fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA
        .get_or_init(|| serde_json::from_str(include_str!("schema1.json")).expect("schema-1 JSON"))
}
fn pointer(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}
fn error(out: &mut Vec<String>, path: &str, message: impl AsRef<str>) {
    out.push(format!(
        "{} {}",
        if path.is_empty() { "/" } else { path },
        message.as_ref()
    ));
}
fn matching(schema: &Value, value: &Value) -> bool {
    let mut errors = Vec::new();
    walk(schema, value, "", &mut errors);
    errors.is_empty()
}
pub fn errors(schema: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    walk(schema, value, "", &mut errors);
    errors
}
pub fn check(value: &Value) -> Result<(), String> {
    let errors = errors(schema(), value);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}
fn walk(s: &Value, v: &Value, path: &str, out: &mut Vec<String>) {
    if s == &json!(false) {
        error(out, path, "boolean schema is false");
        return;
    }
    if let Some(kind) = s["type"].as_str() {
        let valid = match kind {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "boolean" => v.is_boolean(),
            "null" => v.is_null(),
            "number" => v.is_number(),
            "integer" => v.as_f64().is_some_and(|n| n.fract() == 0.0),
            _ => panic!("unsupported schema type"),
        };
        if !valid {
            error(out, path, format!("must be {kind}"));
        }
    }
    if s.get("const").is_some_and(|c| !equal(c, v)) {
        error(out, path, "must be equal to constant");
    }
    if s["enum"]
        .as_array()
        .is_some_and(|a| !a.iter().any(|c| equal(c, v)))
    {
        error(out, path, "must be equal to one of the allowed values");
    }
    if let Some(not) = s.get("not") {
        if matching(not, v) {
            error(out, path, "must NOT be valid");
        }
    }
    if let Some(branches) = s["anyOf"].as_array() {
        let mut branch_errors = Vec::new();
        let mut ok = false;
        for branch in branches {
            let mut errors = Vec::new();
            walk(branch, v, path, &mut errors);
            if errors.is_empty() {
                ok = true;
                break;
            }
            branch_errors.extend(errors);
        }
        if !ok {
            out.extend(branch_errors);
            error(out, path, "must match a schema in anyOf");
        }
    }
    for branch in s["allOf"].as_array().into_iter().flatten() {
        walk(branch, v, path, out);
    }
    if let Some(condition) = s.get("if") {
        let key = if matching(condition, v) {
            "then"
        } else {
            "else"
        };
        if let Some(branch) = s.get(key) {
            let start = out.len();
            walk(branch, v, path, out);
            if out.len() != start {
                error(out, path, format!("must match \"{key}\" schema"));
            }
        }
    }
    if let Some(n) = v.as_f64() {
        for (key, sign) in [("maximum", "<="), ("minimum", ">=")] {
            if let Some(limit) = s[key].as_f64() {
                if (key == "maximum" && n > limit) || (key == "minimum" && n < limit) {
                    error(out, path, format!("must be {sign} {}", s[key]));
                }
            }
        }
    }
    if let Some(text) = v.as_str() {
        for (key, sign) in [("maxLength", "more"), ("minLength", "fewer")] {
            if let Some(limit) = s[key].as_u64() {
                let length = text.chars().count() as u64;
                if (key == "maxLength" && length > limit) || (key == "minLength" && length < limit)
                {
                    error(
                        out,
                        path,
                        format!("must NOT have {sign} than {limit} characters"),
                    );
                }
            }
        }
        if let Some(pattern) = s["pattern"].as_str() {
            if !Regex::new(pattern).expect("schema pattern").is_match(text) {
                error(out, path, format!("must match pattern \"{pattern}\""));
            }
        }
    }
    if let Some(array) = v.as_array() {
        for (key, sign) in [("maxItems", "more"), ("minItems", "fewer")] {
            if let Some(limit) = s[key].as_u64() {
                if (key == "maxItems" && array.len() as u64 > limit)
                    || (key == "minItems" && (array.len() as u64) < limit)
                {
                    error(
                        out,
                        path,
                        format!("must NOT have {sign} than {limit} items"),
                    );
                }
            }
        }
        if let Some(items) = s.get("items") {
            for (i, value) in array.iter().enumerate() {
                walk(items, value, &format!("{path}/{i}"), out);
            }
        }
    }
    if let Some(object) = v.as_object() {
        if let Some(max) = s["maxProperties"].as_u64() {
            if object.len() as u64 > max {
                error(
                    out,
                    path,
                    format!("must NOT have more than {max} properties"),
                );
            }
        }
        for required in s["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !object.contains_key(required) {
                error(
                    out,
                    path,
                    format!("must have required property '{required}'"),
                );
            }
        }
        if let Some(names) = s.get("propertyNames") {
            for key in object.keys() {
                let start = out.len();
                walk(names, &json!(key), path, out);
                if start != out.len() {
                    error(out, path, "property name must be valid");
                }
            }
        }
        for (key, value) in object {
            let known = s["properties"].get(key).is_some();
            let pattern = s["patternProperties"].as_object().is_some_and(|p| {
                p.keys()
                    .any(|p| Regex::new(p).expect("property pattern").is_match(key))
            });
            if !known && !pattern {
                match s.get("additionalProperties") {
                    Some(Value::Bool(false)) => {
                        error(out, path, "must NOT have additional properties")
                    }
                    Some(Value::Object(_)) => {
                        walk(&s["additionalProperties"], value, &pointer(path, key), out)
                    }
                    _ => {}
                }
            }
        }
        for (key, property) in s["properties"].as_object().into_iter().flatten() {
            if let Some(value) = object.get(key) {
                walk(property, value, &pointer(path, key), out);
            }
        }
        for (pattern, property) in s["patternProperties"].as_object().into_iter().flatten() {
            let pattern = Regex::new(pattern).expect("property pattern");
            for (key, value) in object {
                if pattern.is_match(key) {
                    walk(property, value, &pointer(path, key), out);
                }
            }
        }
    }
}
fn equal(a: &Value, b: &Value) -> bool {
    a == b || a.as_f64().zip(b.as_f64()).is_some_and(|(a, b)| a == b)
}
