//! Schema-1 plugin hook boundary; the process host supplies JSON bytes, not JS.
use crate::{BoxFuture, StateHandle, persist::StateFile};
use ndimon_types::{Origin, PluginBackupSection};
use serde_json::{Value, json};
use std::fmt::Debug;

pub trait PluginBackup: Debug + Send + Sync {
    fn available<'a>(&'a self, name: &'a str) -> BoxFuture<'a, bool>;
    fn export<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Vec<u8>, String>>;
    /// Validate and import one object; a failure affects only this plugin.
    fn import<'a>(&'a self, name: &'a str, bytes: Vec<u8>) -> BoxFuture<'a, Result<(), String>>;
}
#[derive(Debug)]
pub struct Unavailable;
impl PluginBackup for Unavailable {
    fn available<'a>(&'a self, _: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn export<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async { Err("backup support is unavailable".into()) })
    }
    fn import<'a>(&'a self, _: &'a str, _: Vec<u8>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("restorable after plugin support is available".into()) })
    }
}
/// The production adapter shares PX-1's typed subtree and legacy-hook boundary.
#[derive(Debug)]
pub struct Native {
    handle: StateHandle,
}
impl Native {
    pub fn new(handle: StateHandle) -> Self {
        Self { handle }
    }
}
impl PluginBackup for Native {
    fn available<'a>(&'a self, name: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.handle
                .plugin_backup_available(name)
                .await
                .unwrap_or(false)
        })
    }
    fn export<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            let section = self
                .handle
                .export_plugin_backup_for(name)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_vec(&section.data).map_err(|e| e.to_string())
        })
    }
    fn import<'a>(&'a self, name: &'a str, bytes: Vec<u8>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let state = self.handle.snapshot().await.map_err(|e| e.to_string())?;
            let plugin = state
                .document
                .plugins
                .get(name)
                .ok_or("plugin not installed")?;
            let section = PluginBackupSection::new(
                plugin.version.clone(),
                serde_json::from_slice(&bytes).map_err(|_| "invalid plugin backup data")?,
            );
            self.handle
                .validate_plugin_backup(name, &section)
                .await
                .map_err(|e| e.to_string())?;
            // The coordinator has already accepted the aggregate restore impact.
            self.handle
                .import_plugin_backup(name.into(), section, state.revision, Origin::Migration)
                .await
                .map_err(|e| e.to_string())?;
            Ok(())
        })
    }
}
/// One plugin's export, checked the way a backup checks it.
async fn export_one(host: &dyn PluginBackup, name: &str) -> Result<Value, String> {
    let bytes = host.export(name).await?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "export contains non-JSON values")?;
    if !value.is_object() {
        return Err("export must be a plain object".into());
    }
    if super::legacy::stringify(&value, false)?.len() > 1024 * 1024 {
        return Err("export exceeds 1 MiB".into());
    }
    Ok(value)
}
pub async fn capture(
    host: &dyn PluginBackup,
    state: &StateFile,
    disabled: bool,
    warnings: &mut Vec<String>,
) -> Value {
    let mut out = json!({});
    for (name, plugin) in &state.document.plugins {
        if name == "tessera" || !plugin.installed || !disabled && !plugin.enabled {
            continue;
        }
        if !host.available(name).await {
            warnings.push(format!(
                "plugin {name}: export failed: backup support is unavailable"
            ));
            continue;
        }
        match export_one(host, name).await {
            Ok(value) => out[name] = json!({"version":plugin.version,"data":value}),
            Err(error) => warnings.push(format!("plugin {name}: export failed: {error}")),
        }
    }
    out
}
/// What this device would export for just these plugins, so that a review can
/// say whether a backup's sections differ. Only the exports run (read-only, one
/// per named plugin); nothing is imported, and a plugin that cannot export is
/// left out (the review then counts its section as changed).
pub async fn capture_named(host: &dyn PluginBackup, state: &StateFile, names: &[String]) -> Value {
    let mut out = json!({});
    for name in names {
        let Some(plugin) = state.document.plugins.get(name) else {
            continue;
        };
        if name == "tessera" || !plugin.installed || !host.available(name).await {
            continue;
        }
        if let Ok(value) = export_one(host, name).await {
            out[name] = json!({"version":plugin.version,"data":value});
        }
    }
    out
}
pub async fn apply(host: &dyn PluginBackup, state: &StateFile, sections: &Value) -> Value {
    let mut plugins = json!({});
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    for (name, section) in sections.as_object().into_iter().flatten() {
        if name == "tessera" && super::legacy::native(ndimon_types::Domain::Tessera) {
            continue;
        }
        if state
            .document
            .plugins
            .get(name)
            .is_none_or(|p| !p.installed)
        {
            let warning = format!("plugin {name} not installed; section skipped");
            warnings.push(warning.clone());
            plugins[name] = json!({"ok":true,"skipped":true,"warning":warning});
            continue;
        }
        let result = if host.available(name).await {
            match serde_json::to_vec(&section["data"]) {
                Ok(bytes) => host.import(name, bytes).await,
                Err(_) => Err("invalid plugin backup data".into()),
            }
        } else {
            Err("restorable after plugin support is available".into())
        };
        match result {
            Ok(()) => plugins[name] = json!({"ok":true}),
            Err(error) => {
                errors.push(format!("plugin {name}: {error}"));
                plugins[name] = json!({"ok":false,"error":error});
            }
        }
    }
    let mut out = json!({"ok":errors.is_empty(),"plugins":plugins});
    if !warnings.is_empty() {
        out["warning"] = json!(warnings.join("; "));
    }
    if !errors.is_empty() {
        out["error"] = json!(errors.join("; "));
    }
    out
}
