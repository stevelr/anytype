// any-mcp - bounded, workflow-oriented MCP server for Anytype
// SPDX-License-Identifier: Apache-2.0

//! Multi-transport real-server acceptance harness for the artifact data plane.
//!
//! The harness owns three concerns that every artifact acceptance scenario
//! shares, so scenario families stay small and comparable:
//!
//! 1. a closed transport matrix (control plane x data plane) whose complete
//!    coverage is proven by an offline inventory test rather than by hand,
//! 2. fixture discipline: an operator-shaped strict policy fixture on private
//!    temporary directories, immediate resource registration, exact teardown,
//!    and rejection of skipped disposable admission,
//! 3. content-free evidence: exact catalog/schema snapshots plus byte hashes
//!    that are compared for parity across every executed transport.
//!
//! Nothing here retains artifact bytes, credentials, staging bearers, or raw
//! server log lines; every reported value is a hash, a count, or a fixed
//! category name.
#![allow(dead_code)] // Shared support: each consuming target executes a subset.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anytype::test_util::{DisposableRun, TestContext, unique_suffix};
use reqwest::header::{AUTHORIZATION, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::McpDriver;

/// Exact sorted production artifact tool inventory.
pub const ARTIFACT_TOOL_NAMES: [&str; 8] = [
    "artifact_release",
    "artifact_stage_upload",
    "artifact_status",
    "document_export",
    "document_import_create",
    "document_import_update",
    "file_export",
    "file_import",
];

/// Exact bytes imported and exported by every smoke scenario.
pub const ARTIFACT_FILE_PAYLOAD: &[u8] = b"artifact-file-payload";
/// Exact Markdown source used to create the smoke document.
pub const ARTIFACT_CREATE_MARKDOWN: &str = "# Artifact create\n";
/// Exact Markdown source used to update the smoke document.
pub const ARTIFACT_UPDATE_MARKDOWN: &str = "# Artifact update\n";
/// Fixed canonical MIME essence asserted for the smoke file.
pub const ARTIFACT_FILE_MEDIA_TYPE: &str = "application/octet-stream";
/// Fixed canonical MIME essence asserted for staged Markdown uploads.
pub const ARTIFACT_MARKDOWN_MEDIA_TYPE: &str = "text/markdown";

/// Control plane through which acceptance scenarios reach the artifact tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactControlPlane {
    /// Exact JSON-RPC frames exchanged with a spawned production child.
    ScriptedProtocol,
    /// In-process production router dispatch without any transport.
    DirectRouter,
    /// Spawned production stdio child on the stable protocol revision.
    SpawnedStableStdio,
    /// Spawned production stdio child on the preview protocol revision.
    SpawnedPreviewStdio,
}

impl ArtifactControlPlane {
    /// Complete closed control-plane inventory.
    pub const ALL: [Self; 4] = [
        Self::ScriptedProtocol,
        Self::DirectRouter,
        Self::SpawnedStableStdio,
        Self::SpawnedPreviewStdio,
    ];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ScriptedProtocol => "scripted_protocol",
            Self::DirectRouter => "direct_router",
            Self::SpawnedStableStdio => "spawned_stable_stdio",
            Self::SpawnedPreviewStdio => "spawned_preview_stdio",
        }
    }

    /// Advertised MCP protocol revision for this control plane.
    #[must_use]
    pub const fn protocol_version(self) -> &'static str {
        match self {
            Self::SpawnedPreviewStdio => "2026-07-28",
            Self::ScriptedProtocol | Self::DirectRouter | Self::SpawnedStableStdio => "2025-11-25",
        }
    }

    /// Whether the control plane runs in a separate production process.
    #[must_use]
    pub const fn is_spawned(self) -> bool {
        matches!(
            self,
            Self::ScriptedProtocol | Self::SpawnedStableStdio | Self::SpawnedPreviewStdio
        )
    }

    /// Parses an exact stable identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }
}

/// Byte path taken by imported and exported artifact payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactDataPlane {
    /// Bytes move through authorized local import/export roots.
    LocalRoots,
    /// Bytes move through the remote HTTP staging service.
    RemoteStaging,
}

impl ArtifactDataPlane {
    /// Complete closed data-plane inventory.
    pub const ALL: [Self; 2] = [Self::LocalRoots, Self::RemoteStaging];

    /// Stable identifier used in evidence and failure reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalRoots => "local_roots",
            Self::RemoteStaging => "remote_staging",
        }
    }

    /// Parses an exact stable identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }
}

/// One acceptance transport: a control plane paired with a data plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactTransport {
    control: ArtifactControlPlane,
    data: ArtifactDataPlane,
}

impl ArtifactTransport {
    /// Complete closed acceptance matrix.
    pub const ALL: [Self; 8] = [
        Self::new(
            ArtifactControlPlane::ScriptedProtocol,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::ScriptedProtocol,
            ArtifactDataPlane::RemoteStaging,
        ),
        Self::new(
            ArtifactControlPlane::DirectRouter,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::DirectRouter,
            ArtifactDataPlane::RemoteStaging,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedStableStdio,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedStableStdio,
            ArtifactDataPlane::RemoteStaging,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedPreviewStdio,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedPreviewStdio,
            ArtifactDataPlane::RemoteStaging,
        ),
    ];

    /// Transports executed by the in-crate direct-router acceptance target.
    pub const DIRECT_MATRIX: [Self; 2] = [
        Self::new(
            ArtifactControlPlane::DirectRouter,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::DirectRouter,
            ArtifactDataPlane::RemoteStaging,
        ),
    ];

    /// Transports executed by the spawned production-process acceptance target.
    pub const SPAWNED_MATRIX: [Self; 6] = [
        Self::new(
            ArtifactControlPlane::ScriptedProtocol,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::ScriptedProtocol,
            ArtifactDataPlane::RemoteStaging,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedStableStdio,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedStableStdio,
            ArtifactDataPlane::RemoteStaging,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedPreviewStdio,
            ArtifactDataPlane::LocalRoots,
        ),
        Self::new(
            ArtifactControlPlane::SpawnedPreviewStdio,
            ArtifactDataPlane::RemoteStaging,
        ),
    ];

    /// Pairs one control plane with one data plane.
    #[must_use]
    pub const fn new(control: ArtifactControlPlane, data: ArtifactDataPlane) -> Self {
        Self { control, data }
    }

    /// Selected control plane.
    #[must_use]
    pub const fn control(self) -> ArtifactControlPlane {
        self.control
    }

    /// Selected data plane.
    #[must_use]
    pub const fn data(self) -> ArtifactDataPlane {
        self.data
    }

