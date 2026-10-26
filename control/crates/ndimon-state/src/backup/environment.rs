//! Backup file reads and the sole privileged password-hash writer.
use crate::BoxFuture;
use ndimon_privd_proto::{LegacyConfigFile, Request, client::Client};
use serde_json::Value;
use std::{fmt::Debug, path::PathBuf, time::Duration};

pub trait BackupIo: Debug + Send + Sync {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String>;
    fn hostname(&self) -> String;
    fn platform(&self) -> String;
    fn mac_suffix(&self) -> String;
    fn import_auth(&self, record: Value) -> BoxFuture<'_, Result<(), String>>;
}

#[derive(Debug)]
pub struct SystemIo {
    pub root: PathBuf,
    pub auth_file: PathBuf,
    pub privd: Client,
}
impl Default for SystemIo {
    fn default() -> Self {
        Self {
            root: "/".into(),
            auth_file: "/etc/ndimon-auth.json".into(),
            privd: Client::new(ndimon_privd_proto::CONTROL_SOCKET, Duration::from_secs(10)),
        }
    }
}
impl BackupIo for SystemIo {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        let path = if path == "/etc/ndimon-auth.json" {
            self.auth_file.clone()
        } else {
            self.root.join(path.trim_start_matches('/'))
        };
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err("cannot read backup input".into()),
        }
    }
    fn hostname(&self) -> String {
        // Linux exposes the UTS hostname without starting a subprocess.
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .unwrap_or_default()
            .trim()
            .into()
    }
    fn platform(&self) -> String {
        match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            "arm" => "arm",
            arch => arch,
        }
        .into()
    }
    fn mac_suffix(&self) -> String {
        let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
            return String::new();
        };
        let mut names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
        names.sort();
        for name in names {
            if name == "lo" {
                continue;
            }
            let Ok(mac) =
                std::fs::read_to_string(PathBuf::from("/sys/class/net").join(name).join("address"))
            else {
                continue;
            };
            let mac = mac.trim().replace(':', "");
            if mac.len() >= 6 && mac != "000000000000" {
                return mac[mac.len() - 6..].to_uppercase();
            }
        }
        String::new()
    }
    fn import_auth(&self, record: Value) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            tokio::time::timeout(
                Duration::from_secs(10),
                self.privd.execute(Request::WriteLegacyConfig {
                    file: LegacyConfigFile::Auth,
                    json: record,
                }),
            )
            .await
            .map_err(|_| "auth writer timed out")?
            .map(|_| ())
            .map_err(|_| "auth writer refused the record".into())
        })
    }
}

pub fn export_auth(io: &dyn BackupIo) -> Result<Option<Value>, String> {
    let Some(bytes) = io.read("/etc/ndimon-auth.json")? else {
        return Ok(None);
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    // Never include parser context: it could contain password-hash material.
    let record: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "Invalid backup auth file /etc/ndimon-auth.json: invalid JSON")?;
    let truthy = |value: &Value| match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.0),
        _ => true,
    };
    if !truthy(&record["salt"]) || !truthy(&record["hash"]) {
        return Ok(None);
    }
    let mut out = serde_json::json!({"salt":record["salt"],"hash":record["hash"]});
    if truthy(&record["algo"]) {
        out["algo"] = record["algo"].clone();
    }
    if record["algo"] == "argon2id" {
        if let Some(phc) = record.get("phc") {
            out["phc"] = phc.clone();
        }
    }
    Ok(Some(out))
}

pub fn import_auth(record: &Value) -> Result<Value, String> {
    let hex = |key: &str, length| {
        record[key].as_str().is_some_and(|s| {
            s.len() == length
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    };
    if !hex("salt", 32) {
        return Err("invalid password salt".into());
    }
    if !hex("hash", 64) {
        return Err("invalid password hash".into());
    }
    let mut out = serde_json::json!({"salt":record["salt"],"hash":record["hash"]});
    if let Some(algo) = record.get("algo") {
        if algo != "scrypt" && algo != "argon2id" {
            return Err("invalid password algorithm".into());
        }
        out["algo"] = algo.clone();
    }
    if record["algo"] == "argon2id" {
        let phc = record["phc"].as_str().unwrap_or("");
        let pattern = regex::Regex::new(r"^\$argon2id\$v=19\$m=([0-9]+),t=([0-9]+),p=([0-9]+)\$[A-Za-z0-9+/]{8,128}\$[A-Za-z0-9+/]{16,128}$").expect("PHC pattern");
        let valid = pattern.captures(phc).is_some_and(|m| {
            phc.len() <= 512
                && [65536, 10, 8]
                    .iter()
                    .enumerate()
                    .all(|(i, max)| m[i + 1].parse::<u64>().is_ok_and(|n| n > 0 && n <= *max))
        });
        if !valid {
            return Err("invalid Argon2id hash".into());
        }
        out["phc"] = record["phc"].clone();
    }
    out["updated"] = serde_json::json!(super::storage::iso(std::time::SystemTime::now()));
    out["imported"] = serde_json::json!(true);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndimon_privd_proto::{ActionOutput, Envelope, Error, Reply, ReplyResult, VERSION, codec};
    #[tokio::test]
    async fn auth_restore_uses_typed_privd_and_propagates_refusal() {
        for refuse in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("privd.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let record = serde_json::json!({"salt":"a".repeat(32),"hash":"b".repeat(64)});
            let expected = record.clone();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request: Envelope = codec::read_async(&mut stream).await.unwrap();
                assert_eq!(
                    request.req,
                    Request::WriteLegacyConfig {
                        file: LegacyConfigFile::Auth,
                        json: expected
                    }
                );
                codec::write_async(
                    &mut stream,
                    &Reply {
                        v: VERSION,
                        id: request.id,
                        result: if refuse {
                            ReplyResult::Err(Error::PermissionDenied)
                        } else {
                            ReplyResult::Ok(ActionOutput::default())
                        },
                    },
                )
                .await
                .unwrap();
            });
            let io = SystemIo {
                privd: Client::new(&socket, Duration::from_secs(10)),
                ..Default::default()
            };
            let result = io.import_auth(record).await;
            if refuse {
                assert_eq!(result.unwrap_err(), "auth writer refused the record");
            } else {
                result.unwrap();
            }
            server.await.unwrap();
        }
    }
}
