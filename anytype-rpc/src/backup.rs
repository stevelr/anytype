//! Space backup helpers based on ObjectListExport.
//!
//! Note: when `zip` is true, compression is performed by the Anytype server.
//! This helper does not re-compress backup output locally.

use std::path::PathBuf;

use chrono::Utc;
use prost_types::value::Kind;
use tonic::Request;

use crate::anytype::rpc::object::list_export::Request as ObjectListExportRequest;
use crate::anytype::rpc::object::show::Request as ObjectShowRequest;
use crate::auth::with_token;
use crate::client::AnytypeGrpcClient;
pub use crate::error::BackupError;
pub use crate::model::export::Format as ExportFormat;

/// Options for a space backup request.
#[derive(Debug, Clone)]
pub struct SpaceBackupOptions {
    /// Target space ID.
    pub space_id: String,
    /// Destination folder for backup output.
    pub backup_dir: PathBuf,
    /// Prefix used in generated target name.
    pub filename_prefix: String,
    /// Object IDs to export. Empty means full space export.
    pub object_ids: Vec<String>,
    /// Export format.
    pub format: ExportFormat,
    /// Ask server to produce a zip archive.
    pub zip: bool,
    /// Include linked objects.
    pub include_nested: bool,
    /// Include attached files.
    pub include_files: bool,
    /// For protobuf export, produce JSON payload format.
    pub is_json: bool,
    /// Include archived objects (default false).
    pub include_archived: bool,
    /// Disable export progress events.
    pub no_progress: bool,
    /// Include backlinks.
    pub include_backlinks: bool,
    /// Include space metadata.
    pub include_space: bool,
    /// Include properties frontmatter and schema for markdown export.
    pub md_include_properties_and_schema: bool,
}

impl SpaceBackupOptions {
    /// Creates backup options for a full-space backup with practical defaults.
    pub fn new(space_id: impl Into<String>) -> Self {
        Self {
            space_id: space_id.into(),
            backup_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            filename_prefix: "backup".to_string(),
            object_ids: Vec::new(),
            format: ExportFormat::Protobuf,
            zip: true,
            include_nested: true,
            include_files: true,
            is_json: false,
            include_archived: false,
            no_progress: false,
            include_backlinks: false,
            include_space: false,
            md_include_properties_and_schema: true,
        }
    }
}

/// Result from `backup_space`.
#[derive(Debug, Clone)]
pub struct SpaceBackupResult {
    /// Final local backup path after target naming/relocation.
    pub output_path: PathBuf,
    /// Server-reported export path before local relocation.
    pub server_path: PathBuf,
    /// Number of exported objects reported by the server.
    pub exported: i32,
    /// Generated target filename or directory name.
    pub generated_name: String,
}

impl AnytypeGrpcClient {
    /// Exports a space backup using gRPC `ObjectListExport` and moves the server output to
    /// a deterministic target name: `<prefix>_<space-name>_<timestamp>`.
    pub async fn backup_space(
        &self,
        options: SpaceBackupOptions,
    ) -> Result<SpaceBackupResult, BackupError> {
        if options.space_id.trim().is_empty() {
            return Err(BackupError::InvalidOptions {
                message: "space_id is required".to_string(),
            });
        }

        std::fs::create_dir_all(&options.backup_dir).map_err(|source| BackupError::BackupIo {
            path: options.backup_dir.clone(),
            source,
        })?;

        // Space-name lookup is for output naming only. Export should still succeed
        // if name lookup fails for an otherwise valid space id.
        let space_name = self
            .lookup_space_name(&options.space_id)
            .await
            .unwrap_or_else(|_| options.space_id.clone());

        let mut commands = self.client_commands();
        let request = ObjectListExportRequest {
            space_id: options.space_id.clone(),
            path: options.backup_dir.to_string_lossy().to_string(),
            object_ids: options.object_ids.clone(),
            format: options.format as i32,
            zip: options.zip,
            include_nested: options.include_nested,
            include_files: options.include_files,
            is_json: options.is_json,
            include_archived: options.include_archived,
            no_progress: options.no_progress,
            links_state_filters: None,
            include_backlinks: options.include_backlinks,
            include_space: options.include_space,
            md_include_properties_and_schema: options.md_include_properties_and_schema,
        };
        let request = with_token(Request::new(request), self.token())?;
        let response = commands.object_list_export(request).await?.into_inner();

        if let Some(error) = response.error
            && error.code != 0
        {
            return Err(BackupError::BackupApiResponse {
                code: error.code,
                description: error.description,
            });
        }

        if response.path.trim().is_empty() {
            return Err(BackupError::MissingExportPath);
        }

        let server_path = PathBuf::from(&response.path);
        let source_path = if server_path.is_absolute() {
            server_path.clone()
        } else {
            options.backup_dir.join(server_path.clone())
        };
        let generated_name =
            generated_target_name(&options.filename_prefix, &space_name, options.zip);
        let target_path = options.backup_dir.join(&generated_name);

        if source_path != target_path {
            std::fs::rename(&source_path, &target_path).map_err(|source| {
                BackupError::BackupMove {
                    from: source_path.clone(),
                    to: target_path.clone(),
                    source,
                }
            })?;
        }

        Ok(SpaceBackupResult {
            output_path: target_path,
            server_path,
            exported: response.succeed,
            generated_name,
        })
    }