    /// Stable `<control>+<data>` identifier used in evidence.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match (self.control, self.data) {
            (ArtifactControlPlane::ScriptedProtocol, ArtifactDataPlane::LocalRoots) => {
                "scripted_protocol+local_roots"
            }
            (ArtifactControlPlane::ScriptedProtocol, ArtifactDataPlane::RemoteStaging) => {
                "scripted_protocol+remote_staging"
            }
            (ArtifactControlPlane::DirectRouter, ArtifactDataPlane::LocalRoots) => {
                "direct_router+local_roots"
            }
            (ArtifactControlPlane::DirectRouter, ArtifactDataPlane::RemoteStaging) => {
                "direct_router+remote_staging"
            }
            (ArtifactControlPlane::SpawnedStableStdio, ArtifactDataPlane::LocalRoots) => {
                "spawned_stable_stdio+local_roots"
            }
            (ArtifactControlPlane::SpawnedStableStdio, ArtifactDataPlane::RemoteStaging) => {
                "spawned_stable_stdio+remote_staging"
            }
            (ArtifactControlPlane::SpawnedPreviewStdio, ArtifactDataPlane::LocalRoots) => {
                "spawned_preview_stdio+local_roots"
            }
            (ArtifactControlPlane::SpawnedPreviewStdio, ArtifactDataPlane::RemoteStaging) => {
                "spawned_preview_stdio+remote_staging"
            }
        }
    }

    /// Parses an exact stable `<control>+<data>` identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.id() == value)
    }
}

/// Reviewed exact snapshot of the complete artifact tool catalog.
///
/// The fixture is regenerated only by the ignored updater documented beside it
/// in `tests/snapshots/README.md`; ordinary runs compare against it.
pub const REVIEWED_ARTIFACT_CATALOG: &str = include_str!("../snapshots/artifact-catalog.snap");

/// Exact catalog and schema snapshot of the advertised artifact tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactCatalogSnapshot {
    tools: BTreeMap<String, String>,
}

impl ArtifactCatalogSnapshot {
    /// Builds the snapshot from complete `tools/list` descriptors.
    ///
    /// Only the closed artifact inventory is retained. Each entry hashes the
    /// canonical (recursively key-sorted) descriptor, so an added field, a
    /// changed description, or a relaxed schema bound all diverge.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when a descriptor is malformed or when the
    /// advertised artifact inventory is not exactly [`ARTIFACT_TOOL_NAMES`].
    pub fn from_descriptors(descriptors: &[Value]) -> Result<Self, String> {
        let mut tools = BTreeMap::new();
        for descriptor in descriptors {
            let name = descriptor
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "tools/list descriptor omitted its name".to_owned())?;
            if !ARTIFACT_TOOL_NAMES.contains(&name) {
                continue;
            }
            if tools
                .insert(name.to_owned(), canonical_digest(descriptor))
                .is_some()
            {
                return Err(format!("duplicate artifact tool descriptor: {name}"));
            }
        }
        let advertised = tools.keys().map(String::as_str).collect::<Vec<_>>();
        if advertised != ARTIFACT_TOOL_NAMES {
            return Err("advertised artifact catalog is not the exact inventory".to_owned());
        }
        Ok(Self { tools })
    }

    /// Builds the snapshot from the reviewed committed fixture.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when the fixture is malformed or no longer
    /// contains the exact artifact inventory.
    pub fn reviewed() -> Result<Self, String> {
        let value: Value = serde_json::from_str(REVIEWED_ARTIFACT_CATALOG)
            .map_err(|_| "reviewed artifact catalog fixture is malformed".to_owned())?;
        let tools = value
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| "reviewed artifact catalog fixture omitted its tools".to_owned())?;
        Self::from_descriptors(tools)
    }

    /// Exact digest over the complete artifact catalog.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        for (name, digest) in &self.tools {
            hasher.update(name.as_bytes());
            hasher.update(b"\0");
            hasher.update(digest.as_bytes());
            hasher.update(b"\n");
        }
        hex_digest(&hasher.finalize())
    }

    /// Per-tool canonical descriptor digests, sorted by tool name.
    #[must_use]
    pub fn tool_digests(&self) -> &BTreeMap<String, String> {
        &self.tools
    }

    /// Compares two snapshots and names the first divergent tool.
    ///
    /// # Errors
    ///
    /// Returns a fixed message naming only the divergent tool; no schema
    /// fragment is retained in the report.
    pub fn compare(&self, other: &Self) -> Result<(), String> {
        for (name, digest) in &self.tools {
            match other.tools.get(name) {
                None => return Err(format!("artifact catalog omitted tool: {name}")),
                Some(candidate) if candidate != digest => {
                    return Err(format!("artifact tool contract diverged: {name}"));
                }
                Some(_) => {}
            }
        }
        if other.tools.len() == self.tools.len() {
            Ok(())
        } else {
            Err("artifact catalog advertised an unexpected tool".to_owned())
        }
    }
}

/// Recursively sorts object keys and preserves array order.
fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, nested)| (key.clone(), canonical_value(nested)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(canonical_value).collect()),
        other => other.clone(),
    }
}

fn canonical_digest(value: &Value) -> String {
    let canonical = canonical_value(value);
    let encoded =
        serde_json::to_string(&canonical).unwrap_or_else(|_| String::from("<unencodable>"));
    hex_digest(&Sha256::digest(encoded.as_bytes()))
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut encoded, byte| {
        encoded.push_str(&format!("{byte:02x}"));
        encoded
    })
}

