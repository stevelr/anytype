use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::IsTerminal,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use crate::archive::{
    ArchiveFileEntry, ArchiveReader, infer_object_id_from_snapshot_path,
    infer_object_ids_from_files,
};
use crate::markdown::{SavedObjectKind, save_archive_object};
use anyhow::{Context, Result, anyhow, bail, ensure};
use anytype::{
    prelude::*,
    process_watcher::{
        ProcessCompletionFallback, ProcessKind, ProcessWatchCancelToken, ProcessWatchProgress,
        ProcessWatchRequest, ProcessWatcher,
    },
    validation::looks_like_object_id,
};
#[cfg(feature = "snapshot-import")]
use anytype_rpc::anytype::SnapshotWithType;
use anytype_rpc::{
    anytype::rpc::object::import::{Request as ObjectImportRequest, request as import_request},
    auth::with_token,
};
use chrono::{
    DateTime, FixedOffset, Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc,
};
use clap::{Args, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
#[cfg(feature = "snapshot-import")]
use prost::Message;
use same_file::Handle as FileIdentity;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{info, warn};

mod deadline;
pub mod decode;
#[cfg(feature = "tui")]
mod inspector;
pub mod output;

pub use deadline::WorkflowDeadline;
pub use output::{CommandOutput, OutputMode, TextBuilder};

use decode::{
    ExpandedSnapshotEntry, ImportEventProgressReport, ImportReport, MANIFEST_NAME, Manifest,
    ManifestSummary, ObjectDescriptor, ObjectImportError, archive_binding_from_file, detail_value,
    format_datetime_display, format_last_modified, manifest_sidecar_path, manifest_summary,
    parse_expanded_entries, parse_snapshot_details_from_pb, parse_snapshot_details_from_pb_json,
    read_manifest_from_reader, read_manifest_from_sidecar, read_manifest_prefer_sidecar,
};

const TMP_BACKUP_PREFIX: &str = "anyback_tmp";
#[cfg(feature = "snapshot-import")]
const DEFAULT_IMPORT_MAX_SINGLE_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
#[cfg(feature = "snapshot-import")]
const DEFAULT_IMPORT_MAX_BATCH_BYTES: usize = 3 * 1024 * 1024;
#[cfg(feature = "snapshot-import")]
const DEFAULT_IMPORT_MAX_BATCH_SNAPSHOTS: usize = 128;
const IMPORT_CANCEL_REASON: &str = "restore canceled by user";

type ImportCancelToken = ProcessWatchCancelToken;

#[derive(Debug)]
struct ImportCancelState {
    receiver: mpsc::UnboundedReceiver<ImportCancelToken>,
}

impl ImportCancelState {
    fn new(receiver: mpsc::UnboundedReceiver<ImportCancelToken>) -> Self {
        Self { receiver }
    }

    fn receiver_mut(&mut self) -> &mut mpsc::UnboundedReceiver<ImportCancelToken> {
        &mut self.receiver
    }
}

fn new_import_cancel_channel() -> (mpsc::UnboundedSender<ImportCancelToken>, ImportCancelState) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (sender, ImportCancelState::new(receiver))
}

fn spawn_import_cancel_signal_forwarder(
    sender: mpsc::UnboundedSender<ImportCancelToken>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(stream) => stream,
                Err(err) => {
                    warn!("failed to register SIGTERM handler: {err:#}");
                    let _ = tokio::signal::ctrl_c().await;
                    let _ = sender.send(ImportCancelToken::Requested);
                    return;
                }
            };

            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = sigterm.recv() => {},
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }

        let _ = sender.send(ImportCancelToken::Requested);
    })
}

#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum Commands {
    /// Create a backup (requires Anytype CLI server and gRPC credentials)
    Create(BackupCreateArgs),

    /// Restore objects (CLI server/gRPC credentials required unless --dry-run)
    Restore(RestoreApplyArgs),

    /// List archive contents
    List(ListArgs),

    /// Show archive manifest
    Manifest(ManifestArgs),

    /// Compare two archives
    Diff(DiffArgs),

    /// Extract one object from an archive
    Extract(ExtractArgs),

    /// Export objects (requires Anytype CLI server and gRPC credentials)
    Export(BackupCreateArgs),

    /// Import objects (CLI server/gRPC credentials required unless --dry-run)
    Import(RestoreApplyArgs),

    /// Interactive archive browser (TUI)
    #[cfg(feature = "tui")]
    Inspect(InspectorArgs),
}

#[cfg(feature = "tui")]
#[derive(Args, Debug)]
pub struct InspectorArgs {
    /// Archive path (directory or .zip)
    pub archive: PathBuf,

    /// Maximum inspector cache size (default unit: MiB). Examples: 200, 512k, 64mb, 1g
    #[arg(long = "max-cache", value_name = "SIZE", default_value = "200", value_parser = parse_cache_size)]
    pub max_cache: usize,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Args, Debug)]
pub struct BackupCreateArgs {
    /// Space name or id. Name must be unambiguous.
    #[arg(long, value_name = "NAME_OR_ID")]
    pub space: String,

    /// Object IDs source path, or '-' to read from stdin. Omit for full-space backup.
    #[arg(long, value_name = "FILE|-")]
    pub objects: Option<String>,

    /// Export format
    #[arg(long, value_enum, default_value_t = ExportFormatArg::Pb)]
    pub format: ExportFormatArg,

    /// Backup mode
    #[arg(long, value_enum, default_value_t = BackupModeArg::Full)]
    pub mode: BackupModeArg,

    /// Incremental lower bound timestamp.
    /// Accepts RFC3339 with timezone/offset, or no-timezone local time (assumed local timezone).
    /// Example UTC values: `2026-01-12T10:11:22Z`, `2026-01-12 10:11:22 UTC`, `2026-01-12T10:11:22+00:00`.
    #[arg(long, value_name = "RFC3339", required_if_eq("mode", "incremental"))]
    pub since: Option<String>,

    /// Incremental window mode
    #[arg(long, value_enum, default_value_t = SinceModeArg::Exclusive)]
    pub since_mode: SinceModeArg,

    /// Include only these object types (comma-separated keys and/or ids)
    #[arg(
        long,
        value_name = "TYPE_KEY_OR_ID[,TYPE_KEY_OR_ID,...]",
        value_delimiter = ',',
        conflicts_with = "objects"
    )]
    pub types: Option<Vec<String>>,

    /// Parent directory where the archive will be created (default: current directory)
    #[arg(long, value_name = "DIR", conflicts_with = "dest")]
    pub dir: Option<PathBuf>,

    /// Output archive path to create
    #[arg(long, value_name = "PATH", conflicts_with_all = ["dir", "prefix"])]
    pub dest: Option<PathBuf>,

    /// Archive name prefix used with --dir/default parent; ignored when --dest is used
    #[arg(long, value_name = "PREFIX")]
    pub prefix: Option<String>,

    /// Include linked (nested) objects in export payload
    #[arg(long)]
    pub include_nested: bool,

    /// Include file objects and file binaries in export payload
    #[arg(long)]
    pub include_files: bool,

    /// Include archived objects in backup selection
    #[arg(long)]
    pub include_archived: bool,

    /// Include backlinks in export payload
    #[arg(long)]
    pub include_backlinks: bool,

    /// Include properties and schema in markdown export output
    #[arg(long)]
    pub include_properties: bool,
}

#[derive(Args, Debug)]
pub struct RestoreApplyArgs {
    /// Archive path (directory or .zip)
    #[arg(value_name = "ARCHIVE")]
    pub archive: PathBuf,

    /// Optional object IDs source path, or '-' to read from stdin.
    #[arg(long, value_name = "FILE|-")]
    pub objects: Option<String>,

    /// Destination space name or id. Space must exist.
    #[arg(long, value_name = "NAME_OR_ID")]
    pub space: Option<String>,

    /// Validate restore inputs and selection without importing objects
    #[arg(long)]
    pub dry_run: bool,

    /// Write detailed JSON import report to file
    #[arg(long, value_name = "REPORT_OUTPUT")]
    pub log: Option<PathBuf>,

    /// Import mode. all-or-nothing stops on first error but does not roll back prior imports.
    #[arg(long, value_enum, default_value_t = ImportModeArg::IgnoreErrors)]
    pub import_mode: ImportModeArg,

