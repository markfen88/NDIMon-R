//! A bounded JSON-compatible YAML subset; never retain Wi-Fi credentials.
use crate::{MAX_PRIVATE, Result, unsupported};
use serde_json::Value;
use yaml_rust2::{Yaml, YamlLoader};
pub(crate) fn parse(text: &str) -> Result<Value> {
    if text.len() > MAX_PRIVATE {
        return Err(unsupported("unsafe_configuration"));
    }
    // Bound nesting before the DOM loader allocates recursively. Tabs/aliases
    // and deeply indented input are conservatively refused for source rewrites.
    let mut nesting = 0usize;
    let mut quoted = None;
    let mut escaped = false;
    let mut previous = '\n';
    for c in text.chars() {
        if let Some(q) = quoted {
            if c == q && !escaped {
                quoted = None;
            }
            escaped = c == '\\' && !escaped;
            previous = c;
            continue;
        }
        match c {
            '\'' | '"' => quoted = Some(c),
            '[' | '{' => {
                nesting += 1;
                if nesting > 64 {
                    return Err(unsupported("unsafe_configuration"));
                }
            }
            ']' | '}' => nesting = nesting.saturating_sub(1),
            '*' | '&' if previous.is_whitespace() || [':', ',', '[', '{'].contains(&previous) => {
                return Err(unsupported("unsafe_configuration"));
            }
            _ => (),
        }
        previous = c;
    }
    if text
        .lines()
        .any(|l| l.chars().take_while(|c| c.is_whitespace()).count() > 128 || l.starts_with('\t'))
    {
        return Err(unsupported("unsafe_configuration"));
    }
    let docs = YamlLoader::load_from_str(text).map_err(|_| unsupported("unsafe_configuration"))?;
    if docs.len() != 1 {
        return Err(unsupported("unsafe_configuration"));
    }
    convert(&docs[0], 0)
}
fn convert(v: &Yaml, depth: usize) -> Result<Value> {
    if depth > 64 {
        return Err(unsupported("unsafe_configuration"));
    }
    match v {
        Yaml::Null => Ok(Value::Null),
        Yaml::Boolean(b) => Ok(Value::Bool(*b)),
        Yaml::Integer(i) => Ok(Value::Number((*i).into())),
        Yaml::Real(f) => f
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| unsupported("unsafe_configuration")),
        Yaml::String(s) => Ok(Value::String(s.clone())),
        Yaml::Array(a) => a
            .iter()
            .map(|v| convert(v, depth + 1))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Yaml::Hash(m) => {
            let mut out = serde_json::Map::new();
            for (k, v) in m {
                out.insert(
                    k.as_str()
                        .ok_or_else(|| unsupported("unsafe_configuration"))?
                        .into(),
                    convert(v, depth + 1)?,
                );
            }
            Ok(Value::Object(out))
        }
        _ => Err(unsupported("unsafe_configuration")),
    }
}
pub(crate) fn emit(v: &Value) -> Result<String> {
    serde_json::to_string_pretty(v)
        .map(|s| s + "\n")
        .map_err(|_| unsupported("unrepresentable_configuration"))
}
