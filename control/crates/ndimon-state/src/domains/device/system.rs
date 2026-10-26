//! Read-only System observations and signed release requirements.
use ndimon_types::{ApiError, FieldError, Query};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::process::Command;

pub fn read_trim(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path.trim_start_matches('/')))
        .unwrap_or_default()
        .replace('\0', "")
        .trim()
        .into()
}
/// Where the package builder records the commit and date it was built from.
pub const BUILD_INFO: &str = "/usr/share/ndimon/build-info";
/// `commit=` and `date=` lines of a build-info file (HW-1 H7).
pub fn parse_build_info(text: &str) -> (String, String) {
    let value = |key: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    (value("commit"), value("date"))
}
/// The commit and build date of what is installed. A package carries them in
/// `/usr/share/ndimon/build-info`; `/etc/ndimon-build-{commit,date}` belong to a
/// source install and are left over, stale, by the installer of an earlier release
/// (the ROCK 5B reported the beta's commit next to firmware 1.3.1), so they only
/// answer when the package says nothing.
pub fn build_info(root: &Path) -> (String, String) {
    let (commit, date) = parse_build_info(&read_trim(root, BUILD_INFO));
    (
        if commit.is_empty() {
            read_trim(root, "/etc/ndimon-build-commit")
        } else {
            commit
        },
        if date.is_empty() {
            read_trim(root, "/etc/ndimon-build-date")
        } else {
            date
        },
    )
}
async fn output(program: &str, args: &[&str], seconds: u64) -> Option<String> {
    let result = tokio::time::timeout(
        Duration::from_secs(seconds),
        Command::new(program).args(args).kill_on_drop(true).output(),
    )
    .await
    .ok()?
    .ok()?;
    result
        .status
        .success()
        .then(|| String::from_utf8_lossy(&result.stdout).trim().into())
}
pub fn vaapi(raw: &str) -> Value {
    if raw.is_empty() {
        return json!({"available":false});
    }
    let mut driver = String::new();
    let mut profiles = Vec::new();
    for line in raw.lines() {
        if let Some((_, value)) = line.split_once("Driver version:") {
            driver = value.trim().into();
        }
        if let Some((_, rest)) = line.split_once("VAProfile") {
            if let Some((profile, entry)) = rest.split_once(':') {
                let profile = profile.trim();
                let lower = profile.to_ascii_lowercase();
                if entry.trim().starts_with("VAEntrypointVLD")
                    && ["h264", "hevc", "vp9", "av1"]
                        .iter()
                        .any(|codec| lower.contains(codec))
                    && !profiles.iter().any(|p| p == profile)
                {
                    profiles.push(profile.to_string());
                }
            }
        }
    }
    json!({"available":true,"driver":driver,"decode_profiles":profiles})
}
pub async fn version(root: &Path) -> Value {
    let fallback = |path, default: &str| {
        let v = read_trim(root, path);
        if v.is_empty() { default.into() } else { v }
    };
    let configured = release_keys_configured(root).unwrap_or(false);
    let (commit, build_date) = build_info(root);
    json!({"firmware":fallback("/etc/ndimon-firmware-version","1.4.0"),"commit":commit,"build_date":build_date,"ndi_version":fallback("/etc/ndimon-ndi-version","6.x"),"update_supported":configured,"update_running":false,"update_message":if configured {"updates need a signed release version"} else {NO_RELEASE_KEY}})
}
/// Add what the update agent's release lookup found to the version reply (only
/// asked when a release key is configured). The web page installs
/// `latest_version`, so it is set only for a release the agent verified as newer;
/// a lookup that failed leaves the counts out and says why in the message.
pub fn add_latest_release(version: &mut Value, lookup: Result<Option<String>, String>) {
    match lookup {
        Ok(Some(latest)) => {
            version["update_message"] = json!(format!("release {latest} is available"));
            version["updates_available"] = json!(1);
            version["latest_version"] = json!(latest);
        }
        Ok(None) => {
            version["update_message"] = json!("no newer signed release");
            version["updates_available"] = json!(0);
        }
        Err(reason) => {
            version["update_message"] =
                json!(format!("could not check for a signed release: {reason}"));
        }
    }
}
pub const NO_RELEASE_KEY: &str = "updates need a signed release; no release key is configured";
pub fn release_keys_configured(root: &Path) -> Result<bool, ApiError> {
    ndimon_release::trust::load_keys(
        &root.join(ndimon_release::trust::OVERRIDE_FILE.trim_start_matches('/')),
    )
    .map(|keys| !keys.is_empty())
    .map_err(|error| ApiError::Unsupported {
        what: error.to_string(),
    })
}
pub fn check_signed_update(version: &str, force: bool) -> Result<(), ApiError> {
    if !release_keys_configured(Path::new("/"))? {
        return Err(ApiError::Unsupported {
            what: NO_RELEASE_KEY.into(),
        });
    }
    if std::env::var("NDIMON_PRIVD").as_deref() == Ok("0") {
        return Err(ApiError::Unsupported {
            what: "signed updates require ndimon-privd".into(),
        });
    }
    signed_request(version, force).map(|_| ())
}
pub fn signed_request(version: &str, force: bool) -> Result<ndimon_privd_proto::Request, ApiError> {
    serde_json::from_value(json!({"install_update":{"version":version,"force":force}}))
        .map_err(|e| ApiError::invalid(vec![FieldError::new("version", e.to_string())]))
}
pub fn priv_error(error: ndimon_privd_proto::Error) -> String {
    match error {
        ndimon_privd_proto::Error::Busy => "update already running".into(),
        error => error.to_string(),
    }
}
pub async fn observe(query: Query) -> Result<Value, String> {
    let root = Path::new("/");
    let hostname = || read_trim(root, "/proc/sys/kernel/hostname");
    Ok(match query {
        Query::Version => version(root).await,
        Query::VaapiInfo => vaapi(&read_trim(root, "/etc/ndimon-vaapi-info")),
        Query::Hostname => json!({"hostname":hostname()}),
        Query::Ntp => {
            let synchronized = output(
                "timedatectl",
                &["show", "-p", "NTPSynchronized", "--value"],
                4,
            )
            .await;
            let active = output(
                "timedatectl",
                &["show-timesync", "-p", "ServerName", "--value"],
                4,
            )
            .await
            .unwrap_or_default();
            json!({"synchronized":synchronized.as_deref()==Some("yes"),"active_server":active})
        }
        Query::RebootSchedule => {
            let raw = output(
                "systemctl",
                &[
                    "show",
                    "ndimon-scheduled-reboot.timer",
                    "-p",
                    "NextElapseUSecRealtime",
                    "--value",
                ],
                3,
            )
            .await;
            let mut next = None;
            if let Some(us) = raw.and_then(|v| v.parse::<u64>().ok()).filter(|v| *v > 0) {
                next = output(
                    "date",
                    &[
                        "--utc",
                        "--date",
                        &format!("@{}.{:06}", us / 1_000_000, us % 1_000_000),
                        "+%Y-%m-%dT%H:%M:%S.%3NZ",
                    ],
                    2,
                )
                .await;
            }
            json!({"next":next})
        }
        Query::AboutMe => {
            let interfaces = output("ip", &["-j", "-4", "address", "show"], 3)
                .await
                .and_then(|v| serde_json::from_str::<Value>(&v).ok())
                .unwrap_or(json!([]));
            let ip = interfaces
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|v| v["addr_info"].as_array().into_iter().flatten())
                .find(|v| v["scope"] != "host" && v["family"] == "inet")
                .and_then(|v| v["local"].as_str())
                .unwrap_or("0.0.0.0");
            let fallback = |path, default: &str| {
                let v = read_trim(root, path);
                if v.is_empty() { default.into() } else { v }
            };
            let arch = match std::env::consts::ARCH {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                "x86" => "ia32",
                "arm" => "arm",
                other => other,
            };
            json!({"DeviceName":"NDI Decoder","HostName":hostname(),"IPAddress":ip,"FirmwareVersion":fallback("/etc/ndimon-firmware-version","1.4.0"),"Model":fallback("/proc/device-tree/model",&format!("aarch64 ({arch})")),"NDIVersion":fallback("/etc/ndimon-ndi-version","6.x"),"BuildDate":build_info(root).1,"chInfo":[{"ChNum":1,"Mode":"decode","Status":"active","Stream":""}]})
        }
        _ => return Err("unsupported System observation".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let path = dir.path().join(path.trim_start_matches('/'));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        dir
    }

    #[test] // HW-1 H7: the package's own commit and date, not the beta installer's.
    fn the_installed_package_answers_before_the_leftover_etc_files() {
        let stale = [
            ("/etc/ndimon-build-commit", "440d54e\n"),
            ("/etc/ndimon-build-date", "2026-10-19\n"),
        ];
        let package = (
            "/usr/share/ndimon/build-info",
            "commit=b533ca2f0a11\ndate=2026-10-26\n",
        );
        let mut both = stale.to_vec();
        both.push(package);
        assert_eq!(
            build_info(root(&both).path()),
            ("b533ca2f0a11".into(), "2026-10-26".into())
        );
        // A source install has only the /etc files.
        assert_eq!(
            build_info(root(&stale).path()),
            ("440d54e".into(), "2026-10-19".into())
        );
        // A package built outside git says so rather than lying.
        let unknown = [(
            "/usr/share/ndimon/build-info",
            "commit=unknown\ndate=unknown\n",
        )];
        assert_eq!(
            build_info(root(&unknown).path()),
            ("unknown".into(), "unknown".into())
        );
        assert_eq!(build_info(root(&[]).path()), (String::new(), String::new()));
    }

    #[test]
    fn build_info_ignores_other_lines_and_whitespace() {
        assert_eq!(
            parse_build_info("# note\n commit=abc \nrelease=1\ndate = x\ndate=2026-01-02\n"),
            ("abc".into(), "2026-01-02".into())
        );
        assert_eq!(parse_build_info(""), (String::new(), String::new()));
    }

    #[tokio::test] // The route reports it next to the firmware version.
    async fn version_reports_the_package_commit() {
        let dir = root(&[
            ("/etc/ndimon-firmware-version", "1.3.1-1\n"),
            ("/etc/ndimon-build-commit", "440d54e\n"),
            ("/etc/ndimon-build-date", "2026-10-19\n"),
            (
                "/usr/share/ndimon/build-info",
                "commit=b533ca2f0a11\ndate=2026-10-26\n",
            ),
        ]);
        let v = version(dir.path()).await;
        assert_eq!(v["firmware"], "1.3.1-1");
        assert_eq!(v["commit"], "b533ca2f0a11");
        assert_eq!(v["build_date"], "2026-10-26");
    }
}