    /// Replace objects that already exist in the destination space.
    /// Without this flag, existing objects are left unchanged.
    #[arg(long)]
    pub replace: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ListArgs {
    /// Archive path (directory or .zip)
    pub archive: PathBuf,

    /// Summary only (omit object IDs)
    #[arg(long, group = "list_mode")]
    pub brief: bool,

    /// Include per-object expanded metadata
    #[arg(long, group = "list_mode")]
    pub expanded: bool,

    /// Include file listing with sizes
    #[arg(long, group = "list_mode")]
    pub files: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ManifestArgs {
    /// Archive path (directory or .zip)
    pub archive: PathBuf,
}

#[derive(Args, Debug, Clone)]
pub struct DiffArgs {
    /// First archive path (directory or .zip)
    #[arg(value_name = "ARCHIVE1")]
    pub archive1: PathBuf,

    /// Second archive path (directory or .zip)
    #[arg(value_name = "ARCHIVE2")]
    pub archive2: PathBuf,
}

#[derive(Args, Debug, Clone)]
pub struct ExtractArgs {
    /// Archive path (directory or .zip)
    #[arg(value_name = "ARCHIVE")]
    pub archive: PathBuf,

    /// Object ID to extract
    #[arg(value_name = "ID")]
    pub object_id: String,

    /// Output file path
    #[arg(value_name = "OUTPUT")]
    pub destination: PathBuf,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ExportFormatArg {
    Markdown,
    Pb,
    PbJson,
    Json,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ImportModeArg {
    AllOrNothing,
    IgnoreErrors,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum BackupModeArg {
    Full,
    Incremental,
}

impl BackupModeArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Incremental => "incremental",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SinceModeArg {
    Exclusive,
    Inclusive,
}

impl ImportModeArg {
    fn to_rpc_mode(self) -> i32 {
        match self {
            Self::AllOrNothing => import_request::Mode::AllOrNothing as i32,
            Self::IgnoreErrors => import_request::Mode::IgnoreErrors as i32,
        }
    }
}

impl ExportFormatArg {
    fn to_backup_export_format(self) -> BackupExportFormat {
        match self {
            Self::Markdown => BackupExportFormat::Markdown,
            Self::Pb | Self::PbJson => BackupExportFormat::Protobuf,
            Self::Json => BackupExportFormat::Json,
        }
    }

    fn is_pb_json(self) -> bool {
        matches!(self, Self::PbJson)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Pb => "pb",
            Self::PbJson => "pb-json",
            Self::Json => "json",
        }
    }
}

pub struct AppContext {
    pub client: AnytypeClient,
    pub output: CommandOutput,
}

struct WorkflowContext {
    app: AppContext,
    deadline: WorkflowDeadline,
}

impl std::ops::Deref for WorkflowContext {
    type Target = AppContext;

    fn deref(&self) -> &Self::Target {
        &self.app
    }
}

/// Commands that render an interactive terminal UI and therefore cannot be
/// redirected, formatted, or silenced by the standard output contract.
#[must_use]
pub const fn command_is_interactive(command: &Commands) -> bool {
    #[cfg(feature = "tui")]
    {
        matches!(command, Commands::Inspect(_))
    }
    #[cfg(not(feature = "tui"))]
    {
        let _ = command;
        false
    }
}

/// The command name as spelled on the command line, for diagnostics.
#[must_use]
pub const fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Create(_) => "create",
        Commands::Restore(_) => "restore",
        Commands::List(_) => "list",
        Commands::Manifest(_) => "manifest",
        Commands::Diff(_) => "diff",
        Commands::Extract(_) => "extract",
        Commands::Export(_) => "export",
        Commands::Import(_) => "import",
        #[cfg(feature = "tui")]
        Commands::Inspect(_) => "inspect",
    }
}

/// Rejects a result path that aliases an input or artifact used by `command`.
///
/// Parent CLIs should call this during argument validation for early errors.
/// [`run_command`] also calls it before dispatch so library users receive the
/// same protection.
pub fn validate_command_output(command: &Commands, output: &CommandOutput) -> Result<()> {
    match command {
        Commands::Create(args) | Commands::Export(args) => {
            validate_object_list_output(output, args.objects.as_deref())?;
            if let Some(dest) = args.dest.as_deref() {
                validate_archive_output(output, dest, "created archive")?;
            }
        }
        Commands::Restore(args) | Commands::Import(args) => {
            validate_archive_output(output, &args.archive, "input archive")?;
            validate_object_list_output(output, args.objects.as_deref())?;
            if let Some(log) = args.log.as_deref() {
                output.ensure_distinct_from(log, "restore report")?;
            }
        }
        Commands::List(args) => {
            validate_archive_output(output, &args.archive, "input archive")?;
        }
        Commands::Manifest(args) => {
            validate_archive_output(output, &args.archive, "input archive")?;
        }
        Commands::Diff(args) => {
            validate_archive_output(output, &args.archive1, "first input archive")?;
            validate_archive_output(output, &args.archive2, "second input archive")?;
        }
        Commands::Extract(args) => {
            validate_archive_output(output, &args.archive, "input archive")?;
            output.ensure_distinct_from(&args.destination, "extracted object")?;
        }
        #[cfg(feature = "tui")]
        Commands::Inspect(_) => {}
    }
    Ok(())
}

fn validate_archive_output(
    output: &CommandOutput,
    archive: &Path,
    description: &str,
) -> Result<()> {
    output.ensure_distinct_from(archive, description)?;
    output.ensure_distinct_from(&manifest_sidecar_path(archive), "archive manifest")
}

fn validate_object_list_output(output: &CommandOutput, spec: Option<&str>) -> Result<()> {
    if let Some(spec) = spec.filter(|value| *value != "-") {
        output.ensure_distinct_from(Path::new(spec), "object list input")?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct ListReport {
    archive: String,
    source: String,
    file_count: usize,
    total_bytes: u64,
    manifest_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_summary: Option<ManifestSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<ArchiveFileEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expanded: Option<Vec<ExpandedSnapshotEntry>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ArchiveCmpObject {
    object_id: String,
    r#type: String,
    name: String,
    size: u64,
    last_modified: String,
}

#[derive(Debug, Clone, Serialize)]
struct ArchiveCmpChanged {
    left: ArchiveCmpObject,
    right: ArchiveCmpObject,
}

#[derive(Debug, Clone, Serialize)]
struct ArchiveCmpReport {
    archive1: String,
    archive2: String,
    format1: String,
    format2: String,
    archive1_only: Vec<ArchiveCmpObject>,
    archive2_only: Vec<ArchiveCmpObject>,
    changed: Vec<ArchiveCmpChanged>,
}

/// Execute a backup command with a client and output contract configured by
/// the parent application.
///
/// The parent application is responsible for rejecting output combinations the
/// requested command cannot honor; see [`command_is_interactive`].
pub async fn run_command(
    command: Commands,
    client: AnytypeClient,
    output: CommandOutput,
) -> Result<()> {
    let deadline = if matches!(
        &command,
        Commands::Create(_) | Commands::Export(_) | Commands::Restore(_) | Commands::Import(_)
    ) {
        WorkflowDeadline::from_env()?
    } else {
        WorkflowDeadline::local_command()
    };
    Box::pin(run_command_with_deadline(command, client, output, deadline)).await
}

/// Executes a backup command with timeout configuration captured before client construction.
pub async fn run_command_with_deadline(
    command: Commands,
    client: AnytypeClient,
    output: CommandOutput,
    deadline: WorkflowDeadline,
) -> Result<()> {
    validate_command_output(&command, &output)?;
    let ctx = WorkflowContext {
        app: AppContext { client, output },
        deadline,
    };

    match command {
        Commands::Create(args) | Commands::Export(args) => handle_backup_create(&ctx, args).await,
        Commands::Restore(args) | Commands::Import(args) => handle_restore_apply(&ctx, args).await,
        Commands::List(args) => handle_list(&ctx.output, &args),
        Commands::Manifest(args) => handle_manifest(&ctx.output, &args),
        Commands::Diff(args) => handle_diff(&ctx.output, &args),
        Commands::Extract(args) => handle_extract(&ctx.output, &args),
        #[cfg(feature = "tui")]
        Commands::Inspect(args) => inspector::run_inspector(&args.archive, args.max_cache),
    }
}

async fn handle_backup_create(ctx: &WorkflowContext, args: BackupCreateArgs) -> Result<()> {
    validate_backup_args(&args)?;
    ctx.deadline.ensure_read_remaining()?;
    let export_options = backup_export_options(&args);

    let progress = ProgressReporter::new(&ctx.output, "Starting backup");
    let space = ctx
        .deadline
        .run_read(resolve_space(&ctx.client, &args.space))
        .await??;
    let backup_target = resolve_backup_target(&args, &space.id)?;
    validate_archive_output(&ctx.output, &backup_target.archive_path, "created archive")?;
    progress.set_message("Resolved destination space");

    progress.set_message("Collecting object metadata");
    let selection = ctx
        .deadline
        .run_read(resolve_backup_selection(ctx, &space, &args))
        .await??;

    progress.set_message("Exporting archive");
    let mut backup_builder = ctx
        .client
        .backup_space(&space.id)
        .backup_dir(&backup_target.parent_dir)
        .filename_prefix(TMP_BACKUP_PREFIX)
        .format(export_options.format)
        .is_json(export_options.is_json)
        .zip(backup_target.zip)
        .include_nested(export_options.include_nested)
        .include_files(export_options.include_files)
        .include_archived(export_options.include_archived)
        .include_backlinks(export_options.include_backlinks)
        .include_space(export_options.include_space)
        .md_include_properties_and_schema(export_options.md_include_properties_and_schema);

    if let Some(object_ids) = selection.object_ids.clone() {
        backup_builder = backup_builder.object_ids(object_ids);
    }

    let backup = ctx
        .deadline
        .run_export(backup_builder.backup())
        .await?
        .context(
            "export request failed; read was aborted and a server-side export artifact may exist",
        )?;
    let manifest = Manifest {
        schema_version: 1,
        tool: format!("anyback/{}", env!("CARGO_PKG_VERSION")),
        created_at: Utc::now().to_rfc3339(),
        created_at_display: Some(local_now_display()),
        source_space_id: space.id,
        source_space_name: space.name,
        format: args.format.as_str().to_string(),
        object_count: selection.descriptors.len(),
        objects: selection.descriptors,
        mode: Some(args.mode.as_str().to_string()),
        since: selection.since,
        since_display: selection.since_display,
        until: selection.until,
        until_display: selection.until_display,
        type_ids: selection.type_ids,
        archive_size: None,
        archive_sha256: None,
    };

    let source_path = backup.output_path.clone();
    let archive_path = backup_target.archive_path.clone();
    let publication_manifest = manifest.clone();
    ctx.deadline
        .run_read_publication(
            "backup workflow timed out after export; read was aborted and a server-side export artifact may exist",
            move || prepare_backup_artifacts(source_path, archive_path, &publication_manifest),
            commit_backup_artifacts,
        )
        .await?;
    progress.finish("Backup completed");
    publish_backup_result(
        ctx,
        backup_target.archive_path,
        backup.exported,
        manifest.objects.len(),
    )
    .await
}

async fn publish_backup_result(
    ctx: &WorkflowContext,
    archive_path: PathBuf,
    exported: i32,
    requested: usize,
) -> Result<()> {
    let report = serde_json::json!({
        "archive": archive_path.clone(),
        "exported": exported,
        "requested": requested,
    });
    let output = ctx.output.clone();
    let report_archive_path = archive_path;
    ctx.deadline
        .run_read_publication(
            "backup workflow timed out after export; read was aborted and a server-side export artifact may exist",
            move || {
                let Some(rendered) = output.render(&report, || {
                    format!(
                        "archive={} exported={exported}",
                        report_archive_path.display()
                    )
                })? else {
                    return Ok(output::PreparedOutput::Quiet);
                };
                output.prepare_rendered(rendered)
            },
            CommandOutput::commit_prepared,
        )
        .await
}

fn validate_backup_args(args: &BackupCreateArgs) -> Result<()> {
    ensure!(
        !args.include_properties || matches!(args.format, ExportFormatArg::Markdown),
        "--include-properties is only valid with --format markdown"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
struct BackupExportOptions {
    format: BackupExportFormat,
    is_json: bool,
    include_nested: bool,
    include_files: bool,
    include_archived: bool,
    include_backlinks: bool,
    include_space: bool,
    md_include_properties_and_schema: bool,
}

fn backup_export_options(args: &BackupCreateArgs) -> BackupExportOptions {
    BackupExportOptions {
        format: args.format.to_backup_export_format(),
        is_json: args.format.is_pb_json(),
        include_nested: args.include_nested,
        include_files: args.include_files,
        include_archived: args.include_archived,
        include_backlinks: args.include_backlinks,
        // Intentionally always enabled in CLI wiring; this is not a user-facing flag.
        include_space: true,
        md_include_properties_and_schema: args.include_properties,
    }
}

#[derive(Debug)]
struct BackupTarget {
    parent_dir: PathBuf,
    archive_path: PathBuf,
    zip: bool,
}

struct BackupSelection {
    object_ids: Option<Vec<String>>,
    descriptors: Vec<ObjectDescriptor>,
    since: Option<String>,
    since_display: Option<String>,
    until: Option<String>,
    until_display: Option<String>,
    type_ids: Option<Vec<String>>,
}

struct TypeFilter {
    keys: BTreeSet<String>,
    manifest_type_ids: Vec<String>,
}

async fn resolve_backup_selection(
    ctx: &AppContext,
    space: &Space,
    args: &BackupCreateArgs,
) -> Result<BackupSelection> {
    if let Some(spec) = args.objects.as_deref() {
        let object_ids = load_object_ids_spec(spec)?;
        ensure!(
            !object_ids.is_empty(),
            "no object ids supplied to --objects"
        );
        let descriptors = fetch_descriptors_by_ids(&ctx.client, &space.id, &object_ids).await?;
        return Ok(BackupSelection {
            object_ids: Some(object_ids),
            descriptors,
            since: None,
            since_display: None,
            until: None,
            until_display: None,
            type_ids: None,
        });
    }

    let mut query = ctx.client.objects(&space.id).limit(10_000);
    let mut use_filtered_query = false;
    let mut since: Option<String> = None;
    let mut since_display: Option<String> = None;
    let mut until: Option<String> = None;
    let mut until_display: Option<String> = None;

    if matches!(args.mode, BackupModeArg::Incremental) {
        let since_value = parse_since(args.since.as_ref())?;
        let since_rfc3339 = to_rfc3339_with_offset(since_value);
        since_display = Some(format_since_display(since_value));
        since = Some(since_rfc3339.clone());
        let until_now = Utc::now();
        until = Some(until_now.to_rfc3339());
        until_display = Some(format!("{} UTC", until_now.format("%Y-%m-%d %H:%M:%S")));
        use_filtered_query = true;
        query = match args.since_mode {
            SinceModeArg::Exclusive => {
                query.filter(Filter::date_greater("last_modified_date", since_rfc3339))
            }
            SinceModeArg::Inclusive => query.filter(Filter::date_greater_or_equal(
                "last_modified_date",
                since_rfc3339,
            )),
        };
    }

    let type_filter = resolve_type_filter(ctx, &space.id, args.types.as_ref()).await?;
    if type_filter.is_some() {
        use_filtered_query = true;
    }

    if use_filtered_query {
        let objects = query.list().await?.collect_all().await?;
        let mut descriptors: Vec<_> = if type_filter.is_some() {
            let ids: Vec<String> = objects.iter().map(|obj| obj.id.clone()).collect();
            fetch_descriptors_by_ids(&ctx.client, &space.id, &ids).await?
        } else {
            objects.iter().map(object_to_descriptor).collect()
        };
        if let Some(filter) = type_filter.as_ref() {
            descriptors.retain(|descriptor| descriptor_matches_type_filter(descriptor, filter));
        }
        let object_ids = descriptors.iter().map(|d| d.id.clone()).collect();
        return Ok(BackupSelection {
            object_ids: Some(object_ids),
            descriptors,
            since,
            since_display,
            until,
            until_display,
            type_ids: type_filter.map(|f| f.manifest_type_ids),
        });
    }

    let descriptors = ctx
        .client
        .objects(&space.id)
        .limit(10_000)
        .list()
        .await?
        .collect_all()
        .await?
        .into_iter()
        .map(|obj| object_to_descriptor(&obj))
        .collect();

    Ok(BackupSelection {
        object_ids: None,
        descriptors,
        since: None,
        since_display: None,
        until: None,
        until_display: None,
        type_ids: None,
    })
}

async fn fetch_descriptors_by_ids(
    client: &AnytypeClient,
    space_id: &str,
    object_ids: &[String],
) -> Result<Vec<ObjectDescriptor>> {
    let mut descriptors = Vec::with_capacity(object_ids.len());
    for object_id in object_ids {
        let object = client
            .object(space_id, object_id)
            .get()
            .await
            .with_context(|| format!("failed to fetch object {object_id}"))?;
        descriptors.push(object_to_descriptor(&object));
    }
    Ok(descriptors)
}

fn parse_since(since: Option<&String>) -> Result<DateTime<FixedOffset>> {
    let since = since.ok_or_else(|| anyhow!("--since is required when --mode incremental"))?;
    let raw = since.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return Ok(parsed);
    }
    if let Some(utc_suffix) = raw
        .strip_suffix(" UTC")
        .or_else(|| raw.strip_suffix(" utc"))
        && let Some(naive) = parse_local_naive(utc_suffix.trim())
    {
        let utc = naive.and_utc();
        if let Some(offset) = FixedOffset::east_opt(0) {
            return Ok(utc.with_timezone(&offset));
        }
    }
    if let Some(utc_suffix) = raw.strip_suffix("+0").or_else(|| raw.strip_suffix("+00"))
        && let Some(naive) = parse_local_naive(utc_suffix.trim())
    {
        let utc = naive.and_utc();
        if let Some(offset) = FixedOffset::east_opt(0) {
            return Ok(utc.with_timezone(&offset));
        }
    }
    parse_local_since(raw).with_context(|| {
        format!(
            "invalid --since value: {since}. Expected RFC3339 with timezone/offset, or local/partial time without timezone (e.g. 2026-01-12T10:11:22, 2026-01-12, 2026-01, 2026)"
        )
    })
}

fn parse_local_since(value: &str) -> Result<DateTime<FixedOffset>> {
    let naive =
        parse_local_naive(value).ok_or_else(|| anyhow!("unable to parse local timestamp"))?;
    let local = Local
        .from_local_datetime(&naive)
        .single()
        .ok_or_else(|| anyhow!("ambiguous/non-existent local time due to timezone transition"))?;
    Ok(local.fixed_offset())
}

fn parse_local_naive(value: &str) -> Option<NaiveDateTime> {
    const FORMATS: &[&str] = &[
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ];
    for format in FORMATS {
        if let Ok(dt) = NaiveDateTime::parse_from_str(value, format) {
            return Some(dt);
        }
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .or_else(|| {
            NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
        })
        .or_else(|| {
            NaiveDate::parse_from_str(&format!("{value}-01-01"), "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
        })
}

fn to_rfc3339_with_offset(value: DateTime<FixedOffset>) -> String {
    if value.offset().local_minus_utc() == 0 {
        value
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Secs, true)
    } else {
        value.to_rfc3339_opts(SecondsFormat::Secs, false)
    }
}

fn format_since_display(value: DateTime<FixedOffset>) -> String {
    let tz = if value.offset().local_minus_utc() == 0 {
        "UTC".to_string()
    } else {
        value.offset().to_string()
    };
    format!("{} {}", value.format("%Y-%m-%d %H:%M:%S"), tz)
}

fn local_now_display() -> String {
    let now = Local::now();
    format!("{} {}", now.format("%Y-%m-%d %H:%M:%S"), now.format("%Z"))
}

async fn resolve_type_filter(
    ctx: &AppContext,
    space_id: &str,
    type_values: Option<&Vec<String>>,
) -> Result<Option<TypeFilter>> {
    let Some(values) = type_values else {
        return Ok(None);
    };
    let mut keys = BTreeSet::new();
    let mut manifest_type_ids = Vec::new();
    let mut manifest_seen = BTreeSet::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if looks_like_object_id(trimmed) {
            let typ = ctx
                .client
                .get_type(space_id, trimmed)
                .get()
                .await
                .with_context(|| format!("type not found for id '{trimmed}'"))?;
            keys.insert(typ.key.clone());
            if manifest_seen.insert(typ.id.clone()) {
                manifest_type_ids.push(typ.id);
            }
        } else {
            let typ = ctx
                .client
                .lookup_type_by_key(space_id, trimmed)
                .await
                .with_context(|| format!("type not found for key '{trimmed}'"))?;
            keys.insert(typ.key.clone());
            if manifest_seen.insert(typ.id.clone()) {
                manifest_type_ids.push(typ.id);
            }
        }
    }
    ensure!(
        !keys.is_empty(),
        "no valid type entries supplied to --types"
    );
    Ok(Some(TypeFilter {
        keys,
        manifest_type_ids,
    }))
}

fn descriptor_matches_type_filter(object: &ObjectDescriptor, filter: &TypeFilter) -> bool {
    object
        .r#type
        .as_ref()
        .is_some_and(|type_key| filter.keys.contains(type_key))
}

fn resolve_backup_target(args: &BackupCreateArgs, space_id: &str) -> Result<BackupTarget> {
    let zip = true;

    if let Some(dest) = args.dest.as_ref() {
        ensure!(
            !dest.exists(),
            "target archive path already exists: {}",
            dest.display()
        );
        let parent = dest
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        ensure!(
            parent.exists(),
            "parent directory for --dest does not exist: {}",
            parent.display()
        );
        ensure!(
            parent.is_dir(),
            "parent path for --dest is not a directory: {}",
            parent.display()
        );
        return Ok(BackupTarget {
            parent_dir: parent.to_path_buf(),
            archive_path: dest.clone(),
            zip,
        });
    }

    let parent_dir = args.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    ensure!(
        parent_dir.exists(),
        "output directory does not exist: {}",
        parent_dir.display()
    );
    ensure!(
        parent_dir.is_dir(),
        "output path is not a directory: {}",
        parent_dir.display()
    );

    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    let prefix = args.prefix.as_deref().unwrap_or("backup");
    let mut archive_name = format!("{}_{}_{}", sanitize_path_component(prefix), space_id, ts);
    if zip {
        archive_name.push_str(".zip");
    }
    let archive_path = parent_dir.join(archive_name);
    ensure!(
        !archive_path.exists(),
        "target archive path already exists: {}",
        archive_path.display()
    );
    Ok(BackupTarget {
        parent_dir,
        archive_path,
        zip,
    })
}

struct PreparedBackupPublication {
    source: PathBuf,
    staged_archive: PathBuf,
    archive_identity: FileIdentity,
    dest: PathBuf,
    sidecar: PathBuf,
    staged_sidecar: PathBuf,
    sidecar_identity: FileIdentity,
}

impl Drop for PreparedBackupPublication {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.source);
        let _ = fs::remove_file(&self.staged_archive);
        let _ = fs::remove_file(&self.staged_sidecar);
    }
}

fn prepare_backup_artifacts(
    source: PathBuf,
    dest: PathBuf,
    manifest: &Manifest,
) -> Result<PreparedBackupPublication> {
    prepare_backup_artifacts_with_hook(source, dest, manifest, |_| Ok(()))
}

fn prepare_backup_artifacts_with_hook(
    source: PathBuf,
    dest: PathBuf,
    manifest: &Manifest,
    after_archive_stage: impl FnOnce(&Path) -> Result<()>,
) -> Result<PreparedBackupPublication> {
    ensure!(
        source != dest,
        "backup staging path unexpectedly equals destination"
    );
    let mut source_file = fs::File::open(&source)
        .with_context(|| format!("failed to open staged archive {}", source.display()))?;
    let (mut archive_stage, archive_stage_path) = create_backup_staging_file(&dest, "archive")?;
    if let Err(error) =
        io::copy(&mut source_file, &mut archive_stage).and_then(|_| archive_stage.sync_all())
    {
        drop(archive_stage);
        let _ = fs::remove_file(&archive_stage_path);
        return Err(error).context("failed to copy and sync owned backup archive staging file");
    }
    if let Err(error) = after_archive_stage(&archive_stage_path) {
        drop(archive_stage);
        let _ = fs::remove_file(&archive_stage_path);
        return Err(error).context("backup archive staging barrier failed");
    }
    let (archive_size, archive_sha256) = match archive_binding_from_file(&mut archive_stage) {
        Ok(binding) => binding,
        Err(error) => {
            drop(archive_stage);
            let _ = fs::remove_file(&archive_stage_path);
            return Err(error).context("failed to bind staged backup archive handle");
        }
    };
    let archive_identity = match FileIdentity::from_file(archive_stage) {
        Ok(identity) => identity,
        Err(error) => {
            let _ = fs::remove_file(&archive_stage_path);
            return Err(error).context("failed to retain staged backup archive identity");
        }
    };
    let mut bound_manifest = manifest.clone();
    bound_manifest.archive_size = Some(archive_size);
    bound_manifest.archive_sha256 = Some(archive_sha256);
    let text = match serde_json::to_vec_pretty(&bound_manifest) {
        Ok(text) => text,
        Err(error) => {
            let _ = fs::remove_file(&archive_stage_path);
            return Err(error).context("failed to serialize bound backup manifest");
        }
    };

    let sidecar_path = manifest_sidecar_path(&dest);
    let (mut stage, stage_path) = match create_backup_staging_file(&sidecar_path, "manifest") {
        Ok(staging) => staging,
        Err(error) => {
            let _ = fs::remove_file(&archive_stage_path);
            return Err(error);
        }
    };
    if let Err(error) = stage.write_all(&text).and_then(|()| stage.sync_all()) {
        drop(stage);
        let _ = fs::remove_file(&archive_stage_path);
        let _ = fs::remove_file(&stage_path);
        return Err(error).context("failed to sync staged backup manifest");
    }
    let sidecar_identity = match FileIdentity::from_file(stage) {
        Ok(identity) => identity,
        Err(error) => {
            let _ = fs::remove_file(&archive_stage_path);
            let _ = fs::remove_file(&stage_path);
            return Err(error).context("failed to retain staged backup manifest identity");
        }
    };

    Ok(PreparedBackupPublication {
        source,
        staged_archive: archive_stage_path,
        archive_identity,
        dest,
        sidecar: sidecar_path,
        staged_sidecar: stage_path,
        sidecar_identity,
    })
}

fn commit_backup_artifacts(
    prepared: PreparedBackupPublication,
    authority: deadline::PublicationCommit,
) -> Result<()> {
    commit_backup_artifacts_with_hook(prepared, authority, || Ok(()))
}

fn commit_backup_artifacts_with_hook(
    mut prepared: PreparedBackupPublication,
    authority: deadline::PublicationCommit,
    after_manifest_claim: impl FnOnce() -> Result<()>,
) -> Result<()> {
    authority.commit(|| {
        ensure!(
            !prepared.dest.exists(),
            "backup archive destination already exists: {}",
            prepared.dest.display()
        );
        claim_owned_staging_file(
            &prepared.staged_sidecar,
            &prepared.sidecar,
            &prepared.sidecar_identity,
        )
        .with_context(|| {
            format!(
                "failed to publish backup manifest {} without overwriting an existing destination",
                prepared.sidecar.display()
            )
        })?;

        let finish = after_manifest_claim()
            .and_then(|()| ensure_file_owned(&prepared.sidecar, &prepared.sidecar_identity))
            .and_then(|()| {
                claim_owned_staging_file(
                    &prepared.staged_archive,
                    &prepared.dest,
                    &prepared.archive_identity,
                )
                .with_context(|| {
                    format!(
                        "failed to publish backup archive {} without overwriting an existing destination",
                        prepared.dest.display()
                    )
                })
            });
        if let Err(error) = finish {
            remove_file_if_owned(&prepared.sidecar, &prepared.sidecar_identity).with_context(|| {
                format!(
                    "backup publication failed ({error}); refused to remove a manifest path no longer owned by this publication"
                )
            })?;
            return Err(error);
        }

        fs::remove_file(&prepared.source).context("failed to remove original archive staging file")?;
        fs::remove_file(&prepared.staged_archive)
            .context("failed to remove owned archive staging file")?;
        fs::remove_file(&prepared.staged_sidecar)
            .context("failed to remove owned manifest staging file")?;
        prepared.source.clear();
        prepared.staged_archive.clear();
        prepared.staged_sidecar.clear();
        Ok(())
    })
}

fn claim_owned_staging_file(
    staging: &Path,
    destination: &Path,
    identity: &FileIdentity,
) -> Result<()> {
    ensure_file_owned(staging, identity).context("staging identity changed before publication")?;
    fs::hard_link(staging, destination)?;
    ensure_file_owned(destination, identity)
        .context("published file identity does not match its owned staging file")?;
    Ok(())
}

fn ensure_file_owned(path: &Path, identity: &FileIdentity) -> Result<()> {
    let current = FileIdentity::from_path(path).context("failed to inspect file identity")?;
    ensure!(&current == identity, "file identity changed");
    Ok(())
}

fn remove_file_if_owned(path: &Path, identity: &FileIdentity) -> Result<()> {
    let current = match FileIdentity::from_path(path) {
        Ok(current) => current,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("failed to inspect publication rollback target"),
    };
    ensure!(
        &current == identity,
        "publication rollback target identity changed"
    );
    fs::remove_file(path).context("failed to remove owned publication path")
}

fn create_backup_staging_file(destination: &Path, purpose: &str) -> Result<(fs::File, PathBuf)> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let name = destination
        .file_name()
        .ok_or_else(|| anyhow!("backup manifest destination must name a file"))?;
    for nonce in 0..100_u32 {
        let path = parent.join(format!(
            ".{}.anyback-stage-{}-{nonce}",
            name.to_string_lossy(),
            std::process::id()
        ));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create backup {purpose} staging file"));
            }
        }
    }
    bail!("failed to allocate backup {purpose} staging file")
}

async fn handle_restore_apply(ctx: &WorkflowContext, args: RestoreApplyArgs) -> Result<()> {
    ctx.deadline.ensure_restore_preflight_remaining()?;
    let progress = ProgressReporter::new(&ctx.output, "Starting restore");
    let (cancel_sender, mut cancel_state) = new_import_cancel_channel();
    let signal_forwarder = spawn_import_cancel_signal_forwarder(cancel_sender);
    let result = async {
        let archive = args.archive.as_path();
        let space_name_or_id = args
            .space
            .as_deref()
            .ok_or_else(|| anyhow!("--space is required"))?;
        let space = ctx
            .deadline
            .run_restore_preflight(resolve_space(&ctx.client, space_name_or_id))
            .await??;
        progress.set_message("Resolved destination space");
        let archive_owned = archive.to_path_buf();
        let objects_owned = args.objects.clone();
        let plan = ctx
            .deadline
            .run_restore_preflight(tokio::task::spawn_blocking(move || {
                build_import_plan(&archive_owned, objects_owned.as_deref())
            }))
            .await?
            .context("restore preflight worker failed")??;
        if args.dry_run {
            progress.finish("Restore preflight completed");
            return publish_restore_dry_run(ctx, archive, &space.id, &plan).await;
        }
        progress.set_message("Importing archive");
        let mut report = init_import_report(archive, &space.id, &plan.selected_ids);
        let execution = execute_object_import(
            ctx,
            &space.id,
            &plan.import_path,
            args.objects.is_some(),
            &plan.selected_ids,
            args.import_mode,
            args.replace,
            progress.enabled(),
            &mut cancel_state,
        )
        .await?;
        ctx.deadline.ensure_mutation_remaining()?;
        let response = aggregate_import_responses(&execution.responses);
        report.event_progress = execution.event_progress;
        apply_import_response(
            &mut report,
            response,
            &plan.selected_ids,
            plan.manifest.as_ref(),
        );
        progress.finish("Restore completed");
        ctx.deadline.ensure_mutation_remaining()?;
        if let Some(path) = args.log.clone() {
            let report_for_file = report.clone();
            ctx.deadline
                .run_mutation_publication(
                    move || prepare_report(&report_for_file, &path),
                    CommandOutput::commit_prepared,
                )
                .await?;
        }
        log_report_summary(&report);
        publish_restore_result(ctx, report).await?;
        Ok(())
    }
    .await;
    signal_forwarder.abort();
    result
}

async fn publish_restore_dry_run(
    ctx: &WorkflowContext,
    archive: &Path,
    space_id: &str,
    plan: &ImportPlan,
) -> Result<()> {
    let archive = archive.to_path_buf();
    let space_id = space_id.to_string();
    let requested = plan.selected_ids.len();
    let manifest_present = plan.manifest.is_some();
    let payload = serde_json::json!({
        "dry_run": true,
        "archive": archive.clone(),
        "space_id": space_id.clone(),
        "requested": requested,
        "manifest_present": manifest_present,
    });
    let output = ctx.output.clone();
    ctx.deadline
        .run_read_publication(
            "restore workflow timed out before mutation dispatch",
            move || {
                let Some(rendered) = output.render(&payload, || {
                    format!(
                        "dry-run ok archive={} space={} requested={} manifest={}",
                        archive.display(),
                        space_id,
                        requested,
                        if manifest_present {
                            "present"
                        } else {
                            "missing"
                        }
                    )
                })?
                else {
                    return Ok(output::PreparedOutput::Quiet);
                };
                output.prepare_rendered(rendered)
            },
            CommandOutput::commit_prepared,
        )
        .await
}

async fn publish_restore_result(ctx: &WorkflowContext, report: ImportReport) -> Result<()> {
    let output = ctx.output.clone();
    ctx.deadline
        .run_mutation_publication(
            move || {
                let Some(rendered) = output.render(&report, || render_report_summary(&report))?
                else {
                    return Ok(output::PreparedOutput::Quiet);
                };
                output.prepare_rendered(rendered)
            },
            CommandOutput::commit_prepared,
        )
        .await
}

struct ImportPlan {
    manifest: Option<Manifest>,
    selected_ids: Vec<String>,
    import_path: PathBuf,
}

#[derive(Debug, Clone)]
#[cfg(feature = "snapshot-import")]
struct ImportSnapshotEntry {
    path: String,
    id: String,
    sb_type: i32,
    snapshot: import_request::Snapshot,
    encoded_bytes: usize,
}

#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Copy)]
#[cfg(feature = "snapshot-import")]
struct ImportChunkLimits {
    max_single_snapshot_bytes: usize,
    max_batch_bytes: usize,
    max_batch_snapshots: usize,
}

fn build_import_plan(archive: &Path, objects_spec: Option<&str>) -> Result<ImportPlan> {
    let manifest = read_manifest_from_archive(archive)?;
    let selected_ids = if let Some(spec) = objects_spec {
        let ids = load_object_ids_spec(spec)?;
        ensure!(!ids.is_empty(), "no object ids supplied to --objects");
        ids
    } else {
        infer_object_ids_from_archive(archive).unwrap_or_default()
    };

    Ok(ImportPlan {
        manifest,
        selected_ids,
        import_path: archive.to_path_buf(),
    })
}

fn infer_object_ids_from_archive(archive: &Path) -> Result<Vec<String>> {
    let reader = ArchiveReader::from_path(archive)?;
    let files = reader.list_files()?;
    Ok(infer_object_ids_from_files(&files))
}

fn init_import_report(archive: &Path, space_id: &str, selected_ids: &[String]) -> ImportReport {
    ImportReport {
        archive: archive.display().to_string(),
        space_id: space_id.to_string(),
        attempted: selected_ids.len(),
        imported: 0,
        failed: 0,
        success: Vec::new(),
        errors: Vec::new(),
        summary: Vec::new(),
        event_progress: None,
    }
}

#[derive(Debug)]
struct ImportExecutionOutcome {
    responses: Vec<anytype_rpc::anytype::rpc::object::import::Response>,
    event_progress: Option<ImportEventProgressReport>,
}

fn process_progress_to_report(progress: ProcessWatchProgress) -> ImportEventProgressReport {
    ImportEventProgressReport {
        processes_started: progress.processes_started,
        processes_done: progress.processes_done,
        process_updates: progress.process_updates,
        import_finish_events: progress.import_finish_events,
        import_finish_objects: progress.import_finish_objects,
        last_process_id: progress.last_process_id,
        last_process_state: progress.last_process_state,
        last_progress_done: progress.last_progress_done,
        last_progress_total: progress.last_progress_total,
        last_progress_message: progress.last_progress_message,
        last_process_error: progress.last_process_error,
    }
}

#[cfg(feature = "tui")]
fn parse_cache_size(raw: &str) -> Result<usize> {
    let input = raw.trim();
    ensure!(!input.is_empty(), "cache size must not be empty");

    let split = input
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(input.len());
    let (digits, unit_raw) = input.split_at(split);
    ensure!(!digits.is_empty(), "cache size must start with a number");
    let value = digits.parse::<u64>()?;
    ensure!(value > 0, "cache size must be > 0");
    let unit = unit_raw.trim().to_ascii_lowercase();

    let multiplier = match unit.as_str() {
        "" | "m" | "mb" => 1024_u64 * 1024_u64,
        "k" | "kb" => 1024_u64,
        "g" | "gb" => 1024_u64 * 1024_u64 * 1024_u64,
        _ => bail!("unsupported cache size unit: {unit_raw}"),
    };

    let bytes = value
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow!("cache size is too large"))?;
    usize::try_from(bytes).context("cache size exceeds platform limits")
}

