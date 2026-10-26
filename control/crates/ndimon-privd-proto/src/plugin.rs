//! Shared, closed plugin manifests and bounded data-only JSON schemas.
use crate::{DebianVersion, Error, PluginCommandName, PluginName, PluginUnit};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
fn default_routes() -> String {
    "routes.js".into()
}
fn default_ui() -> String {
    "ui.html".into()
}
fn default_backup() -> Option<String> {
    Some("backup.js".into())
}
fn default_install() -> String {
    "install.sh".into()
}
fn default_uninstall() -> String {
    "uninstall.sh".into()
}
fn default_icon() -> String {
    "bi-puzzle".into()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: PluginName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<Process>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<StateSchema>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "unique_commands"
    )]
    pub commands: BTreeMap<String, CommandSchema>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    pub version: DebianVersion,
    pub title: String,
    pub min_ndimon: DebianVersion,
    #[serde(default = "default_icon")]
    pub icon: String,
    #[serde(default = "default_routes")]
    pub routes: String,
    #[serde(default = "default_ui")]
    pub ui: String,
    #[serde(default = "default_backup")]
    pub backup: Option<String>,
    #[serde(default = "default_install")]
    pub install: String,
    #[serde(default = "default_uninstall")]
    pub uninstall: String,
    #[serde(default)]
    pub priv_commands: Vec<PluginCommandName>,
    #[serde(default)]
    pub services: Vec<PluginUnit>,
    #[serde(default)]
    pub supplementary_groups: Vec<String>,
    #[serde(default)]
    pub readwrite_paths: Vec<String>,
    #[serde(default)]
    pub config_files: Vec<String>,
}
fn simple_file(s: &str) -> bool {
    let Some((stem, ext)) = s.split_once('.') else {
        return false;
    };
    !stem.is_empty()
        && !ext.is_empty()
        && stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
}
fn safe_manifest_path(s: &str) -> bool {
    s.starts_with('/')
        && s.len() > 1
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
        && !s.split('/').any(|p| p == "..")
}
/// First-party plugins whose install, uninstall and privileged operations are
/// typed privd handlers. Their archives carry no executable entry points, so
/// neither the builder nor the installer demands `install`, `uninstall` or
/// `priv/<command>` files for them.
pub fn native_lifecycle(name: &str) -> bool {
    name == "tessera"
}
/// The architectures a plugin archive can be published for: the Debian names of
/// the devices that install it.
pub const ARCHIVE_ARCHITECTURES: [&str; 2] = ["amd64", "arm64"];
/// The name of a plugin archive in a release feed, `NAME-plugin_VERSION_ARCH.tar.gz`
/// (a `~` in the version becomes `-`). A feed offers a device only the archives
/// built for its own architecture; there is no architecture-independent name, so
/// a plugin that does not depend on it is published once per architecture.
pub fn archive_name(plugin: &str, version: &str, arch: &str) -> String {
    format!(
        "{plugin}-plugin_{}_{arch}.tar.gz",
        version.replace('~', "-")
    )
}
/// The `(name, version)` of a feed asset built for `arch`, or `None` when the
/// asset is no plugin archive or is built for another architecture.
pub fn parse_archive_name<'a>(file: &'a str, arch: &str) -> Option<(&'a str, &'a str)> {
    let (name, suffix) = file.split_once("-plugin_")?;
    let version = suffix.strip_suffix(&format!("_{arch}.tar.gz"))?;
    Some((name, version))
}
/// Host units are never plugin-scoped. The retired API and watchdog units stay
/// reserved so a plugin cannot adopt their names on an upgraded device.
pub fn permitted_service(name: &str, unit: &str) -> bool {
    ![
        "ssh.service",
        "sshd.service",
        "ndimon-r.service",
        "ndimon-api.service",
        "ndimon-finder.service",
        "ndimon-control.service",
        "ndimon-watchdog.service",
    ]
    .contains(&unit)
        && (name == "tessera" && unit == "tesserad.service"
            || name == "bluetooth" && unit == "bluealsa.service"
            || unit == format!("{name}.service")
            || [
                format!("{name}-"),
                format!("{name}@"),
                format!("ndimon-{name}."),
                format!("ndimon-{name}-"),
            ]
            .iter()
            .any(|p| unit.starts_with(p)))
}
impl Manifest {
    pub fn validate(
        &self,
        name: &PluginName,
        expected: Option<&DebianVersion>,
    ) -> Result<(), Error> {
        if &self.name != name {
            return Err(Error::invalid("manifest.name", "plugin name mismatch"));
        }
        if expected.is_some_and(|v| v != &self.version) {
            return Err(Error::invalid(
                "manifest.version",
                "bundle version mismatch",
            ));
        }
        if !(1..=40).contains(&self.title.chars().count())
            || !self.icon.starts_with("bi-")
            || self.icon.len() <= 3
            || !self.icon[3..]
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(Error::invalid("manifest", "invalid title or icon"));
        }
        if [&self.routes, &self.ui, &self.install, &self.uninstall]
            .into_iter()
            .chain(self.backup.iter())
            .any(|s| !simple_file(s))
        {
            return Err(Error::invalid("manifest", "invalid entry filename"));
        }
        let commands: Vec<_> = self.priv_commands.iter().map(|s| s.as_str()).collect();
        let services: Vec<_> = self.services.iter().map(|s| s.as_str()).collect();
        for list in [&commands, &services] {
            if list.iter().collect::<BTreeSet<_>>().len() != list.len() {
                return Err(Error::invalid("manifest", "duplicate declaration"));
            }
        }
        for list in [
            &self.supplementary_groups,
            &self.readwrite_paths,
            &self.config_files,
        ] {
            if list.iter().collect::<BTreeSet<_>>().len() != list.len() {
                return Err(Error::invalid("manifest", "duplicate declaration"));
            }
        }
        for group in &self.supplementary_groups {
            let _: PluginName = group
                .clone()
                .try_into()
                .map_err(|e| Error::invalid("supplementary_groups", e))?;
            if !permitted_group(name.as_str(), group) {
                return Err(Error::invalid("supplementary_groups", "privileged group"));
            }
        }
        for unit in &self.services {
            if !permitted_service(name.as_str(), unit.as_str()) {
                return Err(Error::invalid("services", "unscoped service"));
            }
        }
        for path in self.config_files.iter().chain(&self.readwrite_paths) {
            if !safe_manifest_path(path) {
                return Err(Error::invalid("manifest", "invalid configuration path"));
            }
        }
        for path in &self.readwrite_paths {
            if !["/run/", "/var/lib/", "/etc/"].iter().any(|base| {
                let scope = format!("{base}{name}");
                path == &scope
                    || path.starts_with(&(scope + "/"))
                    || name.as_str() == "tessera" && path.starts_with(&format!("{base}tessera"))
            }) {
                return Err(Error::invalid("readwrite_paths", "unscoped writable path"));
            }
        }
        self.validate_process()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Process {
    pub exec: String,
    #[serde(default)]
    pub args: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSchema {
    pub schema: Value,
    pub default: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSchema {
    pub input: Value,
    pub output: Value,
}
fn unique_commands<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, CommandSchema>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = BTreeMap<String, CommandSchema>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("unique command names")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate command name"));
                }
            }
            Ok(result)
        }
    }
    d.deserialize_map(Visitor)
}
pub fn permitted_group(name: &str, group: &str) -> bool {
    // The retired privileged-socket groups stay reserved on upgraded devices.
    ![
        "root",
        "sudo",
        "wheel",
        "adm",
        "shadow",
        "disk",
        "docker",
        "lxd",
        "kmem",
        "systemd-journal",
        "ndimon",
        "ndimon-priv",
        "ndimon-priv-control",
        "ndimon-priv-api",
        "ndimon-control",
    ]
    .contains(&group)
        && (name == "tessera" || !["tessera", "tessera-admin"].contains(&group))
}
pub fn relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 240
        && path.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        })
}
pub fn command_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
}
impl Manifest {
    fn validate_process(&self) -> Result<(), Error> {
        let invalid = |reason: &str| Error::invalid("manifest.process", reason);
        let Some(process) = &self.process else {
            if self.schema_version.is_some_and(|v| v != 1)
                || self.state.is_some()
                || !self.commands.is_empty()
                || !self.events.is_empty()
            {
                return Err(invalid("v2 declarations require process"));
            }
            return Ok(());
        };
        if self.schema_version != Some(2)
            || !relative_path(&process.exec)
            || process.args.len() > 32
            || process
                .args
                .iter()
                .any(|s| s.len() > 1024 || s.chars().any(char::is_control))
        {
            return Err(invalid("invalid process version, executable or arguments"));
        }
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| invalid("state schema required"))?;
        validate_schema(&state.schema).map_err(|e| invalid(&e))?;
        if !state.default.is_object() || state.schema["type"] != "object" {
            return Err(invalid("state must be an object"));
        }
        validate_value(&state.schema, &state.default).map_err(|e| invalid(&e))?;
        if self.commands.len() > 64 {
            return Err(invalid("too many commands"));
        }
        for (name, command) in &self.commands {
            if !command_name(name) {
                return Err(invalid("invalid command name"));
            }
            validate_schema(&command.input).map_err(|e| invalid(&e))?;
            validate_schema(&command.output).map_err(|e| invalid(&e))?;
        }
        if self.events.iter().collect::<BTreeSet<_>>().len() != self.events.len()
            || self
                .events
                .iter()
                .any(|e| !["state_changed", "observed", "drift", "resync"].contains(&e.as_str()))
        {
            return Err(invalid("invalid or duplicate consumed event"));
        }
        Ok(())
    }
}
/// No references, regex, unions or executable extensions. Traversal and schema
/// bytes are bounded before recursive validation of any plugin-controlled data.
pub fn validate_schema(schema: &Value) -> Result<(), String> {
    if serde_json::to_vec(schema).map_err(|e| e.to_string())?.len() > 32 * 1024 {
        return Err("schema exceeds 32 KiB".into());
    }
    fn visit(s: &Value, depth: usize, nodes: &mut usize) -> Result<(), String> {
        *nodes += 1;
        if depth > 16 || *nodes > 1024 {
            return Err("schema complexity limit".into());
        }
        let obj = s.as_object().ok_or("schema must be an object")?;
        let kind = s["type"].as_str().ok_or("schema type required")?;
        if ![
            "object", "array", "string", "boolean", "integer", "number", "null",
        ]
        .contains(&kind)
        {
            return Err("unsupported schema type".into());
        }
        for (key, value) in obj {
            match key.as_str() {
                "type" => (),
                "properties" if kind == "object" => {
                    for child in value
                        .as_object()
                        .ok_or("properties must be an object")?
                        .values()
                    {
                        visit(child, depth + 1, nodes)?;
                    }
                }
                "required" if kind == "object" => {
                    let names = value.as_array().ok_or("required must be an array")?;
                    let mut seen = BTreeSet::new();
                    for n in names {
                        let n = n.as_str().ok_or("required names must be strings")?;
                        if !seen.insert(n) || s["properties"].get(n).is_none() {
                            return Err("unknown or duplicate required property".into());
                        }
                    }
                }
                "additionalProperties" if kind == "object" && value.is_boolean() => (),
                "items" if kind == "array" => visit(value, depth + 1, nodes)?,
                "minLength" | "maxLength" if kind == "string" && value.as_u64().is_some() => (),
                "minItems" | "maxItems" if kind == "array" && value.as_u64().is_some() => (),
                "minProperties" | "maxProperties"
                    if kind == "object" && value.as_u64().is_some() => {}
                "minimum" | "maximum"
                    if ["number", "integer"].contains(&kind) && value.is_number() => {}
                "enum"
                    if value
                        .as_array()
                        .is_some_and(|a| !a.is_empty() && a.len() <= 256) => {}
                _ => return Err(format!("unsupported schema keyword {key}")),
            }
        }
        for (min, max) in [
            ("minLength", "maxLength"),
            ("minItems", "maxItems"),
            ("minProperties", "maxProperties"),
            ("minimum", "maximum"),
        ] {
            if let (Some(a), Some(b)) = (s[min].as_f64(), s[max].as_f64()) {
                if a > b {
                    return Err("inverted schema bounds".into());
                }
            }
        }
        Ok(())
    }
    visit(schema, 0, &mut 0)
}
pub fn validate_value(schema: &Value, value: &Value) -> Result<(), String> {
    if serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > 1024 * 1024 {
        return Err("plugin data exceeds 1 MiB".into());
    }
    fn check(s: &Value, v: &Value, depth: usize) -> Result<(), String> {
        if depth > 32 {
            return Err("plugin data nesting limit".into());
        }
        let kind = s["type"].as_str().ok_or("schema type required")?;
        let valid = match kind {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "boolean" => v.is_boolean(),
            "integer" => v.is_i64() || v.is_u64(),
            "number" => v.is_number(),
            "null" => v.is_null(),
            _ => false,
        };
        if !valid {
            return Err(format!("expected {kind}"));
        }
        if s["enum"].as_array().is_some_and(|a| !a.contains(v)) {
            return Err("value outside enum".into());
        }
        let length = if let Some(obj) = v.as_object() {
            for name in s["required"].as_array().into_iter().flatten() {
                if !obj.contains_key(name.as_str().ok_or("invalid required")?) {
                    return Err("required property missing".into());
                }
            }
            for (key, item) in obj {
                if let Some(child) = s["properties"].get(key) {
                    check(child, item, depth + 1)?;
                } else if s["additionalProperties"] == false {
                    return Err(format!("unknown property {key}"));
                }
            }
            Some((obj.len(), "minProperties", "maxProperties"))
        } else if let Some(a) = v.as_array() {
            if let Some(child) = s.get("items") {
                for item in a {
                    check(child, item, depth + 1)?;
                }
            }
            Some((a.len(), "minItems", "maxItems"))
        } else {
            v.as_str()
                .map(|t| (t.chars().count(), "minLength", "maxLength"))
        };
        if let Some((n, min, max)) = length {
            if s[min].as_u64().is_some_and(|b| (n as u64) < b)
                || s[max].as_u64().is_some_and(|b| n as u64 > b)
            {
                return Err("length outside schema bounds".into());
            }
        }
        if let Some(n) = v.as_f64() {
            if s["minimum"].as_f64().is_some_and(|b| n < b)
                || s["maximum"].as_f64().is_some_and(|b| n > b)
            {
                return Err("number outside schema bounds".into());
            }
        }
        Ok(())
    }
    check(schema, value, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../../tests/fixtures/plugins/hello-process/plugin.json"
        ))
        .unwrap()
    }
    #[test]
    fn archive_names_round_trip_for_every_architecture_and_nothing_else() {
        for arch in ARCHIVE_ARCHITECTURES {
            let file = archive_name("hello", "1.2.0~beta", arch);
            assert_eq!(file, format!("hello-plugin_1.2.0-beta_{arch}.tar.gz"));
            assert_eq!(
                parse_archive_name(&file, arch),
                Some(("hello", "1.2.0-beta"))
            );
            for other in ARCHIVE_ARCHITECTURES.iter().filter(|other| **other != arch) {
                assert_eq!(parse_archive_name(&file, other), None);
            }
        }
        // `any` is no architecture a feed matches.
        assert!(!ARCHIVE_ARCHITECTURES.contains(&"any"));
        for arch in ARCHIVE_ARCHITECTURES {
            assert_eq!(
                parse_archive_name("hello-plugin_1.2.0_any.tar.gz", arch),
                None
            );
        }
        assert_eq!(parse_archive_name("SHA256SUMS", "amd64"), None);
        assert_eq!(
            parse_archive_name("tessera-bundle_0.3.0-1_amd64.tar.gz", "amd64"),
            None
        );
    }
    #[test]
    fn process_manifest_shared_validation_and_schema_limits() {
        let name = PluginName::try_from("hello-process".to_owned()).unwrap();
        let valid = fixture();
        serde_json::from_value::<Manifest>(valid.clone())
            .unwrap()
            .validate(&name, None)
            .unwrap();
        for patch in [
            json!({"schema_version":1}),
            json!({"process":{"exec":"/bin/sh"}}),
            json!({"process":{"exec":"../hello"}}),
            json!({"events":["observed","observed"]}),
            json!({"state":{"schema":{"type":"object","$ref":"https://evil"},"default":{}}}),
            json!({"state":{"schema":{"type":"string"},"default":{}}}),
        ] {
            let mut value = valid.clone();
            for (k, v) in patch.as_object().unwrap() {
                value[k] = v.clone();
            }
            let result = serde_json::from_value::<Manifest>(value)
                .and_then(|m| m.validate(&name, None).map_err(serde::de::Error::custom));
            assert!(result.is_err());
        }
        for schema in [
            json!({"type":"string","pattern":".*"}),
            json!({"type":"array","items":{"$ref":"#"}}),
            json!({"type":"integer","minimum":10,"maximum":1}),
            json!({"type":"object","required":["unknown"]}),
        ] {
            assert!(validate_schema(&schema).is_err());
        }
        let schema = json!({"type":"object","additionalProperties":false,"properties":{"message":{"type":"string","maxLength":3}},"required":["message"]});
        assert!(validate_value(&schema, &json!({"message":"ok"})).is_ok());
        for data in [
            json!({}),
            json!({"message":1}),
            json!({"message":"long"}),
            json!({"message":"ok","other":1}),
        ] {
            assert!(validate_value(&schema, &data).is_err());
        }
    }
    #[test]
    fn duplicate_command_names_are_refused_before_a_map_can_discard_them() {
        let text=serde_json::to_string(&fixture()).unwrap().replace("\"commands\":{","\"commands\":{\"same\":{\"input\":{\"type\":\"object\"},\"output\":{\"type\":\"object\"}},\"same\":{\"input\":{\"type\":\"object\"},\"output\":{\"type\":\"object\"}},");
        assert!(serde_json::from_str::<Manifest>(&text).is_err());
    }
}