/// Lowercase SHA-256 of exact bytes.
#[must_use]
pub fn artifact_sha256(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

/// Strict artifact policy options shared by every acceptance fixture.
#[derive(Debug, Clone, Copy)]
pub struct ArtifactPolicyOptions {
    /// Whether the remote HTTP staging service is enabled.
    pub staging: bool,
    /// Whether the server runs in read-only mode.
    pub read_only: bool,
}

impl Default for ArtifactPolicyOptions {
    fn default() -> Self {
        Self {
            staging: true,
            read_only: false,
        }
    }
}

/// Private temporary operator policy, roots, and seeded import sources.
///
/// Dropping the fixture removes the complete tree, so no acceptance byte
/// survives a scenario.
#[derive(Debug)]
pub struct ArtifactPolicyFixture {
    base: PathBuf,
    config: PathBuf,
    import: PathBuf,
    export: PathBuf,
    staging_base_url: Option<String>,
    options: ArtifactPolicyOptions,
}

impl ArtifactPolicyFixture {
    /// Logical import root identifier declared by the fixture policy.
    pub const IMPORT_ROOT: &'static str = "inbox";
    /// Logical export root identifier declared by the fixture policy.
    pub const EXPORT_ROOT: &'static str = "outbox";
    /// Relative path of the seeded binary import source.
    pub const FILE_SOURCE: &'static str = "file.bin";
    /// Relative path of the seeded document-create source.
    pub const CREATE_SOURCE: &'static str = "create.md";
    /// Relative path of the seeded document-update source.
    pub const UPDATE_SOURCE: &'static str = "update.md";

    /// Creates the default read-write fixture with staging enabled.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when a directory, source, or policy file cannot
    /// be created with private permissions.
    pub fn create(space_id: &str) -> Result<Self, String> {
        Self::create_with(space_id, ArtifactPolicyOptions::default())
    }

    /// Creates a fixture with explicit staging and read-only policy.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when a directory, source, or policy file cannot
    /// be created with private permissions.
    pub fn create_with(space_id: &str, options: ArtifactPolicyOptions) -> Result<Self, String> {
        if space_id.is_empty() || space_id.len() > 512 {
            return Err("artifact fixture requires an exact space identity".to_owned());
        }
        let base =
            std::env::temp_dir().join(format!("any-mcp-artifact-harness-{}", unique_suffix()));
        let import = base.join("import");
        let export = base.join("export");
        let staging = base.join("staging");
        fs::create_dir(&base)
            .and_then(|()| fs::create_dir(&import))
            .and_then(|()| fs::create_dir(&export))
            .and_then(|()| fs::create_dir(&staging))
            .map_err(|_| "create artifact acceptance directories".to_owned())?;
        secure_directories(&[&base, &import, &export, &staging])?;

        fs::write(import.join(Self::FILE_SOURCE), ARTIFACT_FILE_PAYLOAD)
            .and_then(|()| fs::write(import.join(Self::CREATE_SOURCE), ARTIFACT_CREATE_MARKDOWN))
            .and_then(|()| fs::write(import.join(Self::UPDATE_SOURCE), ARTIFACT_UPDATE_MARKDOWN))
            .map_err(|_| "write artifact acceptance sources".to_owned())?;
        secure_files(&[
            import.join(Self::FILE_SOURCE),
            import.join(Self::CREATE_SOURCE),
            import.join(Self::UPDATE_SOURCE),
        ])?;

        let staging_base_url = if options.staging {
            let port = reserve_loopback_port()?;
            Some(format!("http://127.0.0.1:{port}/artifacts/v1/"))
        } else {
            None
        };
        let config = base.join("policy.toml");
        fs::write(
            &config,
            render_policy(
                space_id,
                &import,
                &export,
                &staging,
                staging_base_url.as_deref(),
                options,
            ),
        )
        .map_err(|_| "write artifact acceptance policy".to_owned())?;
        secure_files(std::slice::from_ref(&config))?;

        Ok(Self {
            base,
            config,
            import,
            export,
            staging_base_url,
            options,
        })
    }

    /// Path of the strict operator policy passed through `ANY_MCP_CONFIG`.
    #[must_use]
    pub fn config_path(&self) -> &Path {
        &self.config
    }

    /// Physical directory backing the logical import root.
    #[must_use]
    pub fn import_root(&self) -> &Path {
        &self.import
    }

    /// Physical directory backing the logical export root.
    #[must_use]
    pub fn export_root(&self) -> &Path {
        &self.export
    }

    /// Configured staging base URL, when staging is enabled.
    #[must_use]
    pub fn staging_base_url(&self) -> Option<&str> {
        self.staging_base_url.as_deref()
    }

    /// Selected policy options.
    #[must_use]
    pub const fn options(&self) -> ArtifactPolicyOptions {
        self.options
    }

    /// Reads the complete strict policy contents.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when the policy cannot be read.
    pub fn policy_contents(&self) -> Result<String, String> {
        fs::read_to_string(&self.config).map_err(|_| "read artifact acceptance policy".to_owned())
    }

    /// Reads exact bytes published under the export root.
    ///
    /// # Errors
    ///
    /// Returns a fixed message when the relative path is unsafe or unreadable.
    pub fn read_export(&self, relative: &str) -> Result<Vec<u8>, String> {
        if relative.is_empty() || relative.contains("..") || relative.contains('/') {
            return Err("export fixture path must be a simple file name".to_owned());
        }
        fs::read(self.export.join(relative)).map_err(|_| "read artifact export".to_owned())
    }

    /// Whether an export artifact exists under the export root.
    #[must_use]
    pub fn export_exists(&self, relative: &str) -> bool {
        !relative.is_empty()
            && !relative.contains("..")
            && !relative.contains('/')
            && self.export.join(relative).is_file()
    }
}

impl Drop for ArtifactPolicyFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn render_policy(
    space_id: &str,
    import: &Path,
    export: &Path,
    staging: &Path,
    staging_base_url: Option<&str>,
    options: ArtifactPolicyOptions,
) -> String {
    let mut contents = format!(
        "schema_version = 1\n\
         [spaces]\n\
         read_only = {}\n\
         allowed = [{{ id = \"{}\" }}]\n\
         [[roots.import]]\n\
         id = \"{}\"\n\
         path = \"{}\"\n\
         [[roots.export]]\n\
         id = \"{}\"\n\
         path = \"{}\"\n",
        options.read_only,
        toml_basic_string(space_id),
        toml_basic_string(ArtifactPolicyFixture::IMPORT_ROOT),
        toml_basic_string(&import.display().to_string()),
        toml_basic_string(ArtifactPolicyFixture::EXPORT_ROOT),
        toml_basic_string(&export.display().to_string()),
    );
    if let Some(base_url) = staging_base_url {
        let bind = base_url
            .strip_prefix("http://")
            .and_then(|rest| rest.split('/').next())
            .unwrap_or("127.0.0.1:0");
        contents.push_str(&format!(
            "[staging]\n\
             enabled = true\n\
             root = \"{}\"\n\
             bind = \"{}\"\n\
             public_base_url = \"{}\"\n",
            toml_basic_string(&staging.display().to_string()),
            toml_basic_string(bind),
            toml_basic_string(base_url),
        ));
    }
    contents
}

/// Escapes one value for a TOML basic string.
///
/// Windows fixture paths contain backslashes, which are escape introducers in a
/// TOML basic string; emitting them verbatim produces an unparsable policy file.
fn toml_basic_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control.is_control() => {
                escaped.push_str(&format!("\\u{:04X}", u32::from(control)));
            }
            other => escaped.push(other),
        }
    }
    escaped
}

fn reserve_loopback_port() -> Result<u16, String> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|_| "reserve artifact acceptance port".to_owned())
}