#[cfg(feature = "snapshot-import")]
fn parse_import_limit_env(name: &str, default: usize) -> Result<usize> {
    match std::env::var(name) {
        Ok(raw) => {
            let value = raw
                .parse::<usize>()
                .with_context(|| format!("invalid {name} value: {raw}"))?;
            ensure!(value > 0, "{name} must be > 0");
            Ok(value)
        }
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(err) => Err(anyhow!("failed to read {name}: {err}")),
    }
}

#[cfg(feature = "snapshot-import")]
fn import_chunk_limits_from_env() -> Result<ImportChunkLimits> {
    let max_single_snapshot_bytes = parse_import_limit_env(
        "ANYBACK_IMPORT_MAX_SINGLE_SNAPSHOT_BYTES",
        DEFAULT_IMPORT_MAX_SINGLE_SNAPSHOT_BYTES,
    )?;
    let max_batch_bytes = parse_import_limit_env(
        "ANYBACK_IMPORT_MAX_BATCH_BYTES",
        DEFAULT_IMPORT_MAX_BATCH_BYTES,
    )?;
    let max_batch_snapshots = parse_import_limit_env(
        "ANYBACK_IMPORT_MAX_BATCH_SNAPSHOTS",
        DEFAULT_IMPORT_MAX_BATCH_SNAPSHOTS,
    )?;
    ensure!(
        max_batch_bytes >= max_single_snapshot_bytes,
        "ANYBACK_IMPORT_MAX_BATCH_BYTES ({max_batch_bytes}) must be >= ANYBACK_IMPORT_MAX_SINGLE_SNAPSHOT_BYTES ({max_single_snapshot_bytes})"
    );
    Ok(ImportChunkLimits {
        max_single_snapshot_bytes,
        max_batch_bytes,
        max_batch_snapshots,
    })
}

