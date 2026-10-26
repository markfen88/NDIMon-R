//! JSON.stringify field order for the historical Device/System envelopes.
use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::Value;

fn encode(value: &Value) -> Result<Vec<u8>, String> {
    let Value::Object(map) = value else {
        if let Value::Array(items) = value {
            let mut bytes = vec![b'['];
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    bytes.push(b',');
                }
                bytes.extend(encode(item)?);
            }
            bytes.push(b']');
            return Ok(bytes);
        }
        return ndimon_render::node_json(value.clone());
    };
    let order: &[&str] = if map.contains_key("firmware") {
        &[
            "firmware",
            "commit",
            "build_date",
            "ndi_version",
            "update_supported",
            "update_running",
            "updates_available",
            "update_message",
        ]
    } else if map.contains_key("DeviceName") {
        &[
            "DeviceName",
            "HostName",
            "IPAddress",
            "FirmwareVersion",
            "Model",
            "NDIVersion",
            "BuildDate",
            "chInfo",
        ]
    } else if map.contains_key("ChNum") {
        &["ChNum", "Mode", "Status", "Stream"]
    } else if map.contains_key("available") {
        &["available", "driver", "decode_profiles"]
    } else if map.contains_key("time") && map.contains_key("days") {
        &["ok", "error", "enabled", "time", "days", "next"]
    } else if map.contains_key("ntp_server") {
        &["ok", "error", "ntp_server", "synchronized", "active_server"]
    } else {
        &[
            "ok",
            "error",
            "message",
            "job",
            "ndi_recv_name",
            "decode_mode",
            "watchdog_mode",
            "hostname",
        ]
    };
    let mut keys: Vec<&str> = order
        .iter()
        .copied()
        .filter(|k| map.contains_key(*k))
        .collect();
    keys.extend(
        map.keys()
            .map(String::as_str)
            .filter(|k| !order.contains(k)),
    );
    let mut bytes = vec![b'{'];
    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            bytes.push(b',');
        }
        bytes.extend(serde_json::to_vec(key).map_err(|e| e.to_string())?);
        bytes.push(b':');
        bytes.extend(encode(&map[*key])?);
    }
    bytes.push(b'}');
    Ok(bytes)
}
pub fn response(status: u16, value: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        [(header::CONTENT_TYPE, "application/json")],
        encode(&value)
            .unwrap_or_else(|_| b"{\"ok\":false,\"error\":\"json encoding failed\"}".to_vec()),
    )
        .into_response()
}
pub fn success(value: Value) -> Response {
    response(200, value)
}

// ApiSettings returns a compact JSON.stringify of the saved object's key order.
// Deserialize into bytes so unknown nested metadata keeps that order too.
struct OrderedJson(Vec<u8>);
impl<'de> serde::Deserialize<'de> for OrderedJson {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> serde::de::Visitor<'de> for JsonVisitor {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON value")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(if v {
                    b"true".to_vec()
                } else {
                    b"false".to_vec()
                })
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(v.to_string().into_bytes())
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(v.to_string().into_bytes())
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                ndimon_render::node_json(serde_json::json!(v)).map_err(E::custom)
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                serde_json::to_vec(v).map_err(E::custom)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(b"null".to_vec())
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Self::Value, A::Error> {
                let mut out = vec![b'['];
                let mut first = true;
                while let Some(OrderedJson(v)) = a.next_element()? {
                    if !first {
                        out.push(b',');
                    }
                    first = false;
                    out.extend(v);
                }
                out.push(b']');
                Ok(out)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Self::Value, A::Error> {
                use serde::de::Error;
                let mut out = vec![b'{'];
                let mut first = true;
                while let Some((key, OrderedJson(v))) = a.next_entry::<String, OrderedJson>()? {
                    if !first {
                        out.push(b',');
                    }
                    first = false;
                    out.extend(serde_json::to_vec(&key).map_err(A::Error::custom)?);
                    out.push(b':');
                    out.extend(v);
                }
                out.push(b'}');
                Ok(out)
            }
        }
        d.deserialize_any(JsonVisitor).map(OrderedJson)
    }
}
pub fn compact(raw: &[u8]) -> Result<Vec<u8>, String> {
    serde_json::from_slice::<OrderedJson>(raw)
        .map(|v| v.0)
        .map_err(|e| e.to_string())
}
