//! Typed schema-1 calls; package and enabled/version intent are never restored.
use super::*;
const LEGACY_RESTORE: &str = "restorable after plugin support is available";
impl crate::StateHandle {
    pub async fn plugin_backup_available(&self, name: &str) -> Result<bool, ApiError> {
        let state = self.snapshot().await?;
        if name == "tessera"
            || state
                .document
                .plugins
                .get(name)
                .is_none_or(|p| !p.installed)
        {
            return Ok(false);
        }
        let entry = self
            .applier
            .plugin_query(&state, json!({"operation":"manifest","name":name}))
            .await
            .map_err(internal)?;
        let manifest = manifest(&entry)?;
        // Manifest-v1 hooks ran in the removed JavaScript loader.
        Ok(manifest.process.is_some() && manifest.state.is_some())
    }
    pub async fn export_plugin_backup(&self) -> Result<PluginBackupExport, ApiError> {
        let state = self.snapshot().await?;
        let mut export = PluginBackupExport::default();
        for (name, plugin) in &state.document.plugins {
            if !plugin.installed || !plugin.enabled || name == "tessera" {
                continue;
            }
            match self.plugin_backup_section(&state, name).await {
                Ok(section) => {
                    export.plugins.insert(name.clone(), section);
                }
                Err(error) => export.warnings.push(format!("plugin {name}: {error}")),
            }
        }
        Ok(export)
    }
    /// Named capture also serves disabled plugins in private undo snapshots.
    pub async fn export_plugin_backup_for(
        &self,
        name: &str,
    ) -> Result<PluginBackupSection, ApiError> {
        self.plugin_backup_section(&self.snapshot().await?, name)
            .await
    }
    async fn plugin_backup_section(
        &self,
        state: &StateFile,
        name: &str,
    ) -> Result<PluginBackupSection, ApiError> {
        let plugin = state
            .document
            .plugins
            .get(name)
            .filter(|p| p.installed && name != "tessera")
            .ok_or_else(|| invalid("plugin not installed"))?;
        let entry = self
            .applier
            .plugin_query(state, json!({"operation":"manifest","name":name}))
            .await
            .map_err(internal)?;
        let manifest = manifest(&entry)?;
        if manifest.process.is_none() {
            return Err(invalid("backup support is unavailable"));
        }
        let data = plugin.state.as_ref().map_or_else(
            || entry["manifest"]["state"]["default"].clone(),
            |data| json!(data),
        );
        validate(&manifest, &data).map_err(invalid)?;
        check_data(&data).map_err(invalid)?;
        Ok(PluginBackupSection::new(
            plugin.version.clone(),
            serde_json::from_value(data).map_err(invalid)?,
        ))
    }
    pub async fn validate_plugin_backup(
        &self,
        name: &str,
        section: &PluginBackupSection,
    ) -> Result<(), ApiError> {
        let state = self.snapshot().await?;
        if name == "tessera"
            || state
                .document
                .plugins
                .get(name)
                .is_none_or(|p| !p.installed)
        {
            return Err(invalid("plugin not installed"));
        }
        let data = json!(section.data);
        check_data(&data).map_err(invalid)?;
        let entry = self
            .applier
            .plugin_query(&state, json!({"operation":"manifest","name":name}))
            .await
            .map_err(internal)?;
        let manifest = manifest(&entry)?;
        if manifest.process.is_none() {
            return Err(invalid(LEGACY_RESTORE));
        }
        validate(&manifest, &data).map_err(invalid)
    }
    pub async fn import_plugin_backup(
        &self,
        name: String,
        section: PluginBackupSection,
        expect_revision: u64,
        origin: Origin,
    ) -> Result<crate::Submission, ApiError> {
        self.validate_plugin_backup(&name, &section).await?;
        let state = self.snapshot().await?;
        if state.revision != expect_revision {
            return Err(ApiError::RevisionConflict {
                current_revision: state.revision,
            });
        }
        if state.plugin_job.is_some() {
            return Err(ApiError::StillApplying);
        }
        let mut request = CommandRequest::new(Command::SetPluginData {
            name,
            state: section.data,
        });
        request.expect_revision = Some(expect_revision);
        self.submit_with_result(request, origin).await
    }
}
fn manifest(entry: &Value) -> Result<ndimon_privd_proto::plugin::Manifest, ApiError> {
    serde_json::from_value(entry["manifest"].clone()).map_err(invalid)
}
fn validate(manifest: &ndimon_privd_proto::plugin::Manifest, data: &Value) -> Result<(), String> {
    ndimon_privd_proto::plugin::validate_value(
        &manifest
            .state
            .as_ref()
            .ok_or("missing plugin state schema")?
            .schema,
        data,
    )
}
fn check_data(data: &Value) -> Result<(), String> {
    if !data.is_object() {
        return Err("export must be a plain object".into());
    }
    if crate::backup::legacy::stringify(data, false)?.len() > 1024 * 1024 {
        return Err("export exceeds 1 MiB".into());
    }
    Ok(())
}