#[cfg(feature = "snapshot-import")]
fn snapshot_id_from_data(data: &anytype_rpc::model::SmartBlockSnapshotBase) -> Option<String> {
    let details = data.details.as_ref()?;
    let value = details.fields.get("id")?;
    let kind = value.kind.as_ref()?;
    match kind {
        prost_types::value::Kind::StringValue(text) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

#[cfg(feature = "snapshot-import")]
fn parse_import_snapshot_entry(path: &str, bytes: &[u8]) -> Result<ImportSnapshotEntry> {
    let snapshot = SnapshotWithType::decode(bytes)
        .with_context(|| format!("failed to decode protobuf snapshot: {path}"))?;
    let sb_type = snapshot.sb_type;
    let data = snapshot
        .snapshot
        .and_then(|s| s.data)
        .ok_or_else(|| anyhow!("snapshot payload missing data: {path}"))?;
    let id = snapshot_id_from_data(&data)
        .or_else(|| infer_object_id_from_snapshot_path(path))
        .ok_or_else(|| anyhow!("snapshot object id missing or unreadable: {path}"))?;
    let request_snapshot = import_request::Snapshot {
        id: id.clone(),
        snapshot: Some(data),
    };
    let encoded_bytes = request_snapshot.encoded_len();
    Ok(ImportSnapshotEntry {
        path: path.to_string(),
        id,
        sb_type,
        snapshot: request_snapshot,
        encoded_bytes,
    })
}

#[cfg(feature = "snapshot-import")]
fn is_required_support_object_type(sb_type: i32) -> bool {
    use anytype_rpc::model::SmartBlockType;
    matches!(
        SmartBlockType::try_from(sb_type).ok(),
        Some(SmartBlockType::Workspace | SmartBlockType::Widget | SmartBlockType::SpaceView)
    )
}

#[allow(clippy::case_sensitive_file_extension_comparisons)]
#[cfg(feature = "snapshot-import")]
fn collect_import_snapshots(
    import_path: &Path,
    selected_ids: &[String],
) -> Result<Vec<ImportSnapshotEntry>> {
    let reader = ArchiveReader::from_path(import_path)?;
    let files = reader.list_files()?;
    let mut snapshots = Vec::new();
    let selected: std::collections::HashSet<&str> =
        selected_ids.iter().map(String::as_str).collect();
    let selective = !selected.is_empty();
    let mut matched_selected = 0usize;

    for file in files {
        let lower = file.path.to_ascii_lowercase();
        if lower.ends_with(".pb.json") {
            bail!(
                "snapshot transport does not support pb-json yet: {}. Re-run backup with --format pb.",
                file.path
            );
        }
        if !lower.ends_with(".pb") {
            continue;
        }
        let bytes = reader.read_bytes(&file.path)?;
        let parsed = parse_import_snapshot_entry(&file.path, &bytes)?;
        let is_object_snapshot = file.path.starts_with("objects/");
        if selective && is_object_snapshot {
            let keep = selected.contains(parsed.id.as_str())
                || is_required_support_object_type(parsed.sb_type);
            if !keep {
                continue;
            }
            if selected.contains(parsed.id.as_str()) {
                matched_selected = matched_selected.saturating_add(1);
            }
        }
        snapshots.push(parsed);
    }
    ensure!(
        !snapshots.is_empty(),
        "archive contains no protobuf snapshot files (*.pb)"
    );
    if selective {
        ensure!(
            matched_selected > 0,
            "none of the requested object ids were found in archive snapshots"
        );
    }
    Ok(snapshots)
}

#[cfg(feature = "snapshot-import")]
fn plan_snapshot_batches(
    snapshots: &[ImportSnapshotEntry],
    limits: ImportChunkLimits,
) -> Result<Vec<Vec<import_request::Snapshot>>> {
    let mut batches = Vec::<Vec<import_request::Snapshot>>::new();
    let mut current = Vec::<import_request::Snapshot>::new();
    let mut current_bytes = 0usize;

    for entry in snapshots {
        ensure!(
            entry.encoded_bytes <= limits.max_single_snapshot_bytes,
            "snapshot {} ({}) is too large: {} bytes (max {})",
            entry.id,
            entry.path,
            entry.encoded_bytes,
            limits.max_single_snapshot_bytes
        );

        let would_exceed_count = current.len() >= limits.max_batch_snapshots;
        let would_exceed_bytes =
            !current.is_empty() && current_bytes + entry.encoded_bytes > limits.max_batch_bytes;
        if would_exceed_count || would_exceed_bytes {
            batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }

        current_bytes += entry.encoded_bytes;
        current.push(entry.snapshot.clone());
    }

    if !current.is_empty() {
        batches.push(current);
    }
    Ok(batches)
}

fn aggregate_import_responses(
    responses: &[anytype_rpc::anytype::rpc::object::import::Response],
) -> anytype_rpc::anytype::rpc::object::import::Response {
    let mut objects_count = 0i64;
    let mut first_error: Option<anytype_rpc::anytype::rpc::object::import::response::Error> = None;
    for response in responses {
        objects_count = objects_count.saturating_add(response.objects_count.max(0));
        if first_error.is_none() {
            first_error = response.error.clone().filter(|error| error.code != 0);
        }
    }

    anytype_rpc::anytype::rpc::object::import::Response {
        error: first_error,
        collection_id: String::new(),
        objects_count,
    }
}

fn import_error_hint(error_code: i64) -> Option<&'static str> {
    match error_code {
        5 => Some("no objects detected in import source"),
        6 => Some("import was canceled"),
        7 => Some("CSV rows/relations limit exceeded"),
        8 => Some("file load/read error"),
        9 => Some("insufficient permissions for import destination"),
        10 => Some("unsupported/invalid HTML structure"),
        11 => Some("protobuf archive is not valid Anyblock format"),
        12 => Some("import source service is unavailable"),
        13 => Some("import source rate limit exceeded"),
        14 => Some("zip archive contains no importable objects"),
        17 => Some("directory contains no importable objects"),
        _ => None,
    }
}

fn format_import_api_error(description: &str, error_code: i64) -> String {
    import_error_hint(error_code).map_or_else(
        || format!("{description} (code {error_code})"),
        |hint| format!("{description} (code {error_code}; hint: {hint})"),
    )
}

#[cfg(feature = "snapshot-import")]
async fn execute_object_import_batches(
    ctx: &WorkflowContext,
    space_id: &str,
    batches: Vec<Vec<import_request::Snapshot>>,
    import_mode: ImportModeArg,
    replace_existing: bool,
    interactive_output: bool,
    cancel_state: &mut ImportCancelState,
) -> Result<ImportExecutionOutcome> {
    let grpc = ctx
        .deadline
        .run_restore_preflight(ctx.client.grpc_client())
        .await??;
    let mut commands = grpc.client_commands();
    let timeouts = ctx.deadline.process_timeouts()?;
    let mut tracker = ctx
        .deadline
        .run_restore_preflight(ProcessWatcher::subscribe(&grpc, timeouts))
        .await??;
    let watch_request = import_watch_request(space_id, interactive_output);
    let import_result: Result<_> = async {
        let mut responses = Vec::with_capacity(batches.len());
        for batch in batches {
            ctx.deadline.ensure_restore_preflight_remaining()?;
            let generation = tracker.begin_generation().context(
                "failed to establish import process event generation before mutation dispatch",
            )?;
            ctx.deadline.ensure_restore_preflight_remaining()?;
            let request = ObjectImportRequest {
                space_id: space_id.to_string(),
                snapshots: batch,
                update_existing_objects: replace_existing,
                r#type: anytype_rpc::model::r#import::Type::External as i32,
                mode: import_mode.to_rpc_mode(),
                no_progress: false,
                is_migration: false,
                is_new_space: false,
                params: None,
            };

            let request = with_token(tonic::Request::new(request), grpc.token())
                .map_err(|err| anyhow!("failed to attach gRPC token: {err}"))?;

            let response = ctx
                .deadline
                .run_mutation(commands.object_import(request))
                .await?
                .context("object import RPC failed; mutation outcome is indeterminate")
                .map(tonic::Response::into_inner)?;
            let correlation = tracker
                .correlate_generation(generation, &response.collection_id)
                .context(
                    "import response could not be correlated to process completion; mutation outcome is indeterminate",
                )?;
            ctx.deadline
                .run_mutation(tracker.wait_for_generation(
                    &grpc,
                    &watch_request,
                    correlation,
                    Some(cancel_state.receiver_mut()),
                ))
                .await?
                .context(
                    "import process completion failed; mutation outcome is indeterminate",
                )?;
            responses.push(response);
        }
        Ok(ImportExecutionOutcome {
            responses,
            event_progress: None,
        })
    }
    .await;

    let unsubscribe_result = tracker.unsubscribe(&grpc).await;
    if let Err(err) = unsubscribe_result {
        if import_result.is_ok() {
            return Err(err.into());
        }
        warn!("failed to unsubscribe process events after restore error: {err:#}");
    }

    let mut outcome = import_result?;
    outcome.event_progress = Some(process_progress_to_report(tracker.into_progress()));
    Ok(outcome)
}

async fn execute_object_import_path(
    ctx: &WorkflowContext,
    space_id: &str,
    archive_path: &Path,
    import_mode: ImportModeArg,
    replace_existing: bool,
    interactive_output: bool,
    cancel_state: &mut ImportCancelState,
) -> Result<ImportExecutionOutcome> {
    let import_paths = pb_import_paths(archive_path)?;
    let grpc = ctx
        .deadline
        .run_restore_preflight(ctx.client.grpc_client())
        .await??;
    let mut commands = grpc.client_commands();
    let timeouts = ctx.deadline.process_timeouts()?;
    let mut tracker = ctx
        .deadline
        .run_restore_preflight(ProcessWatcher::subscribe(&grpc, timeouts))
        .await??;
    let watch_request = import_watch_request(space_id, interactive_output);
    let request = ObjectImportRequest {
        space_id: space_id.to_string(),
        snapshots: Vec::new(),
        update_existing_objects: replace_existing,
        r#type: anytype_rpc::model::r#import::Type::Pb as i32,
        mode: import_mode.to_rpc_mode(),
        no_progress: false,
        is_migration: false,
        is_new_space: false,
        params: Some(import_request::Params::PbParams(import_request::PbParams {
            path: import_paths,
            no_collection: false,
            collection_title: String::new(),
            import_type: import_request::pb_params::Type::Space as i32,
        })),
    };
    let import_result: Result<_> = async {
        ctx.deadline.ensure_restore_preflight_remaining()?;
        let generation = tracker.begin_generation().context(
            "failed to establish import process event generation before mutation dispatch",
        )?;
        ctx.deadline.ensure_restore_preflight_remaining()?;
        let request = with_token(tonic::Request::new(request), grpc.token())
            .map_err(|err| anyhow!("failed to attach gRPC token: {err}"))?;
        let response = ctx
            .deadline
            .run_mutation(commands.object_import(request))
            .await?
            .context("object import RPC failed; mutation outcome is indeterminate")
            .map(tonic::Response::into_inner)?;
        let correlation = tracker
            .correlate_generation(generation, &response.collection_id)
            .context(
                "import response could not be correlated to process completion; mutation outcome is indeterminate",
            )?;
        ctx.deadline
            .run_mutation(tracker.wait_for_generation(
                &grpc,
                &watch_request,
                correlation,
                Some(cancel_state.receiver_mut()),
            ))
            .await?
            .context(
                "import process completion failed; mutation outcome is indeterminate",
            )?;
        Ok(ImportExecutionOutcome {
            responses: vec![response],
            event_progress: None,
        })
    }
    .await;

    let unsubscribe_result = tracker.unsubscribe(&grpc).await;
    if let Err(err) = unsubscribe_result {
        if import_result.is_ok() {
            return Err(err.into());
        }
        warn!("failed to unsubscribe process events after restore error: {err:#}");
    }

    let mut outcome = import_result?;
    outcome.event_progress = Some(process_progress_to_report(tracker.into_progress()));
    Ok(outcome)
}

fn import_watch_request(space_id: &str, interactive_output: bool) -> ProcessWatchRequest {
    ProcessWatchRequest::new(ProcessKind::Import, space_id)
        .allow_empty_space_id(true)
        .completion_fallback(ProcessCompletionFallback::ImportFinishEvent)
        .cancel_message(IMPORT_CANCEL_REASON)
        .log_progress(interactive_output)
}

#[allow(clippy::too_many_arguments)]
async fn execute_object_import(
    ctx: &WorkflowContext,
    space_id: &str,
    archive_path: &Path,
    explicit_object_selection: bool,
    #[allow(unused_variables)] selected_ids: &[String],
    import_mode: ImportModeArg,
    replace_existing: bool,
    interactive_output: bool,
    cancel_state: &mut ImportCancelState,
) -> Result<ImportExecutionOutcome> {
    #[cfg(feature = "snapshot-import")]
    if explicit_object_selection {
        let limits = import_chunk_limits_from_env()?;
        let snapshots = collect_import_snapshots(archive_path, selected_ids)?;
        let batches = plan_snapshot_batches(&snapshots, limits)?;
        return execute_object_import_batches(
            ctx,
            space_id,
            batches,
            import_mode,
            replace_existing,
            interactive_output,
            cancel_state,
        )
        .await;
    }

    #[cfg(not(feature = "snapshot-import"))]
    if explicit_object_selection {
        bail!(
            "--objects restore requires snapshot transport; rebuild anyback with --features snapshot-import"
        );
    }

    execute_object_import_path(
        ctx,
        space_id,
        archive_path,
        import_mode,
        replace_existing,
        interactive_output,
        cancel_state,
    )
    .await
}

fn pb_import_paths(archive_path: &Path) -> Result<Vec<String>> {
    if !archive_path.is_dir() {
        return Ok(vec![archive_path.to_string_lossy().to_string()]);
    }
    if std::env::var("ANYBACK_PB_IMPORT_ROOT_ONLY")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        return Ok(vec![archive_path.to_string_lossy().to_string()]);
    }

    let mut paths = Vec::new();
    let include_files_dir = std::env::var("ANYBACK_PB_IMPORT_INCLUDE_FILES_DIR")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    for entry in fs::read_dir(archive_path).with_context(|| {
        format!(
            "failed to read archive directory {}",
            archive_path.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();

        if path.is_dir() {
            if include_files_dir && file_name.eq_ignore_ascii_case("files") {
                paths.push(path.to_string_lossy().to_string());
                continue;
            }
            if dir_contains_pb_or_json(&path)? {
                paths.push(path.to_string_lossy().to_string());
            }
            continue;
        }

        if file_name == MANIFEST_NAME {
            continue;
        }

        let keep_file = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "pb" | "json"));
        if keep_file || file_name == "config.json" {
            paths.push(path.to_string_lossy().to_string());
        }
    }

    if paths.is_empty() {
        paths.push(archive_path.to_string_lossy().to_string());
    }
    paths.sort_unstable();
    Ok(paths)
}

