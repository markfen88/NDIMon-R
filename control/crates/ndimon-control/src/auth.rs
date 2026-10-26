//! Legacy scrypt verification, Argon2id upgrades and privileged auth persistence.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use ndimon_privd_proto::{CONTROL_SOCKET, LegacyConfigFile, Request, client::Client};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub fn random_hex(bytes: usize) -> String {
    let mut value = vec![0; bytes];
    getrandom::fill(&mut value).expect("operating system randomness");
    hex::encode(value)
}

pub fn same(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

fn scrypt_hash(password: &str, salt: &str) -> Result<String, String> {
    let salt = hex::decode(salt).map_err(|_| "invalid scrypt salt")?;
    let mut output = [0; 32];
    // crypto.scryptSync(String(password), Buffer.from(salt, 'hex'), 32):
    // Node's default cost=16384, blockSize=8, parallelization=1 (not a PHC salt).
    let params = scrypt::Params::new(14, 8, 1, 32).map_err(|_| "invalid scrypt parameters")?;
    scrypt::scrypt(password.as_bytes(), &salt, &params, &mut output)
        .map_err(|_| "scrypt failed")?;
    Ok(hex::encode(output))
}

fn argon_hash(password: &str) -> Result<String, String> {
    let mut salt = [0; 16];
    getrandom::fill(&mut salt).map_err(|_| "randomness unavailable")?;
    let salt = SaltString::encode_b64(&salt).map_err(|_| "invalid salt")?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| "password hashing failed".into())
}

#[derive(Debug, Clone)]
pub struct PasswordFile(pub PathBuf);