#[cfg(unix)]
fn secure_directories(directories: &[&PathBuf]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    for directory in directories {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|_| "secure artifact acceptance directory".to_owned())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_directories(_directories: &[&PathBuf]) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn secure_files(files: &[PathBuf]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    for file in files {
        fs::set_permissions(file, fs::Permissions::from_mode(0o600))
            .map_err(|_| "secure artifact acceptance file".to_owned())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_files(_files: &[PathBuf]) -> Result<(), String> {
    Ok(())
}

/// Content-free result of one transport's artifact smoke scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSmokeEvidence {
    /// Stable transport identifier that produced this evidence.
    pub transport: &'static str,
    /// Exact advertised artifact catalog and schema snapshot.
    pub catalog: ArtifactCatalogSnapshot,
    /// Authorized import roots reported by `artifact_status`.
    pub import_root_count: u64,
    /// Authorized export roots reported by `artifact_status`.
    pub export_root_count: u64,
    /// Whether the staging service reported itself active.
    pub staging_active: bool,
    /// Verified imported and exported file byte length.
    pub file_bytes: u64,
    /// Verified SHA-256 of the round-tripped file bytes.
    pub file_sha256: String,
    /// Canonical Markdown hash proven after document creation.
    pub created_document_sha256: String,
    /// Canonical Markdown hash proven after the exported readback.
    pub exported_document_sha256: String,
    /// Canonical Markdown hash proven after the document update.
    pub updated_document_sha256: String,
    /// Whether an explicitly allocated staging record was released.
    pub stage_released: bool,
}

/// Transport-independent projection compared across the acceptance matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactParityKey {
    catalog_digest: String,
    import_root_count: u64,
    export_root_count: u64,
    staging_active: bool,
    file_bytes: u64,
    file_sha256: String,
    created_document_sha256: String,
    exported_document_sha256: String,
    updated_document_sha256: String,
    stage_released: bool,
}

impl ArtifactSmokeEvidence {
    /// Projection that every transport must reproduce exactly.
    #[must_use]
    pub fn parity_key(&self) -> ArtifactParityKey {
        ArtifactParityKey {
            catalog_digest: self.catalog.digest(),
            import_root_count: self.import_root_count,
            export_root_count: self.export_root_count,
            staging_active: self.staging_active,
            file_bytes: self.file_bytes,
            file_sha256: self.file_sha256.clone(),
            created_document_sha256: self.created_document_sha256.clone(),
            exported_document_sha256: self.exported_document_sha256.clone(),
            updated_document_sha256: self.updated_document_sha256.clone(),
            stage_released: self.stage_released,
        }
    }
}

/// Proves that every executed transport observed the same artifact behavior.
///
/// # Errors
///
/// Returns a fixed message naming the first divergent transport, or reporting
/// an incomplete or duplicated executed matrix.
pub fn assert_artifact_parity(
    executed: &[ArtifactSmokeEvidence],
    expected: &[ArtifactTransport],
) -> Result<(), String> {
    if executed.len() != expected.len() {
        return Err("executed artifact transport matrix is incomplete".to_owned());
    }
    let mut observed = Vec::with_capacity(executed.len());
    for (evidence, transport) in executed.iter().zip(expected) {
        if evidence.transport != transport.id() {
            return Err(format!(
                "artifact transport evidence out of order: {}",
                evidence.transport
            ));
        }
        if observed.contains(&evidence.transport) {
            return Err(format!(
                "duplicate artifact transport evidence: {}",
                evidence.transport
            ));
        }
        observed.push(evidence.transport);
    }
    let Some(baseline) = executed.first() else {
        return Err("artifact parity requires at least one executed transport".to_owned());
    };
    let baseline_key = baseline.parity_key();
    for evidence in executed.iter().skip(1) {
        baseline.catalog.compare(&evidence.catalog)?;
        if evidence.parity_key() != baseline_key {
            return Err(format!(
                "artifact transport diverged from {}: {}",
                baseline.transport, evidence.transport
            ));
        }
    }
    Ok(())
}

/// Fixture inputs owned by the caller rather than by a transport driver.
pub struct ArtifactSmokeFixture<'a> {
    /// Transport under test.
    pub transport: ArtifactTransport,
    /// Strict operator policy backing the server under test.
    pub policy: &'a ArtifactPolicyFixture,
    /// Disposable space context that owns every created resource.
    pub ctx: &'a TestContext,
}