fn dir_contains_pb_or_json(dir: &Path) -> Result<bool> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current)
            .with_context(|| format!("failed to read directory {}", current.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "pb" | "json"))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn apply_import_response(
    report: &mut ImportReport,
    response: anytype_rpc::anytype::rpc::object::import::Response,
    selected_ids: &[String],
    manifest: Option<&Manifest>,
) {
    let imported_count = usize::try_from(response.objects_count.max(0)).unwrap_or(0);
    let selected_descriptors = descriptors_from_selection(selected_ids, manifest);
    let api_error = response.error.filter(|error| error.code != 0);

    if let Some(error) = api_error {
        let message = format_import_api_error(&error.description, i64::from(error.code));
        report.imported = imported_count;
        report.errors = selected_descriptors
            .into_iter()
            .map(|descriptor| ObjectImportError {
                id: descriptor.id,
                name: descriptor.name,
                r#type: descriptor.r#type,
                last_modified: descriptor.last_modified,
                error_code: "import_api_error".to_string(),
                message: message.clone(),
                status: "partial".to_string(),
            })
            .collect();
        report.failed = report.errors.len();
        report
            .summary
            .push(format!("import API reported error: {message}"));
        report.summary.push(
            "best-effort mode: partial import may have succeeded; object-id mapping unavailable"
                .to_string(),
        );
    } else if !selected_descriptors.is_empty() {
        report.success = selected_descriptors;
        report.imported = report.success.len();
        report.summary.push(
            "per-object new ids are not available from import API in v0.1; success list uses source ids"
                .to_string(),
        );
    } else if let Some(manifest) = manifest {
        report.success.clone_from(&manifest.objects);
        report.imported = report.success.len();
        report.attempted = report.imported;
        report.summary.push(
            "import completed from full manifest; per-object new id mapping unavailable"
                .to_string(),
        );
    } else {
        report.imported = imported_count;
        report.summary.push(
            "import completed, but per-object details are unavailable without --objects or manifest"
                .to_string(),
        );
    }

    if report.attempted == 0 {
        report.attempted = report.imported.saturating_add(report.failed);
    }
    if report.failed > 0 {
        report.summary.push(format!(
            "imported {}/{} objects, {} failed",
            report.imported, report.attempted, report.failed
        ));
    } else {
        report.summary.push(format!(
            "imported {}/{} objects",
            report.imported, report.attempted
        ));
    }
    if let Some(events) = report.event_progress.as_ref() {
        report.summary.push(format!(
            "event progress: processes started={} done={} updates={} importFinish={} ({})",
            events.processes_started,
            events.processes_done,
            events.process_updates,
            events.import_finish_events,
            events.import_finish_objects
        ));
        if let (Some(id), Some(state)) = (&events.last_process_id, &events.last_process_state) {
            report
                .summary
                .push(format!("event completion: process {id} state {state}"));
        }
    }
}

fn handle_diff(output: &CommandOutput, args: &DiffArgs) -> Result<()> {
    let (format1, objects1) = collect_cmp_objects(&args.archive1)?;
    let (format2, objects2) = collect_cmp_objects(&args.archive2)?;

    ensure!(
        format1 != "mixed",
        "archive has mixed snapshot formats: {}",
        args.archive1.display()
    );
    ensure!(
        format2 != "mixed",
        "archive has mixed snapshot formats: {}",
        args.archive2.display()
    );
    ensure!(
        format1 != "unknown",
        "no comparable objects found in {}",
        args.archive1.display()
    );
    ensure!(
        format2 != "unknown",
        "no comparable objects found in {}",
        args.archive2.display()
    );
    ensure!(
        format1 == format2
            || matches!(
                (format1.as_str(), format2.as_str()),
                ("pb", "pb-json") | ("pb-json", "pb")
            ),
        "archive formats are not comparable: {} ({}) vs {} ({})",
        args.archive1.display(),
        format1,
        args.archive2.display(),
        format2
    );

    let report = build_archive_cmp_report(
        &args.archive1.display().to_string(),
        &args.archive2.display().to_string(),
        &format1,
        &format2,
        &objects1,
        &objects2,
    );

    output.emit(&report, || {
        let archive1_label = archive_basename(&args.archive1);
        let archive2_label = archive_basename(&args.archive2);
        let mut text = TextBuilder::new();

        text.line(format!("< {archive1_label} only"));
        for row in &report.archive1_only {
            text.line(format!(
                "< {} {} {} {} {}",
                row.object_id, row.r#type, row.name, row.size, row.last_modified
            ));
        }
        text.blank();
        text.line(format!("> {archive2_label} only"));
        for row in &report.archive2_only {
            text.line(format!(
                "> {} {} {} {} {}",
                row.object_id, row.r#type, row.name, row.size, row.last_modified
            ));
        }
        text.blank();
        text.line("* Changed");
        for row in &report.changed {
            text.line(format!(
                "< {} {} {} {} {}",
                row.left.object_id,
                row.left.r#type,
                row.left.name,
                row.left.size,
                row.left.last_modified
            ));
            text.line(format!(
                "> {} {} {} {} {}",
                row.right.object_id,
                row.right.r#type,
                row.right.name,
                row.right.size,
                row.right.last_modified
            ));
        }
        text.finish()
    })
}

fn archive_basename(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.display().to_string(), ToString::to_string)
}

#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn collect_cmp_objects(
    archive: &Path,
) -> Result<(String, std::collections::BTreeMap<String, ArchiveCmpObject>)> {
    let reader = ArchiveReader::from_path(archive)?;
    let files = reader.list_files()?;
    let mut format = "unknown".to_string();
    let mut seen_formats = BTreeSet::new();
    let mut out = std::collections::BTreeMap::<String, ArchiveCmpObject>::new();

    for file in &files {
        let lower = file.path.to_ascii_lowercase();
        let is_pb_json = lower.ends_with(".pb.json");
        let is_pb = lower.ends_with(".pb");
        if !is_pb && !is_pb_json {
            continue;
        }

        let path = Path::new(&file.path);
        let under_objects = path
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .is_some_and(|root| root == "objects");
        if !under_objects {
            continue;
        }

        seen_formats.insert(if is_pb_json { "pb-json" } else { "pb" });
        let bytes = reader.read_bytes(&file.path)?;
        let parsed = if is_pb_json {
            parse_snapshot_details_from_pb_json(&bytes)
        } else {
            parse_snapshot_details_from_pb(&bytes)
        };
        let Ok((_sb_type, details)) = parsed else {
            continue;
        };
        let id = detail_value(&details, "id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| infer_object_id_from_snapshot_path(&file.path));
        let Some(object_id) = id else {
            continue;
        };

        let type_value = detail_value(&details, "type")
            .cloned()
            .unwrap_or(Value::Null);
        let type_text = cmp_value_to_text(&type_value);
        let name = detail_value(&details, "name")
            .and_then(Value::as_str)
            .map_or_else(|| "-".to_string(), ToString::to_string);
        let last_modified = format_last_modified(detail_value(&details, "lastModifiedDate"))
            .unwrap_or_else(|| "-".to_string());

        out.insert(
            object_id.clone(),
            ArchiveCmpObject {
                object_id,
                r#type: type_text,
                name,
                size: file.bytes,
                last_modified,
            },
        );
    }

    if seen_formats.len() == 1 {
        format = seen_formats
            .iter()
            .next()
            .map_or_else(|| "unknown".to_string(), |s| (*s).to_string());
    } else if seen_formats.len() > 1 {
        format = "mixed".to_string();
    }

    Ok((format, out))
}

fn build_archive_cmp_report(
    archive1: &str,
    archive2: &str,
    format1: &str,
    format2: &str,
    objects1: &std::collections::BTreeMap<String, ArchiveCmpObject>,
    objects2: &std::collections::BTreeMap<String, ArchiveCmpObject>,
) -> ArchiveCmpReport {
    let mut archive1_only = Vec::new();
    let mut archive2_only = Vec::new();
    let mut changed = Vec::new();

    let ids: BTreeSet<String> = objects1
        .keys()
        .chain(objects2.keys())
        .map(ToString::to_string)
        .collect();

    for id in ids {
        match (objects1.get(&id), objects2.get(&id)) {
            (Some(left), Some(right)) => {
                if left != right {
                    changed.push(ArchiveCmpChanged {
                        left: left.clone(),
                        right: right.clone(),
                    });
                }
            }
            (Some(left), None) => archive1_only.push(left.clone()),
            (None, Some(right)) => archive2_only.push(right.clone()),
            (None, None) => {}
        }
    }

    ArchiveCmpReport {
        archive1: archive1.to_string(),
        archive2: archive2.to_string(),
        format1: format1.to_string(),
        format2: format2.to_string(),
        archive1_only,
        archive2_only,
        changed,
    }
}

fn cmp_value_to_text(value: &Value) -> String {
    match value {
        Value::Null => "-".to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => value.to_string(),
    }
}

fn handle_list(output: &CommandOutput, args: &ListArgs) -> Result<()> {
    let reader = ArchiveReader::from_path(&args.archive)?;
    let source = reader.source();
    let files = reader.list_files()?;
    let (manifest, manifest_error) = read_manifest_prefer_sidecar(&args.archive, &reader);
    let total_bytes = files
        .iter()
        .fold(0u64, |sum, entry| sum.saturating_add(entry.bytes));
    let inferred_object_ids = infer_object_ids_from_files(&files);
    let expanded = args
        .expanded
        .then(|| parse_expanded_entries(&reader, &files));

    let report = ListReport {
        archive: args.archive.display().to_string(),
        source: source.as_str().to_string(),
        file_count: files.len(),
        total_bytes,
        manifest_present: manifest.is_some(),
        manifest_error,
        manifest_summary: manifest.as_ref().map(manifest_summary),
        object_ids: if args.brief {
            None
        } else {
            Some(inferred_object_ids.clone())
        },
        files: args.files.then_some(files.clone()),
        expanded: expanded.clone(),
    };

    output.emit(&report, || {
        let mut text = TextBuilder::new();
        render_list_summary(&mut text, &report, inferred_object_ids.len());
        if args.files {
            for entry in &files {
                text.line(format!("{} {}", entry.bytes, entry.path));
            }
        } else if let Some(entries) = expanded.as_ref() {
            render_expanded_entries(&mut text, entries);
        } else if !args.brief {
            for object_id in &inferred_object_ids {
                text.line(object_id);
            }
        }
        text.finish()
    })
}

fn handle_manifest(output: &CommandOutput, args: &ManifestArgs) -> Result<()> {
    let reader = ArchiveReader::from_path(&args.archive)?;
    let (manifest, manifest_error) = read_manifest_prefer_sidecar(&args.archive, &reader);
    if let Some(manifest) = manifest {
        // The manifest is already a JSON document; human mode renders it indented.
        output.emit_json(&manifest)
    } else {
        if let Some(err) = manifest_error {
            bail!("manifest unreadable: {err}");
        }
        bail!("manifest not found in archive");
    }
}

fn render_list_summary(text: &mut TextBuilder, report: &ListReport, object_count: usize) {
    text.line(format!("archive: {}", report.archive));
    if let Some(summary) = report.manifest_summary.as_ref() {
        text.line(format!(
            "space: {} ({})",
            summary.source_space_name, summary.source_space_id
        ));
        let created = summary
            .created_at_display
            .clone()
            .or_else(|| format_datetime_display(&summary.created_at))
            .unwrap_or_else(|| summary.created_at.clone());
        text.line(format!("created: {created}"));
        text.line(format!("format: {}", summary.format));
    } else if let Some(err) = report.manifest_error.as_ref() {
        text.line(format!("manifest: unreadable ({err})"));
    } else {
        text.line("manifest: missing");
    }
    text.line(format!("objects: {object_count}"));
    text.line(format!(
        "files: {} ({} bytes)",
        report.file_count, report.total_bytes
    ));
}

fn render_expanded_entries(text: &mut TextBuilder, entries: &[ExpandedSnapshotEntry]) {
    let unreadable = entries.iter().filter(|e| e.status == "unreadable").count();
    text.line(format!(
        "expanded: parsed={} unreadable={}",
        entries.len().saturating_sub(unreadable),
        unreadable
    ));
    for entry in entries {
        if entry.status == "unreadable" {
            text.line(format!(
                "unreadable path={} id={} reason={}",
                entry.path,
                entry.id.as_deref().unwrap_or("-"),
                entry.unreadable_reason.as_deref().unwrap_or("-")
            ));
        } else {
            let object_type = entry
                .object_type
                .as_ref()
                .map_or_else(|| "null".to_string(), ToString::to_string);
            text.line(format!(
                "ok path={} id={} name={} type={} layout={}({}) archived={}",
                entry.path,
                entry.id.as_deref().unwrap_or("-"),
                entry.name.as_deref().unwrap_or("-"),
                object_type,
                entry
                    .layout
                    .map_or_else(|| "-".to_string(), |n| n.to_string()),
                entry.layout_name.as_deref().unwrap_or("-"),
                entry
                    .archived
                    .map_or_else(|| "-".to_string(), |b| b.to_string())
            ));
        }
    }
}

fn handle_extract(output: &CommandOutput, args: &ExtractArgs) -> Result<()> {
    let kind = save_archive_object(&args.archive, &args.object_id, &args.destination)?;
    let label = match kind {
        SavedObjectKind::Markdown => "markdown",
        SavedObjectKind::Raw => "raw",
    };
    let report = serde_json::json!({
        "archive": args.archive,
        "object_id": args.object_id,
        "output": args.destination,
        "kind": label,
    });
    output.emit(&report, || {
        format!(
            "extracted object {} from {} to {} ({label})",
            args.object_id,
            args.archive.display(),
            args.destination.display()
        )
    })
}

async fn resolve_space(client: &AnytypeClient, space_id_or_name: &str) -> Result<Space> {
    if looks_like_object_id(space_id_or_name) {
        return client
            .space(space_id_or_name)
            .get()
            .await
            .with_context(|| format!("space not found: {space_id_or_name}"));
    }

    let spaces = client.spaces().list().await?.collect_all().await?;
    let needle = space_id_or_name.to_lowercase();
    let matches: Vec<_> = spaces
        .into_iter()
        .filter(|space| space.name.to_lowercase() == needle)
        .collect();

    match matches.len() {
        0 => Err(anyhow!("space not found: {space_id_or_name}")),
        1 => Ok(matches[0].clone()),
        _ => Err(anyhow!("space name is ambiguous: {space_id_or_name}")),
    }
}

fn object_to_descriptor(object: &Object) -> ObjectDescriptor {
    let last_modified = object
        .get_property_date("last_modified_date")
        .or_else(|| object.get_property_date("lastModifiedDate"))
        .map(|d| d.to_rfc3339());

    ObjectDescriptor {
        id: object.id.clone(),
        new_id: None,
        name: object.name.clone(),
        r#type: object.r#type.as_ref().map(|typ| typ.key.clone()),
        last_modified,
    }
}

fn parse_object_id_lines(input: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();

    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            ids.push(trimmed.to_string());
        }
    }

    ids
}