impl PasswordFile {
    fn bytes(&self) -> Result<Vec<u8>, String> {
        match std::fs::read(&self.0) {
            Ok(bytes) if bytes.len() <= 16 * 1024 => Ok(bytes),
            Ok(_) => Err("auth file too large".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(_) => Err("cannot read auth file".into()),
        }
    }

    pub fn stamp(&self) -> Result<[u8; 32], String> {
        Ok(Sha256::digest(self.bytes()?).into())
    }

    pub fn is_default(&self) -> bool {
        self.bytes().is_ok_and(|bytes| bytes.is_empty())
    }

    fn check(&self, password: &str) -> Result<(bool, Option<Value>, [u8; 32]), String> {
        let bytes = self.bytes()?;
        let stamp = Sha256::digest(&bytes).into();
        if bytes.is_empty() {
            return Ok((
                bool::from(Sha256::digest(password.as_bytes()).ct_eq(&Sha256::digest(b"ndimon"))),
                None,
                stamp,
            ));
        }
        let mut stored: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid auth file")?;
        let salt = stored
            .get("salt")
            .and_then(Value::as_str)
            .ok_or("missing salt")?;
        let hash = stored
            .get("hash")
            .and_then(Value::as_str)
            .ok_or("missing hash")?;
        match stored
            .get("algo")
            .and_then(Value::as_str)
            .unwrap_or("scrypt")
        {
            "argon2id" => {
                let phc = stored
                    .get("phc")
                    .and_then(Value::as_str)
                    .ok_or("missing PHC hash")?;
                if !bounded_phc(phc) {
                    return Err("invalid Argon2id hash".into());
                }
                let parsed = PasswordHash::new(phc).map_err(|_| "invalid PHC hash")?;
                Ok((
                    Argon2::default()
                        .verify_password(password.as_bytes(), &parsed)
                        .is_ok(),
                    None,
                    stamp,
                ))
            }
            "scrypt" => {
                if salt.len() != 32 || hash.len() != 64 {
                    return Err("invalid scrypt record".into());
                }
                let candidate = scrypt_hash(password, salt)?;
                if !same(&candidate, hash) {
                    return Ok((false, None, stamp));
                }
                stored["algo"] = json!("argon2id");
                stored["phc"] = json!(argon_hash(password)?);
                Ok((true, Some(stored), stamp))
            }
            _ => Err("unsupported password algorithm".into()),
        }
    }

    /// Hashing runs off the async executor; the caller serializes local writes.
    pub async fn verify_and_upgrade(&self, password: String) -> Result<Option<[u8; 32]>, String> {
        let file = self.clone();
        let (valid, upgrade, mut stamp) =
            tokio::task::spawn_blocking(move || file.check(&password))
                .await
                .map_err(|_| "password worker failed")??;
        if !valid {
            return Ok(None);
        }
        // A backup restore can run independently of the local auth gate.
        // Never attach a login verified against one record to another record.
        if self.stamp()? != stamp {
            return Err("auth record changed during login".into());
        }
        if let Some(record) = upgrade {
            stamp = Sha256::digest(serde_json::to_vec(&record).map_err(|_| "invalid auth record")?)
                .into();
            self.write(record).await?;
        }
        if self.stamp()? != stamp {
            return Err("auth record changed during login".into());
        }
        Ok(Some(stamp))
    }

    pub async fn set(&self, password: String) -> Result<[u8; 32], String> {
        let record = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            let salt = random_hex(16);
            Ok(json!({"algo": "argon2id", "phc": argon_hash(&password)?,
                "hash": scrypt_hash(&password, &salt)?, "salt": salt}))
        })
        .await
        .map_err(|_| "password worker failed")??;
        let stamp =
            Sha256::digest(serde_json::to_vec(&record).map_err(|_| "invalid auth record")?).into();
        self.write(record).await?;
        if self.stamp()? != stamp {
            return Err("auth record changed during password update".into());
        }
        Ok(stamp)
    }

    async fn write(&self, record: Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(&record).map_err(|_| "cannot serialize auth record")?;
        if self.0 == Path::new("/etc/ndimon-auth.json") {
            // privd owns mode 0640, root:ndimon, temp+fsync+rename and directory
            // fsync. The control service never writes /etc or invokes sudo.
            write_privd(
                &Client::new(CONTROL_SOCKET, Duration::from_secs(10)),
                record,
            )
            .await
        } else {
            // Development/tests only: same durable replacement, no privilege.
            if self.0.parent() == Some(Path::new("/etc")) {
                return Err("only /etc/ndimon-auth.json is writable".into());
            }
            let path = self.0.clone();
            tokio::task::spawn_blocking(move || atomic_write(&path, &bytes))
                .await
                .map_err(|_| "auth writer failed")?
                .map_err(|_| "auth writer failed".into())
        }
    }
}

async fn write_privd(client: &Client, record: Value) -> Result<(), String> {
    let write = client.execute(Request::WriteLegacyConfig {
        file: LegacyConfigFile::Auth,
        json: record,
    });
    tokio::time::timeout(Duration::from_secs(10), write)
        .await
        .map_err(|_| "auth writer timed out")?
        .map(|_| ())
        .map_err(|_| "auth writer refused the record".into())
}