    async fn lookup_space_name(&self, space_id: &str) -> Result<String, BackupError> {
        let mut commands = self.client_commands();
        let request = ObjectShowRequest {
            object_id: space_id.to_string(),
            space_id: space_id.to_string(),
            include_relations_as_dependent_objects: false,
            ..Default::default()
        };
        let request = with_token(Request::new(request), self.token())?;
        let response = commands.object_show(request).await?.into_inner();

        if let Some(error) = response.error
            && error.code != 0
        {
            return Err(BackupError::SpaceNameLookup {
                space_id: space_id.to_string(),
                message: format!(
                    "ObjectShow failed: {} (code {})",
                    error.description, error.code
                ),
            });
        }

        let object_view = response
            .object_view
            .ok_or_else(|| BackupError::SpaceNameLookup {
                space_id: space_id.to_string(),
                message: "missing object_view".to_string(),
            })?;

        let name = object_view
            .details
            .iter()
            .filter_map(|set| set.details.as_ref())
            .find_map(|details| {
                details
                    .fields
                    .get("name")
                    .and_then(|value| match &value.kind {
                        Some(Kind::StringValue(name)) if !name.trim().is_empty() => {
                            Some(name.trim().to_string())
                        }
                        _ => None,
                    })
            })
            .ok_or_else(|| BackupError::SpaceNameLookup {
                space_id: space_id.to_string(),
                message: "space object has no non-empty name".to_string(),
            })?;

        Ok(name)
    }
}

fn generated_target_name(prefix: &str, space_name: &str, zip: bool) -> String {
    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    let prefix = sanitize_path_component(prefix);
    let space_name = sanitize_path_component(space_name);
    let base = if prefix.is_empty() {
        format!("{space_name}_{ts}")
    } else {
        format!("{prefix}_{space_name}_{ts}")
    };
    if zip { format!("{base}.zip") } else { base }
}

fn sanitize_path_component(input: &str) -> String {
    const SEP: char = '_';
    let mut out = String::with_capacity(input.len());
    let mut prev_sep = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_sep = false;
        } else if !prev_sep {
            out.push(SEP);
            prev_sep = true;
        }
    }
    let trimmed = out.trim_matches(SEP).to_string();
    if trimmed.is_empty() {
        "space".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_component() {
        assert_eq!(sanitize_path_component("My Space"), "my_space");
        assert_eq!(sanitize_path_component("  $$$ "), "space");
        assert_eq!(sanitize_path_component("a/b\\c"), "a_b_c");
    }

    #[test]
    fn target_name_has_zip_when_requested() {
        let name = generated_target_name("backup", "My Space", true);
        assert!(name.starts_with("backup_my_space_"));
        assert!(name.ends_with(".zip"));
    }
}