fn load_object_ids_spec(spec: &str) -> Result<Vec<String>> {
    if spec == "-" {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .context("failed to read object id list from stdin")?;
        return Ok(parse_object_id_lines(&input));
    }

    let text = std::fs::read_to_string(spec)
        .with_context(|| format!("failed to read object list file: {spec}"))?;
    Ok(parse_object_id_lines(&text))
}

fn progress_enabled(output: &CommandOutput, stderr_is_tty: bool) -> bool {
    output.allows_progress() && stderr_is_tty
}

struct ProgressReporter {
    bar: Option<ProgressBar>,
}

impl ProgressReporter {
    fn new(output: &CommandOutput, message: &str) -> Self {
        let enabled = progress_enabled(output, io::stderr().is_terminal());
        if enabled {
            let bar = ProgressBar::new_spinner();
            let style = ProgressStyle::with_template("{spinner:.green} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner());
            bar.set_style(style);
            bar.enable_steady_tick(std::time::Duration::from_millis(120));
            bar.set_message(message.to_string());
            Self { bar: Some(bar) }
        } else {
            Self { bar: None }
        }
    }

    fn enabled(&self) -> bool {
        self.bar.is_some()
    }

    fn set_message(&self, message: &str) {
        if let Some(bar) = &self.bar {
            bar.set_message(message.to_string());
        }
    }

    fn finish(&self, message: &str) {
        if let Some(bar) = &self.bar {
            bar.finish_with_message(message.to_string());
        }
    }
}

fn read_manifest_from_archive(path: &Path) -> Result<Option<Manifest>> {
    let (sidecar_manifest, sidecar_error) = read_manifest_from_sidecar(path);
    if let Some(manifest) = sidecar_manifest {
        return Ok(Some(manifest));
    }
    if let Some(err) = sidecar_error {
        bail!(
            "invalid sidecar manifest for archive {}: {err}",
            path.display()
        );
    }

    let reader = ArchiveReader::from_path(path)?;
    let (manifest, manifest_error) = read_manifest_from_reader(&reader);
    if let Some(manifest) = manifest {
        return Ok(Some(manifest));
    }
    if let Some(err) = manifest_error {
        bail!("invalid manifest in archive {}: {err}", path.display());
    }
    Ok(None)
}

fn descriptors_from_selection(
    selected_ids: &[String],
    manifest: Option<&Manifest>,
) -> Vec<ObjectDescriptor> {
    if let Some(manifest) = manifest {
        let index = manifest
            .objects
            .iter()
            .map(|obj| (obj.id.clone(), obj.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        return selected_ids
            .iter()
            .map(|id| {
                index.get(id).cloned().unwrap_or_else(|| ObjectDescriptor {
                    id: id.clone(),
                    new_id: None,
                    name: None,
                    r#type: None,
                    last_modified: None,
                })
            })
            .collect();
    }

    selected_ids
        .iter()
        .map(|id| ObjectDescriptor {
            id: id.clone(),
            new_id: None,
            name: None,
            r#type: None,
            last_modified: None,
        })
        .collect()
}

/// Records the import outcome on the tracing channel.
///
/// Kept separate from the result document so that quiet and JSON output still
/// produce operator diagnostics on stderr.
fn log_report_summary(report: &ImportReport) {
    info!(
        "import summary: imported={} attempted={} failed={}",
        report.imported, report.attempted, report.failed
    );
    if report.failed > 0 {
        warn!("import completed with failures");
    }
}

/// Renders the human-readable import summary.
fn render_report_summary(report: &ImportReport) -> String {
    let mut text = TextBuilder::new();
    text.line(format!(
        "imported {}/{} objects (failed: {})",
        report.imported, report.attempted, report.failed
    ));
    for line in &report.summary {
        text.line(format!("- {line}"));
    }
    text.finish()
}

fn prepare_report(report: &ImportReport, path: &Path) -> Result<output::PreparedOutput> {
    let output = CommandOutput::new(OutputMode::Pretty, Some(path.to_path_buf()));
    let rendered = output
        .render(report, String::new)?
        .ok_or_else(|| anyhow!("report output was unexpectedly suppressed"))?;
    output.prepare_rendered(rendered)
}

fn sanitize_path_component(input: &str) -> String {
    const SEP: char = '_';
    let mut out = String::with_capacity(input.len());
    let mut prev_sep = false;
    for ch in input.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            ch
        } else {
            SEP
        };
        if mapped == SEP {
            if !prev_sep {
                out.push(SEP);
                prev_sep = true;
            }
        } else {
            out.push(mapped);
            prev_sep = false;
        }
    }
    out.trim_matches(SEP).to_string()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use clap::Parser;

    use super::*;

    #[derive(Debug, Parser)]
    #[command(name = "anyback")]
    struct Cli {
        #[command(subcommand)]
        command: Commands,
    }

    fn parse_user_cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap()
    }

    #[test]
    fn parse_object_lines_ignores_comments_and_blanks() {
        let text = "\n# comment\na\n\n b\n#c\na\n";
        let ids = parse_object_id_lines(text);
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn parse_direct_object_ids_csv() {
        let err = load_object_ids_spec("a,b, c").unwrap_err();
        assert!(
            err.to_string().contains("failed to read object list file"),
            "unexpected error: {err:#}"
        );
    }

    fn publication_test_manifest() -> Manifest {
        Manifest {
            schema_version: 1,
            tool: "anyback/test".to_string(),
            created_at: "1970-01-01T00:00:00Z".to_string(),
            created_at_display: None,
            source_space_id: "space-test".to_string(),
            source_space_name: "Test".to_string(),
            format: "pb".to_string(),
            object_count: 0,
            objects: Vec::new(),
            mode: Some("full".to_string()),
            since: None,
            since_display: None,
            until: None,
            until_display: None,
            type_ids: None,
            archive_size: None,
            archive_sha256: None,
        }
    }

    #[tokio::test]
    async fn archive_publication_refuses_existing_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("stage.zip");
        let destination = temp.path().join("backup.zip");
        fs::write(&source, b"new archive").expect("source");
        fs::write(&destination, b"existing archive").expect("destination");
        let manifest = publication_test_manifest();
        let error = WorkflowDeadline::local_command()
            .run_read_publication(
                "test timeout",
                move || prepare_backup_artifacts(source, destination, &manifest),
                commit_backup_artifacts,
            )
            .await
            .expect_err("existing destination must not be replaced");
        assert!(error.to_string().contains("already exists"));
        assert_eq!(
            fs::read(temp.path().join("backup.zip")).expect("destination"),
            b"existing archive"
        );
        assert!(
            !manifest_sidecar_path(&temp.path().join("backup.zip")).exists(),
            "an archive collision must not leave an orphan manifest"
        );
    }