/// Runs the complete artifact smoke scenario through one transport.
///
/// The scenario imports and exports a file, creates, exports, and updates a
/// document, and allocates then releases an explicit staging record. Every
/// created Anytype resource is registered with the disposable context before
/// the next step, so a mid-scenario failure still tears down exactly.
///
/// # Errors
///
/// Returns a fixed message describing the first failed stage; no artifact
/// bytes, staging bearer, or upstream body is retained.
pub async fn run_artifact_smoke_scenario(
    driver: &mut impl McpDriver,
    fixture: &ArtifactSmokeFixture<'_>,
) -> Result<ArtifactSmokeEvidence, String> {
    let transport = fixture.transport;
    let space_id = fixture.ctx.space_id.as_str();
    let suffix = unique_suffix();

    let descriptors = driver.list_tool_descriptors().await?;
    let catalog = ArtifactCatalogSnapshot::from_descriptors(&descriptors)?;
    ArtifactCatalogSnapshot::reviewed()?.compare(&catalog)?;

    let status = driver.call_tool("artifact_status", json!({})).await?;
    let import_root_count = required_u64(&status, "import_root_count")?;
    let export_root_count = required_u64(&status, "export_root_count")?;
    let staging_configured = status["staging_configured"] == Value::Bool(true);
    let staging_active = status["staging_active"] == Value::Bool(true);
    if import_root_count != 1 || export_root_count != 1 {
        return Err("artifact status did not report the fixture roots".to_owned());
    }
    if staging_configured != fixture.policy.options().staging
        || staging_active != fixture.policy.options().staging
    {
        return Err("artifact status did not report the configured staging service".to_owned());
    }
    if transport.data() == ArtifactDataPlane::RemoteStaging && !staging_active {
        return Err("remote staging transport requires an active staging service".to_owned());
    }

    let file_source = artifact_source(
        driver,
        &transport,
        space_id,
        ARTIFACT_FILE_PAYLOAD,
        ARTIFACT_FILE_MEDIA_TYPE,
        ArtifactPolicyFixture::FILE_SOURCE,
    )
    .await?;
    let imported = driver
        .call_tool(
            "file_import",
            json!({
                "space": space_id,
                "source": file_source,
                "name": format!("artifact-{suffix}.bin"),
                "media_type": ARTIFACT_FILE_MEDIA_TYPE,
                "idempotency_key": format!("artifact-file-import-{suffix}")
            }),
        )
        .await?;
    let file_id = required_str(&imported, "/file_id")?;
    fixture.ctx.register_file(&file_id);
    let file_sha256 = required_str(&imported, "/receipt/sha256")?;
    let file_bytes = imported
        .pointer("/receipt/size_bytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| "file import omitted its verified size".to_owned())?;
    if file_sha256 != artifact_sha256(ARTIFACT_FILE_PAYLOAD)
        || file_bytes != ARTIFACT_FILE_PAYLOAD.len() as u64
    {
        return Err("file import did not verify the exact fixture bytes".to_owned());
    }

    let export_name = format!("file-export-{suffix}.bin");
    let exported = driver
        .call_tool(
            "file_export",
            json!({
                "space": space_id,
                "file_id": file_id,
                "destination": artifact_destination(&transport, &export_name),
                "idempotency_key": format!("artifact-file-export-{suffix}")
            }),
        )
        .await?;
    let exported_bytes =
        read_exported_bytes(&transport, fixture.policy, &exported, &export_name).await?;
    if exported_bytes != ARTIFACT_FILE_PAYLOAD
        || required_str(&exported, "/receipt/sha256")? != file_sha256
    {
        return Err("file export did not republish the exact imported bytes".to_owned());
    }
    release_remote_receipt(driver, &transport, &exported).await?;

    let create_source = artifact_source(
        driver,
        &transport,
        space_id,
        ARTIFACT_CREATE_MARKDOWN.as_bytes(),
        ARTIFACT_MARKDOWN_MEDIA_TYPE,
        ArtifactPolicyFixture::CREATE_SOURCE,
    )
    .await?;
    let created = driver
        .call_tool(
            "document_import_create",
            json!({
                "space": space_id,
                "source": create_source,
                "source_format": "markdown",
                "object_type": "page",
                "name": format!("Artifact document {suffix}"),
                "idempotency_key": format!("artifact-document-create-{suffix}")
            }),
        )
        .await?;
    let object_id = required_str(&created, "/object_id")?;
    fixture.ctx.register_object(&object_id);
    let created_document_sha256 = required_str(&created, "/canonical_sha256")?;
    if required_str(&created, "/source_sha256")?
        != artifact_sha256(ARTIFACT_CREATE_MARKDOWN.as_bytes())
    {
        return Err("document create did not verify the exact source bytes".to_owned());
    }

    let document_name = format!("document-export-{suffix}.md");
    let document_export = driver
        .call_tool(
            "document_export",
            json!({
                "space": space_id,
                "object_id": object_id,
                "destination": artifact_destination(&transport, &document_name),
                "expected_body_sha256": created_document_sha256,
                "idempotency_key": format!("artifact-document-export-{suffix}")
            }),
        )
        .await?;
    let exported_document_sha256 = required_str(&document_export, "/sha256")?;
    let exported_document =
        read_exported_bytes(&transport, fixture.policy, &document_export, &document_name).await?;
    if artifact_sha256(&exported_document) != exported_document_sha256 {
        return Err("document export bytes did not match the reported hash".to_owned());
    }
    release_remote_receipt(driver, &transport, &document_export).await?;

    let update_source = artifact_source(
        driver,
        &transport,
        space_id,
        ARTIFACT_UPDATE_MARKDOWN.as_bytes(),
        ARTIFACT_MARKDOWN_MEDIA_TYPE,
        ArtifactPolicyFixture::UPDATE_SOURCE,
    )
    .await?;
    let updated = driver
        .call_tool(
            "document_import_update",
            json!({
                "space": space_id,
                "object_id": object_id,
                "source": update_source,
                "source_format": "markdown",
                "expected_body_sha256": created_document_sha256,
                "idempotency_key": format!("artifact-document-update-{suffix}")
            }),
        )
        .await?;
    let updated_document_sha256 = required_str(&updated, "/canonical_sha256")?;
    if updated_document_sha256 == created_document_sha256 || updated["no_op"] != Value::Bool(false)
    {
        return Err("document update did not verify a changed body".to_owned());
    }

    let stage_released = allocate_and_release_stage(driver, space_id).await?;

    Ok(ArtifactSmokeEvidence {
        transport: transport.id(),
        catalog,
        import_root_count,
        export_root_count,
        staging_active,
        file_bytes,
        file_sha256,
        created_document_sha256,
        exported_document_sha256,
        updated_document_sha256,
        stage_released,
    })
}

async fn artifact_source(
    driver: &mut impl McpDriver,
    transport: &ArtifactTransport,
    space_id: &str,
    payload: &[u8],
    media_type: &str,
    local_path: &str,
) -> Result<Value, String> {
    match transport.data() {
        ArtifactDataPlane::LocalRoots => Ok(json!({
            "local": {"root": ArtifactPolicyFixture::IMPORT_ROOT, "path": local_path}
        })),
        ArtifactDataPlane::RemoteStaging => {
            let handle = stage_upload(driver, space_id, payload, media_type).await?;
            Ok(json!({"staged_handle": handle}))
        }
    }
}

fn artifact_destination(transport: &ArtifactTransport, relative: &str) -> Value {
    match transport.data() {
        ArtifactDataPlane::LocalRoots => json!({
            "local": {"root": ArtifactPolicyFixture::EXPORT_ROOT, "path": relative}
        }),
        ArtifactDataPlane::RemoteStaging => json!({"remote": true}),
    }
}

/// Allocates a staging record, uploads exact bytes, and returns its bearer.
async fn stage_upload(
    driver: &mut impl McpDriver,
    space_id: &str,
    payload: &[u8],
    media_type: &str,
) -> Result<String, String> {
    let size = u64::try_from(payload.len())
        .map_err(|_| "staged payload exceeds the addressable range".to_owned())?;
    let allocation = driver
        .call_tool(
            "artifact_stage_upload",
            json!({
                "space": space_id,
                "size_bytes": size,
                "media_type": media_type,
                "expected_sha256": artifact_sha256(payload)
            }),
        )
        .await?;
    let handle = required_str(&allocation, "/handle")?;
    let url = required_str(&allocation, "/upload_url")?;
    let last = size
        .checked_sub(1)
        .ok_or_else(|| "staged payload must not be empty".to_owned())?;
    let response = staging_client()?
        .put(&url)
        .header(AUTHORIZATION, format!("Bearer {handle}"))
        .header(CONTENT_TYPE, media_type.to_owned())
        .header(CONTENT_RANGE, format!("bytes 0-{last}/{size}"))
        .body(payload.to_vec())
        .send()
        .await
        .map_err(|_| "staged upload transport failed".to_owned())?;
    if !response.status().is_success() {
        return Err(format!(
            "staged upload rejected with status {}",
            response.status().as_u16()
        ));
    }
    Ok(handle)
}

/// Reads exact published bytes from the selected data plane.
async fn read_exported_bytes(
    transport: &ArtifactTransport,
    policy: &ArtifactPolicyFixture,
    receipt_owner: &Value,
    relative: &str,
) -> Result<Vec<u8>, String> {
    match transport.data() {
        ArtifactDataPlane::LocalRoots => {
            if !policy.export_exists(relative) {
                return Err("export did not create the authorized artifact".to_owned());
            }
            policy.read_export(relative)
        }
        ArtifactDataPlane::RemoteStaging => {
            let handle = required_str(receipt_owner, "/receipt/staging_handle")?;
            let url = required_str(receipt_owner, "/receipt/staging_url")?;
            if url.contains(&handle) {
                return Err("staging URL must never carry the bearer credential".to_owned());
            }
            let response = staging_client()?
                .get(&url)
                .header(AUTHORIZATION, format!("Bearer {handle}"))
                .header(RANGE, "bytes=0-")
                .send()
                .await
                .map_err(|_| "staged download transport failed".to_owned())?;
            if !response.status().is_success() {
                return Err(format!(
                    "staged download rejected with status {}",
                    response.status().as_u16()
                ));
            }
            response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|_| "staged download body failed".to_owned())
        }
    }
}

