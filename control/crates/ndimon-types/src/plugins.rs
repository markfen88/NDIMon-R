//! Plugin subtree backup contract. Package metadata is never restored by schema 1.
use std::collections::BTreeMap;
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct PluginBackupSection {
    pub version: String,
    #[cfg_attr(feature = "ts", ts(type = "{ [key: string]: unknown }"))]
    pub data: BTreeMap<String, serde_json::Value>,
}
impl PluginBackupSection {
    pub fn new(version: String, data: BTreeMap<String, serde_json::Value>) -> Self {
        Self { version, data }
    }
}
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct PluginBackupExport {
    pub plugins: BTreeMap<String, PluginBackupSection>,
    pub warnings: Vec<String>,
}