    #[tokio::test]
    async fn expired_blocking_publication_cannot_report_success_or_replace_output() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("result.json");
        fs::write(&destination, b"existing").expect("destination");
        let output = CommandOutput::new(OutputMode::Pretty, Some(destination.clone()));
        let deadline = WorkflowDeadline::new(
            Some(std::time::Duration::from_millis(20)),
            ProcessWatcherTimeouts::default(),
        );
        let result = deadline
            .run_read_publication(
                "publication timed out",
                move || output.prepare_rendered("replacement".to_string()),
                |prepared, authority| {
                    // Models descheduling after the worker completes but before
                    // the caller-owned commit reaches its final boundary.
                    std::thread::sleep(std::time::Duration::from_millis(60));
                    CommandOutput::commit_prepared(prepared, authority)
                },
            )
            .await;
        assert!(result.is_err());
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert_eq!(fs::read(destination).expect("destination"), b"existing");
    }

    #[tokio::test]
    async fn claimed_commit_is_joined_across_deadline_instead_of_timing_out() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("claimed");
        let committed = destination.clone();
        let deadline = WorkflowDeadline::new(
            Some(std::time::Duration::from_millis(20)),
            ProcessWatcherTimeouts::default(),
        );
        deadline
            .run_read_publication(
                "publication timed out",
                || Ok(()),
                move |(), authority| {
                    authority.commit(|| {
                        std::thread::sleep(std::time::Duration::from_millis(60));
                        fs::write(committed, b"committed")?;
                        Ok(())
                    })
                },
            )
            .await
            .expect("a commit claimed before expiry reaches terminal finalization");
        assert_eq!(fs::read(destination).expect("committed"), b"committed");
    }

    #[test]
    fn crash_before_manifest_claim_publishes_neither_artifact() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("stage.zip");
        let destination = temp.path().join("backup.zip");
        fs::write(&source, b"archive").expect("source");
        let prepared =
            prepare_backup_artifacts(source, destination.clone(), &publication_test_manifest())
                .expect("prepare");
        drop(prepared);
        assert!(!destination.exists());
        assert!(!manifest_sidecar_path(&destination).exists());
    }

    #[test]
    fn staging_path_swap_cannot_change_bound_or_published_archive() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("stage.zip");
        let destination = temp.path().join("backup.zip");
        let retained = temp.path().join("retained-original.zip");
        fs::write(&source, b"owned archive bytes").expect("source");
        let retained_for_hook = retained.clone();
        let prepared = prepare_backup_artifacts_with_hook(
            source,
            destination.clone(),
            &publication_test_manifest(),
            move |staging| {
                fs::rename(staging, &retained_for_hook)?;
                fs::write(staging, b"foreign swapped bytes")?;
                Ok(())
            },
        )
        .expect("prepare retains the opened archive handle");

        let bound: Manifest = serde_json::from_slice(
            &fs::read(&prepared.staged_sidecar).expect("bound staged manifest"),
        )
        .expect("parse bound manifest");
        let (expected_size, expected_digest) =
            decode::archive_binding(&retained).expect("binding for retained original archive");
        assert_eq!(bound.archive_size, Some(expected_size));
        assert_eq!(
            bound.archive_sha256.as_deref(),
            Some(expected_digest.as_str())
        );
        assert!(
            ensure_file_owned(&prepared.staged_archive, &prepared.archive_identity).is_err(),
            "the swapped pathname must not satisfy the retained opened identity"
        );
        drop(prepared);
        assert!(!destination.exists());
        assert!(!manifest_sidecar_path(&destination).exists());
    }

    #[test]
    fn crash_after_manifest_claim_cannot_bind_a_missing_or_foreign_archive() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("stage.zip");
        let destination = temp.path().join("backup.zip");
        fs::write(&source, b"archive").expect("source");
        let prepared =
            prepare_backup_artifacts(source, destination.clone(), &publication_test_manifest())
                .expect("prepare");
        claim_owned_staging_file(
            &prepared.staged_sidecar,
            &prepared.sidecar,
            &prepared.sidecar_identity,
        )
        .expect("manifest claim");
        drop(prepared);
        assert!(!destination.exists());
        let (manifest, missing_error) = read_manifest_from_sidecar(&destination);
        assert!(manifest.is_none());
        assert_eq!(
            missing_error.as_deref(),
            Some("sidecar archive binding could not be verified")
        );
        fs::write(&destination, b"foreign archive").expect("foreign archive");
        let (manifest, mismatch_error) = read_manifest_from_sidecar(&destination);
        assert!(manifest.is_none());
        assert_eq!(
            mismatch_error.as_deref(),
            Some("sidecar archive binding does not match the selected archive")
        );
    }

    #[tokio::test]
    async fn completed_archive_and_manifest_have_a_valid_binding() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("stage.zip");
        let destination = temp.path().join("backup.zip");
        fs::write(&source, b"archive").expect("source");
        let result_path = destination.clone();
        WorkflowDeadline::local_command()
            .run_read_publication(
                "test timeout",
                move || prepare_backup_artifacts(source, destination, &publication_test_manifest()),
                commit_backup_artifacts,
            )
            .await
            .expect("publish bound pair");
        let (manifest, error) = read_manifest_from_sidecar(&result_path);
        assert!(error.is_none());
        assert!(manifest.is_some());
    }

    #[tokio::test]
    async fn concurrent_archive_replacement_is_preserved_without_an_orphan_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("stage.zip");
        let destination = temp.path().join("backup.zip");
        fs::write(&source, b"archive").expect("source");
        let hook_path = destination.clone();
        let assertion_path = destination.clone();
        let result = WorkflowDeadline::local_command()
            .run_read_publication(
                "test timeout",
                move || prepare_backup_artifacts(source, destination, &publication_test_manifest()),
                move |prepared, authority| {
                    commit_backup_artifacts_with_hook(prepared, authority, move || {
                        fs::write(&hook_path, b"foreign archive replacement")?;
                        Ok(())
                    })
                },
            )
            .await;
        assert!(result.is_err());
        assert_eq!(
            fs::read(&assertion_path).expect("replacement"),
            b"foreign archive replacement"
        );
        assert!(!manifest_sidecar_path(&assertion_path).exists());
    }

    #[tokio::test]
    async fn concurrent_manifest_replacement_is_preserved_without_published_archive() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("stage.zip");
        let destination = temp.path().join("backup.zip");
        fs::write(&source, b"archive").expect("source");
        let replacement_path = manifest_sidecar_path(&destination);
        let hook_path = replacement_path.clone();
        let assertion_archive = destination.clone();
        let result = WorkflowDeadline::local_command()
            .run_read_publication(
                "test timeout",
                move || prepare_backup_artifacts(source, destination, &publication_test_manifest()),
                move |prepared, authority| {
                    commit_backup_artifacts_with_hook(prepared, authority, move || {
                        fs::remove_file(&hook_path).expect("remove owned manifest claim");
                        fs::write(&hook_path, b"foreign manifest replacement")?;
                        Ok(())
                    })
                },
            )
            .await;
        assert!(result.is_err());
        assert!(!assertion_archive.exists());
        assert_eq!(
            fs::read(replacement_path).expect("replacement"),
            b"foreign manifest replacement"
        );
    }

    #[test]
    fn parse_backup_create_from_legacy_export_alias() {
        let cli = Cli::try_parse_from([
            "anyback",
            "export",
            "--space",
            "test",
            "--objects",
            "ids.txt",
        ])
        .unwrap();
        assert!(matches!(cli.command, Commands::Export(_)));
    }

    fn extract_backup_create_args(command: Commands) -> BackupCreateArgs {
        match command {
            Commands::Create(args) | Commands::Export(args) => args,
            _ => panic!("expected backup or export command"),
        }
    }

    fn assert_backup_args_equal(left: &BackupCreateArgs, right: &BackupCreateArgs) {
        assert_eq!(left.space, right.space);
        assert_eq!(left.objects, right.objects);
        assert_eq!(left.format.as_str(), right.format.as_str());
        assert_eq!(left.mode.as_str(), right.mode.as_str());
        assert_eq!(left.since, right.since);
        assert!(matches!(
            (left.since_mode, right.since_mode),
            (SinceModeArg::Exclusive, SinceModeArg::Exclusive)
                | (SinceModeArg::Inclusive, SinceModeArg::Inclusive)
        ));
        assert_eq!(left.types, right.types);
        assert_eq!(left.dir, right.dir);
        assert_eq!(left.dest, right.dest);
        assert_eq!(left.prefix, right.prefix);
        assert_eq!(left.include_nested, right.include_nested);
        assert_eq!(left.include_files, right.include_files);
        assert_eq!(left.include_archived, right.include_archived);
        assert_eq!(left.include_backlinks, right.include_backlinks);
        assert_eq!(left.include_properties, right.include_properties);
    }

    #[test]
    fn parse_backup_and_export_alias_map_identically() {
        let backup = parse_user_cli(&[
            "anyback",
            "create",
            "--space",
            "test-space",
            "--objects",
            "ids.txt",
            "--format",
            "pb-json",
            "--mode",
            "incremental",
            "--since",
            "2026-01-01T00:00:00Z",
            "--since-mode",
            "inclusive",
            "--include-nested",
            "--include-files",
            "--include-archived",
            "--include-backlinks",
            "--prefix",
            "pref",
        ]);
        let export = parse_user_cli(&[
            "anyback",
            "export",
            "--space",
            "test-space",
            "--objects",
            "ids.txt",
            "--format",
            "pb-json",
            "--mode",
            "incremental",
            "--since",
            "2026-01-01T00:00:00Z",
            "--since-mode",
            "inclusive",
            "--include-nested",
            "--include-files",
            "--include-archived",
            "--include-backlinks",
            "--prefix",
            "pref",
        ]);

        let backup_args = extract_backup_create_args(backup.command);
        let export_args = extract_backup_create_args(export.command);
        assert_backup_args_equal(&backup_args, &export_args);
    }

    #[test]
    fn parse_import_from_legacy_alias() {
        let cli =
            Cli::try_parse_from(["anyback", "import", "--space", "dest", "archive-dir"]).unwrap();
        assert!(matches!(cli.command, Commands::Import(_)));
    }

    #[test]
    fn parse_backup_create_dir_dest_conflict() {
        let err = Cli::try_parse_from([
            "anyback",
            "create",
            "--space",
            "test",
            "--dir",
            "/tmp",
            "--dest",
            "/tmp/archive",
        ])
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("cannot be used with"));
    }

    #[test]
    fn parse_backup_create_dest_prefix_conflict() {
        let err = Cli::try_parse_from([
            "anyback",
            "create",
            "--space",
            "test",
            "--dest",
            "/tmp/archive",
            "--prefix",
            "mybackup",
        ])
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("cannot be used with"));
    }

    #[test]
    fn parse_backup_create_incremental_requires_since() {
        let err = Cli::try_parse_from([
            "anyback",
            "create",
            "--space",
            "test",
            "--mode",
            "incremental",
        ])
        .unwrap_err();
        assert!(err.to_string().contains("--since"));
    }

    #[test]
    fn parse_backup_create_types_objects_conflict() {
        let err = Cli::try_parse_from([
            "anyback",
            "create",
            "--space",
            "test",
            "--objects",
            "ids.txt",
            "--types",
            "page,note",
        ])
        .unwrap_err();
        assert!(err.to_string().contains("cannot be used with"));
    }

    #[test]
    fn parse_backup_create_types_csv() {
        let cli = Cli::try_parse_from([
            "anyback",
            "create",
            "--space",
            "test",
            "--types",
            "page,note",
        ])
        .unwrap();
        if let Commands::Create(args) = cli.command {
            assert_eq!(
                args.types,
                Some(vec!["page".to_string(), "note".to_string()])
            );
        } else {
            panic!("expected backup command");
        }
    }

    #[test]
    fn parse_restore_apply_import_mode() {
        let cli = parse_user_cli(&[
            "anyback",
            "restore",
            "--space",
            "dest",
            "--import-mode",
            "all-or-nothing",
            "archive-dir",
        ]);
        if let Commands::Restore(args) = cli.command {
            assert!(matches!(args.import_mode, ImportModeArg::AllOrNothing));
        } else {
            panic!("expected restore command");
        }
    }

    #[test]
    fn parse_diff_command() {
        let cli = Cli::try_parse_from(["anyback", "diff", "a.zip", "b.zip"]).unwrap();
        assert!(matches!(cli.command, Commands::Diff(_)));
    }

    #[test]
    fn parse_restore_dry_run_flag() {
        let cli = Cli::try_parse_from([
            "anyback",
            "restore",
            "--dry-run",
            "--space",
            "test-space",
            "full-archive",
        ])
        .unwrap();
        if let Commands::Restore(args) = cli.command {
            assert!(args.dry_run);
        } else {
            panic!("expected restore command");
        }
    }

    #[test]
    fn parse_list_command() {
        let cli = Cli::try_parse_from(["anyback", "list", "--files", "archive-dir"]).unwrap();
        if let Commands::List(args) = cli.command {
            assert!(args.files);
            assert!(!args.brief);
            assert!(!args.expanded);
        } else {
            panic!("expected list command");
        }
    }

    #[test]
    fn parse_list_brief_flag() {
        let cli = Cli::try_parse_from(["anyback", "list", "--brief", "archive-dir"]).unwrap();
        if let Commands::List(args) = cli.command {
            assert!(args.brief);
            assert!(!args.expanded);
            assert!(!args.files);
        } else {
            panic!("expected list command");
        }
    }

    #[test]
    fn parse_list_expanded_flag() {
        let cli = Cli::try_parse_from(["anyback", "list", "--expanded", "archive-dir"]).unwrap();
        if let Commands::List(args) = cli.command {
            assert!(args.expanded);
        } else {
            panic!("expected list command");
        }
    }

    #[test]
    fn parse_list_mutually_exclusive_flags() {
        let err = Cli::try_parse_from(["anyback", "list", "--brief", "--files", "archive-dir"])
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cannot be used with") || msg.contains("list_mode"),
            "expected mutual exclusion error, got: {msg}"
        );
    }

    #[test]
    fn parse_manifest_command() {
        let cli = Cli::try_parse_from(["anyback", "manifest", "archive-dir"]).unwrap();
        assert!(matches!(cli.command, Commands::Manifest(_)));
    }

    #[test]
    fn parse_extract_command() {
        let cli = Cli::try_parse_from([
            "anyback",
            "extract",
            "archive-dir",
            "bafyreitest",
            "/tmp/out.md",
        ])
        .unwrap();
        if let Commands::Extract(args) = cli.command {
            assert_eq!(args.object_id, "bafyreitest");
            assert_eq!(args.archive, PathBuf::from("archive-dir"));
            assert_eq!(args.destination, PathBuf::from("/tmp/out.md"));
        } else {
            panic!("expected extract command");
        }
    }

    #[cfg(feature = "tui")]
    #[test]
    fn parse_inspect_command() {
        let cli = Cli::try_parse_from(["anyback", "inspect", "archive-dir"]).unwrap();
        if let Commands::Inspect(args) = cli.command {
            assert_eq!(args.archive, PathBuf::from("archive-dir"));
            assert_eq!(args.max_cache, 200 * 1024 * 1024);
        } else {
            panic!("expected inspect command");
        }
    }

    #[cfg(feature = "tui")]
    #[test]
    fn parse_inspect_command_with_max_cache_units() {
        let cli = Cli::try_parse_from(["anyback", "inspect", "--max-cache", "512k", "archive-dir"])
            .unwrap();
        if let Commands::Inspect(args) = cli.command {
            assert_eq!(args.max_cache, 512 * 1024);
        } else {
            panic!("expected inspect command");
        }
    }

    #[test]
    fn parse_backup_create_rejects_removed_zip_flag() {
        let err =
            Cli::try_parse_from(["anyback", "create", "--space", "test", "--zip"]).unwrap_err();
        assert!(err.to_string().contains("--zip"));
    }

    #[test]
    fn parse_backup_include_flags() {
        let cli = parse_user_cli(&[
            "anyback",
            "create",
            "--space",
            "test",
            "--include-nested",
            "--include-files",
            "--include-archived",
            "--include-backlinks",
            "--include-properties",
            "--format",
            "markdown",
        ]);
        if let Commands::Create(args) = cli.command {
            assert!(args.include_nested);
            assert!(args.include_files);
            assert!(args.include_archived);
            assert!(args.include_backlinks);
            assert!(args.include_properties);
        } else {
            panic!("expected backup command");
        }
    }

    #[test]
    fn validate_backup_args_rejects_include_properties_non_markdown() {
        let args = BackupCreateArgs {
            space: "space".to_string(),
            objects: None,
            format: ExportFormatArg::Pb,
            mode: BackupModeArg::Full,
            since: None,
            since_mode: SinceModeArg::Exclusive,
            types: None,
            dir: None,
            dest: None,
            prefix: None,
            include_nested: false,
            include_files: false,
            include_archived: false,
            include_backlinks: false,
            include_properties: true,
        };
        let err = validate_backup_args(&args).unwrap_err();
        assert!(err.to_string().contains("--include-properties"));
    }

    #[test]
    fn backup_export_options_maps_include_flags_and_pb_json() {
        let args = BackupCreateArgs {
            space: "space".to_string(),
            objects: None,
            format: ExportFormatArg::PbJson,
            mode: BackupModeArg::Full,
            since: None,
            since_mode: SinceModeArg::Exclusive,
            types: None,
            dir: None,
            dest: None,
            prefix: None,
            include_nested: true,
            include_files: true,
            include_archived: true,
            include_backlinks: true,
            include_properties: false,
        };

        let options = backup_export_options(&args);
        assert_eq!(options.format, BackupExportFormat::Protobuf);
        assert!(options.is_json);
        assert!(options.include_nested);
        assert!(options.include_files);
        assert!(options.include_archived);
        assert!(options.include_backlinks);
        assert!(options.include_space);
        assert!(!options.md_include_properties_and_schema);
    }

    #[test]
    fn backup_export_options_maps_markdown_include_properties() {
        let args = BackupCreateArgs {
            space: "space".to_string(),
            objects: None,
            format: ExportFormatArg::Markdown,
            mode: BackupModeArg::Full,
            since: None,
            since_mode: SinceModeArg::Exclusive,
            types: None,
            dir: None,
            dest: None,
            prefix: None,
            include_nested: false,
            include_files: false,
            include_archived: false,
            include_backlinks: false,
            include_properties: true,
        };

        let options = backup_export_options(&args);
        assert_eq!(options.format, BackupExportFormat::Markdown);
        assert!(!options.is_json);
        assert!(options.md_include_properties_and_schema);
        assert!(options.include_space);
    }

    #[test]
    fn progress_disabled_when_json_enabled() {
        assert!(!progress_enabled(&CommandOutput::json(), true));
        assert!(!progress_enabled(
            &CommandOutput::new(OutputMode::Pretty, None),
            true
        ));
    }

    #[test]
    fn progress_disabled_when_quiet() {
        assert!(!progress_enabled(
            &CommandOutput::new(OutputMode::Quiet, None),
            true
        ));
    }

    #[test]
    fn progress_disabled_for_non_tty() {
        assert!(!progress_enabled(&CommandOutput::human(), false));
    }

    #[test]
    fn progress_enabled_for_tty_human_output() {
        assert!(progress_enabled(&CommandOutput::human(), true));
    }

    #[test]
    fn progress_reporter_disabled_when_json_enabled() {
        let reporter = ProgressReporter::new(&CommandOutput::json(), "hidden");
        assert!(!reporter.enabled());
    }

    #[test]
    fn infer_object_ids_from_files_uses_objects_dir() {
        let valid_id = "bafyreiaebddr63d7sye3eggmtkyeioqxftoaipobsynceksj6faedvd2xi";
        let files = vec![
            ArchiveFileEntry {
                path: format!("objects/{valid_id}.pb"),
                bytes: 42,
            },
            ArchiveFileEntry {
                path: format!("relations/{valid_id}.pb"),
                bytes: 10,
            },
            ArchiveFileEntry {
                path: "objects/not-an-object-id.pb".to_string(),
                bytes: 12,
            },
        ];
        let inferred = infer_object_ids_from_files(&files);
        assert_eq!(inferred, vec![valid_id.to_string()]);
    }

    #[test]
    fn manifest_roundtrip_json() {
        let manifest = Manifest {
            schema_version: 1,
            tool: "anyback/0.1.0".to_string(),
            created_at: chrono::DateTime::<Utc>::from_timestamp(0, 0)
                .unwrap()
                .to_rfc3339(),
            created_at_display: Some("1970-01-01 00:00:00 UTC".to_string()),
            source_space_id: "space1".to_string(),
            source_space_name: "My Space".to_string(),
            format: "pb".to_string(),
            object_count: 1,
            objects: vec![ObjectDescriptor {
                id: "obj1".to_string(),
                new_id: None,
                name: Some("Obj".to_string()),
                r#type: Some("page".to_string()),
                last_modified: None,
            }],
            mode: Some("full".to_string()),
            since: None,
            since_display: None,
            until: None,
            until_display: None,
            type_ids: None,
            archive_size: None,
            archive_sha256: None,
        };

        let text = serde_json::to_string(&manifest).unwrap();
        let parsed: Manifest = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.object_count, 1);
        assert_eq!(parsed.objects[0].id, "obj1");
    }

    #[test]
    fn backup_target_dir_must_exist() {
        let args = BackupCreateArgs {
            space: "space".to_string(),
            objects: None,
            format: ExportFormatArg::Pb,
            mode: BackupModeArg::Full,
            since: None,
            since_mode: SinceModeArg::Exclusive,
            types: None,
            dir: Some(PathBuf::from("/this/definitely/does/not/exist")),
            dest: None,
            prefix: None,
            include_nested: false,
            include_files: false,
            include_archived: false,
            include_backlinks: false,
            include_properties: false,
        };
        let err = resolve_backup_target(&args, "space-id").unwrap_err();
        assert!(err.to_string().contains("output directory does not exist"));
    }

    #[test]
    fn backup_target_dest_must_not_exist() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("existing");
        std::fs::create_dir_all(&dest).unwrap();
        let args = BackupCreateArgs {
            space: "space".to_string(),
            objects: None,
            format: ExportFormatArg::Pb,
            mode: BackupModeArg::Full,
            since: None,
            since_mode: SinceModeArg::Exclusive,
            types: None,
            dir: None,
            dest: Some(dest),
            prefix: None,
            include_nested: false,
            include_files: false,
            include_archived: false,
            include_backlinks: false,
            include_properties: false,
        };
        let err = resolve_backup_target(&args, "space-id").unwrap_err();
        assert!(
            err.to_string()
                .contains("target archive path already exists"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    fn backup_target_dir_uses_space_id_in_default_name() {
        let temp = tempfile::tempdir().unwrap();
        let args = BackupCreateArgs {
            space: "space".to_string(),
            objects: None,
            format: ExportFormatArg::Pb,
            mode: BackupModeArg::Full,
            since: None,
            since_mode: SinceModeArg::Exclusive,
            types: None,
            dir: Some(temp.path().to_path_buf()),
            dest: None,
            prefix: None,
            include_nested: false,
            include_files: false,
            include_archived: false,
            include_backlinks: false,
            include_properties: false,
        };
        let resolved = resolve_backup_target(&args, "spacex").unwrap();
        let name = resolved
            .archive_path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap();
        assert!(name.starts_with("backup_spacex_"));
        assert!(name.ends_with(".zip"));
    }

    #[test]
    fn backup_target_always_uses_zip_extension_for_generated_name() {
        let temp = tempfile::tempdir().unwrap();
        let args = BackupCreateArgs {
            space: "space".to_string(),
            objects: None,
            format: ExportFormatArg::Pb,
            mode: BackupModeArg::Full,
            since: None,
            since_mode: SinceModeArg::Exclusive,
            types: None,
            dir: Some(temp.path().to_path_buf()),
            dest: None,
            prefix: None,
            include_nested: false,
            include_files: false,
            include_archived: false,
            include_backlinks: false,
            include_properties: false,
        };
        let resolved = resolve_backup_target(&args, "spacex").unwrap();
        assert!(resolved.zip);
        assert!(
            resolved
                .archive_path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "zip")
        );
    }

    #[test]
    fn backup_target_is_zip_even_without_dest_zip_extension() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("backup-out");
        let args = BackupCreateArgs {
            space: "space".to_string(),
            objects: None,
            format: ExportFormatArg::Pb,
            mode: BackupModeArg::Full,
            since: None,
            since_mode: SinceModeArg::Exclusive,
            types: None,
            dir: None,
            dest: Some(dest),
            prefix: None,
            include_nested: false,
            include_files: false,
            include_archived: false,
            include_backlinks: false,
            include_properties: false,
        };
        let resolved = resolve_backup_target(&args, "spacex").unwrap();
        assert!(resolved.zip);
    }

    #[test]
    fn build_import_plan_infers_ids_without_manifest_from_directory() {
        let temp = tempfile::tempdir().unwrap();
        let id = "bafyreiaebddr63d7sye3eggmtkyeioqxftoaipobsynceksj6faedvd2xi";
        let objects_dir = temp.path().join("objects");
        std::fs::create_dir_all(&objects_dir).unwrap();
        std::fs::write(objects_dir.join(format!("{id}.pb")), b"not-proto").unwrap();

        let plan = build_import_plan(temp.path(), None).unwrap();
        assert_eq!(plan.selected_ids, vec![id.to_string()]);
    }

    #[test]
    fn build_import_plan_infers_ids_without_manifest_from_zip() {
        let temp = tempfile::tempdir().unwrap();
        let zip_path = temp.path().join("archive.zip");
        let id = "bafyreiaebddr63d7sye3eggmtkyeioqxftoaipobsynceksj6faedvd2xi";
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file(
                    format!("objects/{id}.pb"),
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
            writer.write_all(b"not-proto").unwrap();
            writer.finish().unwrap();
        }

        let plan = build_import_plan(&zip_path, None).unwrap();
        assert_eq!(plan.selected_ids, vec![id.to_string()]);
    }

    #[test]
    fn build_import_plan_rejects_present_invalid_or_mismatched_sidecar() {
        let temp = tempfile::tempdir().expect("tempdir");
        let zip_path = temp.path().join("archive.zip");
        {
            let file = fs::File::create(&zip_path).expect("archive file");
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file(
                    "objects/bafyreiaebddr63d7sye3eggmtkyeioqxftoaipobsynceksj6faedvd2xi.pb",
                    zip::write::SimpleFileOptions::default(),
                )
                .expect("archive entry");
            writer.write_all(b"payload").expect("archive payload");
            writer.finish().expect("finish archive");
        }
        let sidecar = manifest_sidecar_path(&zip_path);
        fs::write(&sidecar, b"{not-json").expect("invalid sidecar");
        let invalid = match build_import_plan(&zip_path, None) {
            Ok(_) => panic!("a present invalid sidecar must fail restore planning"),
            Err(error) => error.to_string(),
        };
        assert!(invalid.contains("invalid sidecar manifest"));

        let mut mismatched = publication_test_manifest();
        mismatched.archive_size = Some(1);
        mismatched.archive_sha256 = Some("00".repeat(32));
        fs::write(
            &sidecar,
            serde_json::to_vec(&mismatched).expect("serialize mismatched sidecar"),
        )
        .expect("mismatched sidecar");
        let mismatch = match build_import_plan(&zip_path, None) {
            Ok(_) => panic!("a binding mismatch must fail restore planning"),
            Err(error) => error.to_string(),
        };
        assert!(mismatch.contains("does not match"));
    }

    #[test]
    fn build_import_plan_uses_archive_path_directly() {
        let temp = tempfile::tempdir().unwrap();
        let objects_dir = temp.path().join("objects");
        std::fs::create_dir_all(&objects_dir).unwrap();
        let id = "bafyreiaebddr63d7sye3eggmtkyeioqxftoaipobsynceksj6faedvd2xi";
        std::fs::write(objects_dir.join(format!("{id}.pb")), b"payload").unwrap();
        std::fs::write(
            temp.path().join("manifest.json"),
            serde_json::to_vec(&publication_test_manifest()).unwrap(),
        )
        .unwrap();

        let plan = build_import_plan(temp.path(), None).unwrap();
        assert_eq!(plan.import_path, temp.path());
    }

    #[cfg(feature = "snapshot-import")]
    fn sample_snapshot_entry(id: &str, encoded_hint: usize) -> ImportSnapshotEntry {
        let details = prost_types::Struct {
            fields: std::collections::BTreeMap::from([(
                "id".to_string(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::StringValue(id.to_string())),
                },
            )]),
        };
        let data = anytype_rpc::model::SmartBlockSnapshotBase {
            details: Some(details),
            ..Default::default()
        };
        let snapshot = import_request::Snapshot {
            id: id.to_string(),
            snapshot: Some(data),
        };
        let encoded_bytes = snapshot.encoded_len().max(encoded_hint);
        ImportSnapshotEntry {
            path: format!("objects/{id}.pb"),
            id: id.to_string(),
            sb_type: anytype_rpc::model::SmartBlockType::Page as i32,
            snapshot,
            encoded_bytes,
        }
    }

    #[cfg(feature = "snapshot-import")]
    #[test]
    fn plan_snapshot_batches_enforces_single_snapshot_limit() {
        let entry = sample_snapshot_entry(
            "bafyreiaebddr63d7sye3eggmtkyeioqxftoaipobsynceksj6faedvd2xi",
            500,
        );
        let limits = ImportChunkLimits {
            max_single_snapshot_bytes: 100,
            max_batch_bytes: 1000,
            max_batch_snapshots: 10,
        };
        let err = plan_snapshot_batches(&[entry], limits).unwrap_err();
        assert!(err.to_string().contains("is too large"));
    }

    #[cfg(feature = "snapshot-import")]
    #[test]
    fn plan_snapshot_batches_splits_by_batch_limits() {
        let entries = vec![
            sample_snapshot_entry(
                "bafyreiaebddr63d7sye3eggmtkyeioqxftoaipobsynceksj6faedvd2x1",
                200,
            ),
            sample_snapshot_entry(
                "bafyreiaebddr63d7sye3eggmtkyeioqxftoaipobsynceksj6faedvd2x2",
                200,
            ),
            sample_snapshot_entry(
                "bafyreiaebddr63d7sye3eggmtkyeioqxftoaipobsynceksj6faedvd2x3",
                200,
            ),
        ];
        let limits = ImportChunkLimits {
            max_single_snapshot_bytes: 300,
            max_batch_bytes: 450,
            max_batch_snapshots: 2,
        };
        let batches = plan_snapshot_batches(&entries, limits).unwrap();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), 2);
        assert_eq!(batches[1].len(), 1);
    }

    #[cfg(feature = "tui")]
    #[test]
    fn parse_cache_size_defaults_to_mib() {
        assert_eq!(parse_cache_size("200").unwrap(), 200 * 1024 * 1024);
    }

    #[cfg(feature = "tui")]
    #[test]
    fn parse_cache_size_accepts_units_case_insensitive() {
        assert_eq!(parse_cache_size("1k").unwrap(), 1024);
        assert_eq!(parse_cache_size("2KB").unwrap(), 2 * 1024);
        assert_eq!(parse_cache_size("3m").unwrap(), 3 * 1024 * 1024);
        assert_eq!(parse_cache_size("4Mb").unwrap(), 4 * 1024 * 1024);
        assert_eq!(parse_cache_size("1G").unwrap(), 1024 * 1024 * 1024);
    }

    #[cfg(feature = "tui")]
    #[test]
    fn parse_cache_size_rejects_invalid_unit() {
        let err = parse_cache_size("10tb").unwrap_err();
        assert!(err.to_string().contains("unsupported cache size unit"));
    }

    #[cfg(feature = "tui")]
    #[test]
    fn parse_cache_size_rejects_zero() {
        let err = parse_cache_size("0").unwrap_err();
        assert!(err.to_string().contains("must be > 0"));
    }

    #[test]
    fn parse_since_accepts_rfc3339_with_offset() {
        let input = "2026-01-12T10:11:22+05:30".to_string();
        let parsed = parse_since(Some(&input)).unwrap();
        assert_eq!(parsed.offset().local_minus_utc(), 5 * 3600 + 30 * 60);
        assert_eq!(to_rfc3339_with_offset(parsed), "2026-01-12T10:11:22+05:30");
    }

    #[test]
    fn parse_since_accepts_utc_suffix() {
        let input = "2026-01-12 10:11:22 UTC".to_string();
        let parsed = parse_since(Some(&input)).unwrap();
        assert_eq!(parsed.offset().local_minus_utc(), 0);
        assert_eq!(to_rfc3339_with_offset(parsed), "2026-01-12T10:11:22Z");
    }

    #[test]
    fn parse_since_accepts_plus_zero_suffix() {
        let input = "2026-01-12 10:11:22 +0".to_string();
        let parsed = parse_since(Some(&input)).unwrap();
        assert_eq!(parsed.offset().local_minus_utc(), 0);
        assert_eq!(to_rfc3339_with_offset(parsed), "2026-01-12T10:11:22Z");
    }

    #[test]
    fn parse_since_accepts_local_time_without_timezone() {
        let input = "2026-01-12 10:11:22".to_string();
        let parsed = parse_since(Some(&input)).unwrap();
        let expected = parse_local_naive("2026-01-12 10:11:22")
            .and_then(|naive| Local.from_local_datetime(&naive).single())
            .unwrap()
            .fixed_offset();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn parse_since_accepts_partial_date_variants_equivalently() {
        let full = parse_since(Some(&"2026-01-01 00:00:00".to_string())).unwrap();
        let hm = parse_since(Some(&"2026-01-01 00:00".to_string())).unwrap();
        let day = parse_since(Some(&"2026-01-01".to_string())).unwrap();
        let month = parse_since(Some(&"2026-01".to_string())).unwrap();
        let year = parse_since(Some(&"2026".to_string())).unwrap();
        assert_eq!(full, hm);
        assert_eq!(full, day);
        assert_eq!(full, month);
        assert_eq!(full, year);
    }

    #[test]
    fn pb_import_paths_skips_manifest_for_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("manifest.json"), "{}").unwrap();
        std::fs::write(root.join("profile"), "profile-bytes").unwrap();
        std::fs::write(root.join("top.pb"), "pb").unwrap();
        std::fs::create_dir(root.join("objects")).unwrap();
        std::fs::write(root.join("objects").join("obj.pb"), "pb").unwrap();

        let paths = pb_import_paths(root).unwrap();
        assert!(paths.iter().any(|p| Path::new(p).ends_with("objects")));
        assert!(paths.iter().any(|p| Path::new(p).ends_with("top.pb")));
        assert!(
            !paths
                .iter()
                .any(|p| Path::new(p).ends_with("manifest.json"))
        );
    }

    #[test]
    fn pb_import_paths_skips_empty_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir(root.join("empty")).unwrap();
        std::fs::create_dir(root.join("objects")).unwrap();
        std::fs::write(root.join("objects").join("a.pb"), "pb").unwrap();

        let paths = pb_import_paths(root).unwrap();
        assert!(paths.iter().any(|p| Path::new(p).ends_with("objects")));
        assert!(!paths.iter().any(|p| Path::new(p).ends_with("empty")));
    }

    #[test]
    fn archive_basename_uses_file_name() {
        assert_eq!(
            archive_basename(Path::new("/tmp/foo/archive-one.zip")),
            "archive-one.zip"
        );
    }

    #[test]
    fn format_import_api_error_includes_known_hint() {
        let message = format_import_api_error("import failed", 11);
        assert!(message.contains("code 11"));
        assert!(message.contains("valid Anyblock format"));
    }

    #[test]
    fn format_import_api_error_unknown_code_has_no_hint() {
        let message = format_import_api_error("import failed", 12345);
        assert_eq!(message, "import failed (code 12345)");
    }
}