/// Releases a remote publication so no staged byte outlives the scenario.
async fn release_remote_receipt(
    driver: &mut impl McpDriver,
    transport: &ArtifactTransport,
    receipt_owner: &Value,
) -> Result<(), String> {
    if transport.data() != ArtifactDataPlane::RemoteStaging {
        return Ok(());
    }
    let handle = required_str(receipt_owner, "/receipt/staging_handle")?;
    let released = driver
        .call_tool("artifact_release", json!({"handle": handle}))
        .await?;
    if released["released"] == Value::Bool(true) {
        Ok(())
    } else {
        Err("remote artifact publication was not released".to_owned())
    }
}

async fn allocate_and_release_stage(
    driver: &mut impl McpDriver,
    space_id: &str,
) -> Result<bool, String> {
    let allocation = driver
        .call_tool(
            "artifact_stage_upload",
            json!({
                "space": space_id,
                "size_bytes": ARTIFACT_FILE_PAYLOAD.len(),
                "media_type": ARTIFACT_FILE_MEDIA_TYPE,
                "expected_sha256": artifact_sha256(ARTIFACT_FILE_PAYLOAD)
            }),
        )
        .await?;
    let handle = required_str(&allocation, "/handle")?;
    let record = required_str(&allocation, "/record")?;
    let url = required_str(&allocation, "/upload_url")?;
    if !url.contains(&record) || url.contains(&handle) {
        return Err("staging URL must expose only the non-secret record".to_owned());
    }
    let released = driver
        .call_tool("artifact_release", json!({"handle": handle}))
        .await?;
    Ok(released["released"] == Value::Bool(true))
}

fn staging_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "build staging data-plane client".to_owned())
}

fn required_str(value: &Value, pointer: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("artifact result omitted {pointer}"))
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("artifact status omitted {field}"))
}

/// Validates one complete JSON-RPC `tools/call` frame and returns its content.
///
/// The scripted-protocol control plane asserts the exact wire envelope rather
/// than a decoded convenience value.
///
/// # Errors
///
/// Returns a fixed message describing the first envelope violation.
pub fn validate_tool_frame(name: &str, id: u64, frame: &Value) -> Result<Value, String> {
    if frame["jsonrpc"] != Value::String("2.0".to_owned()) {
        return Err(format!("{name} frame omitted the JSON-RPC version"));
    }
    if frame["id"].as_u64() != Some(id) {
        return Err(format!("{name} frame carried a mismatched identifier"));
    }
    if frame.get("error").is_some() {
        return Err(format!("{name} frame returned a protocol error"));
    }
    let result = frame
        .get("result")
        .ok_or_else(|| format!("{name} frame omitted its result"))?;
    if result["isError"] != Value::Bool(false) {
        return Err(format!("{name} frame reported a tool error"));
    }
    let content_len = result["content"]
        .as_array()
        .ok_or_else(|| format!("{name} frame omitted its content array"))?
        .len();
    if content_len == 0 {
        return Err(format!("{name} frame returned empty content"));
    }
    result
        .get("structuredContent")
        .cloned()
        .ok_or_else(|| format!("{name} frame omitted structured content"))
}

/// Fixed upstream server-log error classes already isolated and tracked.
pub const KNOWN_SERVER_LOG_CLASSES: [(&str, &str); 5] = [
    (
        "deleted_space_sync_status",
        "failed to update details failed to load space",
    ),
    (
        "filesync_pending_upload",
        "process next pending upload item",
    ),
    ("headsync_peer", "can't sync with peer"),
    ("object_cache_closed", "object cache is closed"),
    ("space_storage_sqlite", "SQLITE_ERROR"),
];

/// Content-free audit of a captured Anytype server log window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArtifactServerLogAudit {
    /// Lines inspected in the bounded window.
    pub inspected_lines: u64,
    /// Lines reporting a panic or a fatal condition.
    pub panic_or_fatal_lines: u64,
    /// Counts of already isolated upstream error classes.
    pub known_classes: BTreeMap<&'static str, u64>,
    /// Error lines matching no known class.
    pub unclassified_error_lines: u64,
}

impl ArtifactServerLogAudit {
    /// Whether the window contains no panic, fatal, or new error class.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.panic_or_fatal_lines == 0 && self.unclassified_error_lines == 0
    }
}

/// Current byte length of a captured server log, used as a window baseline.
///
/// # Errors
///
/// Returns a fixed message when the log cannot be inspected.
pub fn server_log_offset(path: &Path) -> Result<u64, String> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|_| "inspect captured server log".to_owned())
}

/// Classifies error lines appended to a captured server log after `from_offset`.
///
/// Only counts and fixed category names are retained; no log line is returned.
///
/// # Errors
///
/// Returns a fixed message when the log cannot be read or shrank below the
/// recorded baseline.
pub fn audit_server_log(path: &Path, from_offset: u64) -> Result<ArtifactServerLogAudit, String> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    let mut file = fs::File::open(path).map_err(|_| "read captured server log".to_owned())?;
    let length = file
        .metadata()
        .map_err(|_| "read captured server log".to_owned())?
        .len();
    if from_offset > length {
        return Err("captured server log shrank below its baseline".to_owned());
    }
    file.seek(SeekFrom::Start(from_offset))
        .map_err(|_| "read captured server log".to_owned())?;

    // Stream the appended window one line at a time so a multi-hundred-megabyte
    // captured log is never held in memory, and so non-UTF-8 bytes outside the
    // audited window can never fail the audit.
    let mut reader = BufReader::new(file);
    let mut audit = ArtifactServerLogAudit::default();
    let mut raw = Vec::new();
    loop {
        raw.clear();
        let read = reader
            .read_until(b'\n', &mut raw)
            .map_err(|_| "read captured server log".to_owned())?;
        if read == 0 {
            break;
        }
        while matches!(raw.last(), Some(b'\n' | b'\r')) {
            raw.pop();
        }
        classify_server_log_line(&mut audit, &String::from_utf8_lossy(&raw));
    }
    Ok(audit)
}

/// Classifies a bounded captured-log window without retaining any line.
#[must_use]
pub fn classify_server_log(window: &str) -> ArtifactServerLogAudit {
    let mut audit = ArtifactServerLogAudit::default();
    for line in window.lines() {
        classify_server_log_line(&mut audit, line);
    }
    audit
}

/// Accumulates one captured-log line into an audit without retaining it.
fn classify_server_log_line(audit: &mut ArtifactServerLogAudit, line: &str) {
    audit.inspected_lines = audit.inspected_lines.saturating_add(1);
    if is_panic_or_fatal(line) {
        audit.panic_or_fatal_lines = audit.panic_or_fatal_lines.saturating_add(1);
        return;
    }
    if !is_error_line(line) {
        return;
    }
    match KNOWN_SERVER_LOG_CLASSES
        .into_iter()
        .find(|(_, marker)| line.contains(marker))
    {
        Some((class, _)) => {
            let counter = audit.known_classes.entry(class).or_insert(0);
            *counter = counter.saturating_add(1);
        }
        None => {
            audit.unclassified_error_lines = audit.unclassified_error_lines.saturating_add(1);
        }
    }
}