/// Bound imported PHC work factors before allowing the password verifier to
/// allocate memory. Backup.js enforces the same bounds, including on rollback.
fn bounded_phc(phc: &str) -> bool {
    if phc.len() > 512 {
        return false;
    }
    let fields: Vec<_> = phc.split('$').collect();
    if fields.len() != 6 || fields[1] != "argon2id" || fields[2] != "v=19" {
        return false;
    }
    let params: Vec<_> = fields[3].split(',').collect();
    params.len() == 3
        && params
            .iter()
            .zip([("m=", 65536), ("t=", 10), ("p=", 8)])
            .all(|(value, (prefix, max))| {
                value
                    .strip_prefix(prefix)
                    .and_then(|n| n.parse::<u32>().ok())
                    .is_some_and(|n| n > 0 && n <= max)
            })
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if let Ok(metadata) = path.symlink_metadata() {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(std::io::Error::other(
                "auth destination is not a regular file",
            ));
        }
    }
    let temporary = directory.join(format!(".ndimon-auth-{}", random_hex(16)));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o640)
            .open(&temporary)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o640))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        std::fs::File::open(directory)?.sync_all()
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn auth_writer_sends_typed_request_and_propagates_refusal() {
        use ndimon_privd_proto::{
            ActionOutput, Envelope, Error, Reply, ReplyResult, VERSION, codec,
        };

        for result in [
            ReplyResult::Ok(ActionOutput::default()),
            ReplyResult::Err(Error::PermissionDenied),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("privd.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let record = json!({"salt": "test salt", "hash": "test hash"});
            let expected = record.clone();
            let refused = matches!(&result, ReplyResult::Err(_));
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request: Envelope = codec::read_async(&mut stream).await.unwrap();
                assert_eq!(request.v, VERSION);
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
                        result,
                    },
                )
                .await
                .unwrap();
            });
            let outcome = write_privd(&Client::new(&socket, Duration::from_secs(10)), record).await;
            if refused {
                assert_eq!(outcome.unwrap_err(), "auth writer refused the record");
            } else {
                outcome.unwrap();
            }
            server.await.unwrap();
        }
    }

    #[test]
    fn node_scrypt_known_answer() {
        // Derived independently with hashlib.scrypt using Node's documented
        // defaults, binary salt 00..0f, UTF-8 password and dklen=32.
        assert_eq!(
            scrypt_hash("ndimon", "000102030405060708090a0b0c0d0e0f").unwrap(),
            "5bdc70d84c2c2aaf073ac74ce09122725191fb56e032f7ff4596db1d67a985c8"
        );
    }

    #[tokio::test]
    async fn legacy_upgrade_and_password_change_preserve_rollback_fields() {
        let dir = tempfile::tempdir().unwrap();
        let file = PasswordFile(dir.path().join("auth.json"));
        assert!(file.is_default());
        assert!(
            file.verify_and_upgrade("ndimon".into())
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            file.is_default(),
            "default warning remains until explicitly changed"
        );
        let salt = "000102030405060708090a0b0c0d0e0f";
        let legacy = json!({"salt": salt, "hash": scrypt_hash("test password", salt).unwrap(), "imported": true});
        std::fs::write(&file.0, legacy.to_string()).unwrap();
        let before = file.stamp().unwrap();
        assert!(
            file.verify_and_upgrade("wrong".into())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(before, file.stamp().unwrap());
        assert!(
            file.verify_and_upgrade("test password".into())
                .await
                .unwrap()
                .is_some()
        );
        let upgraded: Value = serde_json::from_slice(&std::fs::read(&file.0).unwrap()).unwrap();
        assert_eq!(
            std::fs::metadata(&file.0).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(upgraded["algo"], "argon2id");
        assert!(upgraded["phc"].as_str().unwrap().starts_with("$argon2id$"));
        assert_eq!(upgraded["salt"], legacy["salt"]);
        assert_eq!(upgraded["hash"], legacy["hash"]);
        assert_eq!(upgraded["imported"], true);
        assert_ne!(file.stamp().unwrap(), before);
        assert!(
            file.verify_and_upgrade("test password".into())
                .await
                .unwrap()
                .is_some()
        );
        file.set("new password".into()).await.unwrap();
        assert!(
            file.verify_and_upgrade("test password".into())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            file.verify_and_upgrade("new password".into())
                .await
                .unwrap()
                .is_some()
        );
        let record: Value = serde_json::from_slice(&std::fs::read(&file.0).unwrap()).unwrap();
        assert_eq!(
            record["hash"],
            scrypt_hash("new password", record["salt"].as_str().unwrap()).unwrap()
        );
    }

    #[test]
    fn imported_hash_cost_is_bounded() {
        assert!(bounded_phc("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"));
        assert!(!bounded_phc(
            "$argon2id$v=19$m=99999999,t=2,p=1$c2FsdA$aGFzaA"
        ));
    }
}