fn is_panic_or_fatal(line: &str) -> bool {
    line.contains("\"level\":\"fatal\"")
        || line.contains("panic: ")
        || line.contains("runtime error:")
        || line.starts_with("goroutine ")
}

fn is_error_line(line: &str) -> bool {
    line.contains("\"level\":\"error\"") || line.contains("\tERROR\t")
}

/// Requires a completed disposable run, rejecting skipped admission.
///
/// # Errors
///
/// Returns a fixed message when admission was skipped, so an unauthorized or
/// unconfigured environment can never be reported as passing coverage.
pub fn require_completed<T>(run: DisposableRun<T>, scenario: &str) -> Result<T, String> {
    match run {
        DisposableRun::Completed(value) => Ok(value),
        DisposableRun::Skipped(_) => Err(format!(
            "{scenario} requires prefix-authorized disposable admission"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_matrix_is_closed_complete_and_uniquely_identified() {
        let mut ids = std::collections::BTreeSet::new();
        for transport in ArtifactTransport::ALL {
            assert!(ids.insert(transport.id()), "duplicate transport identifier");
            assert_eq!(ArtifactTransport::parse(transport.id()), Some(transport));
        }
        assert_eq!(
            ids.len(),
            ArtifactControlPlane::ALL.len() * ArtifactDataPlane::ALL.len()
        );
        for control in ArtifactControlPlane::ALL {
            assert_eq!(ArtifactControlPlane::parse(control.as_str()), Some(control));
            for data in ArtifactDataPlane::ALL {
                assert!(ArtifactTransport::ALL.contains(&ArtifactTransport::new(control, data)));
            }
        }
    }

    #[test]
    fn executed_matrices_partition_the_complete_transport_inventory() {
        let mut union = ArtifactTransport::DIRECT_MATRIX.to_vec();
        union.extend(ArtifactTransport::SPAWNED_MATRIX);
        assert_eq!(union.len(), ArtifactTransport::ALL.len());
        for transport in ArtifactTransport::ALL {
            assert_eq!(
                union
                    .iter()
                    .filter(|candidate| **candidate == transport)
                    .count(),
                1,
                "transport {} is not executed exactly once",
                transport.id()
            );
        }
        assert!(
            ArtifactTransport::DIRECT_MATRIX
                .iter()
                .all(|transport| !transport.control().is_spawned())
        );
        assert!(
            ArtifactTransport::SPAWNED_MATRIX
                .iter()
                .all(|transport| transport.control().is_spawned())
        );
    }

    #[test]
    fn preview_control_plane_is_the_only_preview_revision() {
        for control in ArtifactControlPlane::ALL {
            let expected = if control == ArtifactControlPlane::SpawnedPreviewStdio {
                "2026-07-28"
            } else {
                "2025-11-25"
            };
            assert_eq!(control.protocol_version(), expected);
        }
    }

    fn descriptor(name: &str, description: &str) -> Value {
        json!({
            "name": name,
            "description": description,
            "inputSchema": {"type": "object", "properties": {"space": {"type": "string"}}},
            "outputSchema": {"type": "object"}
        })
    }

    fn complete_descriptors() -> Vec<Value> {
        ARTIFACT_TOOL_NAMES
            .into_iter()
            .map(|name| descriptor(name, "bounded"))
            .collect()
    }

    #[test]
    fn catalog_snapshot_requires_the_exact_artifact_inventory() {
        let mut descriptors = complete_descriptors();
        assert!(ArtifactCatalogSnapshot::from_descriptors(&descriptors).is_ok());
        descriptors.push(descriptor("object_search", "unrelated"));
        let snapshot = ArtifactCatalogSnapshot::from_descriptors(&descriptors)
            .expect("unrelated tools are ignored");
        assert_eq!(snapshot.tool_digests().len(), ARTIFACT_TOOL_NAMES.len());

        descriptors.retain(|entry| entry["name"] != json!("file_import"));
        assert!(ArtifactCatalogSnapshot::from_descriptors(&descriptors).is_err());
    }

    #[test]
    fn catalog_snapshot_ignores_key_order_and_detects_contract_drift() {
        let baseline = ArtifactCatalogSnapshot::from_descriptors(&complete_descriptors())
            .expect("baseline catalog");
        let reordered = ARTIFACT_TOOL_NAMES
            .into_iter()
            .map(|name| {
                json!({
                    "outputSchema": {"type": "object"},
                    "inputSchema": {"properties": {"space": {"type": "string"}}, "type": "object"},
                    "description": "bounded",
                    "name": name
                })
            })
            .collect::<Vec<_>>();
        let reordered =
            ArtifactCatalogSnapshot::from_descriptors(&reordered).expect("reordered catalog");
        assert_eq!(baseline.digest(), reordered.digest());
        assert!(baseline.compare(&reordered).is_ok());

        let mut drifted = complete_descriptors();
        drifted[0] = descriptor(ARTIFACT_TOOL_NAMES[0], "relaxed");
        let drifted = ArtifactCatalogSnapshot::from_descriptors(&drifted).expect("drifted catalog");
        assert_ne!(baseline.digest(), drifted.digest());
        assert_eq!(
            baseline.compare(&drifted),
            Err(format!(
                "artifact tool contract diverged: {}",
                ARTIFACT_TOOL_NAMES[0]
            ))
        );
    }

    fn evidence(transport: ArtifactTransport, file_sha256: &str) -> ArtifactSmokeEvidence {
        ArtifactSmokeEvidence {
            transport: transport.id(),
            catalog: ArtifactCatalogSnapshot::from_descriptors(&complete_descriptors())
                .expect("catalog"),
            import_root_count: 1,
            export_root_count: 1,
            staging_active: true,
            file_bytes: 21,
            file_sha256: file_sha256.to_owned(),
            created_document_sha256: "a".repeat(64),
            exported_document_sha256: "a".repeat(64),
            updated_document_sha256: "b".repeat(64),
            stage_released: true,
        }
    }

    #[test]
    fn parity_requires_the_complete_ordered_matrix_and_identical_behavior() {
        let expected = ArtifactTransport::DIRECT_MATRIX;
        let hash = artifact_sha256(ARTIFACT_FILE_PAYLOAD);
        let executed = vec![evidence(expected[0], &hash), evidence(expected[1], &hash)];
        assert_eq!(assert_artifact_parity(&executed, &expected), Ok(()));

        assert!(assert_artifact_parity(&executed[..1], &expected).is_err());
        let reversed = vec![evidence(expected[1], &hash), evidence(expected[0], &hash)];
        assert!(assert_artifact_parity(&reversed, &expected).is_err());

        let divergent = vec![
            evidence(expected[0], &hash),
            evidence(expected[1], &"c".repeat(64)),
        ];
        assert_eq!(
            assert_artifact_parity(&divergent, &expected),
            Err(format!(
                "artifact transport diverged from {}: {}",
                expected[0].id(),
                expected[1].id()
            ))
        );
    }

    #[test]
    fn tool_frames_must_carry_a_complete_successful_envelope() {
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {
                "isError": false,
                "content": [{"type": "text", "text": "{}"}],
                "structuredContent": {"released": true}
            }
        });
        assert_eq!(
            validate_tool_frame("artifact_release", 7, &frame),
            Ok(json!({"released": true}))
        );
        assert!(validate_tool_frame("artifact_release", 8, &frame).is_err());

        for mutation in [
            json!({"id": 7, "result": {"isError": false, "content": [1], "structuredContent": {}}}),
            json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32602}}),
            json!({"jsonrpc": "2.0", "id": 7, "result": {"isError": true, "content": [1], "structuredContent": {}}}),
            json!({"jsonrpc": "2.0", "id": 7, "result": {"isError": false, "content": [], "structuredContent": {}}}),
            json!({"jsonrpc": "2.0", "id": 7, "result": {"isError": false, "content": [1]}}),
        ] {
            assert!(validate_tool_frame("artifact_release", 7, &mutation).is_err());
        }
    }

    #[test]
    fn server_log_audit_counts_classes_without_retaining_lines() {
        let window = concat!(
            "{\"level\":\"error\",\"msg\":\"failed to update details failed to load space, mode is 3\"}\n",
            "{\"level\":\"error\",\"msg\":\"failed to update details failed to load space, mode is 3\"}\n",
            "{\"level\":\"error\",\"msg\":\"process next pending upload item\"}\n",
            "{\"level\":\"info\",\"msg\":\"ordinary\"}\n",
            "{\"level\":\"error\",\"msg\":\"brand new artifact staging failure\"}\n",
            "{\"level\":\"fatal\",\"msg\":\"store closed\"}\n"
        );
        let audit = classify_server_log(window);
        assert_eq!(audit.inspected_lines, 6);
        assert_eq!(audit.panic_or_fatal_lines, 1);
        assert_eq!(audit.unclassified_error_lines, 1);
        assert_eq!(
            audit
                .known_classes
                .get("deleted_space_sync_status")
                .copied(),
            Some(2)
        );
        assert_eq!(
            audit.known_classes.get("filesync_pending_upload").copied(),
            Some(1)
        );
        assert!(!audit.is_clean());
        assert!(classify_server_log("{\"level\":\"info\",\"msg\":\"ok\"}\n").is_clean());
    }

    #[test]
    fn server_log_audit_reads_only_the_window_after_the_baseline() {
        let directory = std::env::temp_dir().join(format!("any-mcp-log-{}", unique_suffix()));
        fs::create_dir_all(&directory).expect("create audit fixture directory");
        let path = directory.join("server.log");
        let mut bytes =
            b"{\"level\":\"error\",\"msg\":\"pre-baseline unclassified \xff\xfe\"}\n".to_vec();
        let baseline = u64::try_from(bytes.len()).expect("baseline offset");
        bytes.extend_from_slice(
            b"{\"level\":\"error\",\"msg\":\"process next pending upload item\"}\r\n",
        );
        bytes.extend_from_slice(b"{\"level\":\"info\",\"msg\":\"ordinary\"}\n");
        fs::write(&path, &bytes).expect("write audit fixture log");

        let audit = audit_server_log(&path, baseline).expect("audit appended window");
        assert_eq!(audit.inspected_lines, 2);
        assert_eq!(audit.panic_or_fatal_lines, 0);
        assert_eq!(audit.unclassified_error_lines, 0);
        assert_eq!(
            audit.known_classes.get("filesync_pending_upload").copied(),
            Some(1)
        );

        let total = u64::try_from(bytes.len()).expect("total length");
        assert!(
            audit_server_log(&path, total.saturating_add(1)).is_err(),
            "a shrunken log must fail closed"
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn policy_values_are_escaped_for_toml_basic_strings() {
        assert_eq!(
            toml_basic_string("C:\\Users\\any \"mcp\"\ttmp"),
            "C:\\\\Users\\\\any \\\"mcp\\\"\\ttmp"
        );
        assert_eq!(toml_basic_string("/tmp/any-mcp"), "/tmp/any-mcp");
        assert_eq!(toml_basic_string("\u{1}"), "\\u0001");
    }

    #[test]
    fn policy_fixture_declares_exact_roots_and_private_permissions() {
        let fixture = ArtifactPolicyFixture::create("bafyrei-acceptance-space")
            .expect("artifact policy fixture");
        let contents = fixture.policy_contents().expect("policy contents");
        assert!(contents.contains("schema_version = 1"));
        assert!(contents.contains("read_only = false"));
        assert!(contents.contains("id = \"bafyrei-acceptance-space\""));
        assert!(contents.contains("id = \"inbox\""));
        assert!(contents.contains("id = \"outbox\""));
        assert!(contents.contains("[staging]"));
        assert!(
            fixture
                .staging_base_url()
                .is_some_and(|url| url.starts_with("http://127.0.0.1:"))
        );
        assert_eq!(
            fs::read(
                fixture
                    .import_root()
                    .join(ArtifactPolicyFixture::FILE_SOURCE)
            )
            .expect("seeded file source"),
            ARTIFACT_FILE_PAYLOAD
        );
        assert!(fixture.read_export("../escape").is_err());
        assert!(!fixture.export_exists("missing.bin"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(fixture.config_path())
                .expect("policy metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let base = fixture.base.clone();
        drop(fixture);
        assert!(!base.exists(), "fixture teardown removed every artifact");
    }

    #[test]
    fn policy_fixture_omits_staging_when_it_is_disabled() {
        let fixture = ArtifactPolicyFixture::create_with(
            "bafyrei-acceptance-space",
            ArtifactPolicyOptions {
                staging: false,
                read_only: true,
            },
        )
        .expect("artifact policy fixture");
        let contents = fixture.policy_contents().expect("policy contents");
        assert!(!contents.contains("[staging]"));
        assert!(contents.contains("read_only = true"));
        assert!(fixture.staging_base_url().is_none());
    }

    #[test]
    fn skipped_disposable_admission_is_never_reported_as_coverage() {
        let completed: DisposableRun<u8> = DisposableRun::Completed(3);
        assert_eq!(require_completed(completed, "artifact smoke"), Ok(3));
        let skipped: DisposableRun<u8> =
            DisposableRun::Skipped(anytype::test_util::DisposableSkip::PrefixNotConfigured);
        assert!(require_completed(skipped, "artifact smoke").is_err());
    }
}
