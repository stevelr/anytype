use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    io::IsTerminal,
    io::{self, Read},
    path::{Path, PathBuf},
    time::Duration,
};

use anyback_reader::archive::{
    ArchiveFileEntry, ArchiveReader, infer_object_id_from_snapshot_path,
    infer_object_ids_from_files,
};
use anyback_reader::markdown::{SavedObjectKind, save_archive_object};
use anyhow::{Context, Result, anyhow, bail, ensure};
use anytype::{
    prelude::*,
    process_watcher::{
        ProcessCompletionFallback, ProcessKind, ProcessWatchCancelToken, ProcessWatchProgress,
        ProcessWatchRequest, ProcessWatcher, ProcessWatcherTimeouts,
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
use clap::{Args, Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
#[cfg(feature = "snapshot-import")]
use prost::Message;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub mod auth;
pub mod decode;
mod inspector;

use decode::{
    ExpandedSnapshotEntry, ImportEventProgressReport, ImportReport, MANIFEST_NAME, Manifest,
    ManifestSummary, ObjectDescriptor, ObjectImportError, detail_value, format_datetime_display,
    format_last_modified, manifest_sidecar_path, manifest_summary, parse_expanded_entries,
    parse_snapshot_details_from_pb, parse_snapshot_details_from_pb_json, read_manifest_from_reader,
    read_manifest_from_sidecar, read_manifest_prefer_sidecar,
};

const DEFAULT_KEYRING_SERVICE: &str = "anyback";
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

#[derive(Parser, Debug)]
#[command(name = "anyback")]
#[command(author, version, about = "Anytype backup and restore tool", long_about = None)]
pub struct Cli {
    /// API endpoint URL. Default: environment `ANYTYPE_URL` or <http://127.0.0.1:31009>
    #[arg(short = 'u', long, env = "ANYTYPE_URL", global = true)]
    pub url: Option<String>,

    /// gRPC endpoint URL
    #[arg(long, env = "ANYTYPE_GRPC_ENDPOINT", global = true)]
    pub grpc: Option<String>,

    /// keystore type or config
    #[arg(long, env = "ANYTYPE_KEYSTORE", global = true)]
    pub keystore: Option<String>,

    /// Override service name (default "anyback")
    #[arg(long, env = "ANYTYPE_KEYSTORE_SERVICE", global = true)]
    pub keystore_service: Option<String>,

    /// Print machine-readable output where applicable
    #[arg(long, global = true)]
    pub json: bool,

    /// Verbose mode (repeat for more)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Color mode for CLI and log output
    #[arg(long, value_enum, default_value_t = ColorArg::Auto, global = true)]
    pub color: ColorArg,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Authentication commands
    Auth(AuthArgs),

    /// Create a backup archive
    Backup(BackupCreateArgs),

    /// Restore objects from an archive
    Restore(RestoreApplyArgs),

    /// List archive contents
    List(ListArgs),

    /// Show archive manifest
    Manifest(ManifestArgs),

    /// Compare two archives
    Diff(DiffArgs),

    /// Extract one object from an archive
    Extract(ExtractArgs),

    /// Export selected objects to an archive
    Export(BackupCreateArgs),

    /// Import objects from an archive
    Import(RestoreApplyArgs),

    /// Interactive archive browser (TUI)
    Inspect(InspectorArgs),
}

#[derive(Args, Debug)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommands,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommands {
    /// Perform interactive login with desktop app
    Login {
        #[arg(long)]
        force: bool,
    },

    /// Log out and clear api keys from memory and keystore
    Logout,

    /// Display authentication status
    Status,

    /// Set HTTP API token (read from stdin)
    SetHttp,

    /// Set gRPC credentials
    SetGrpc {
        /// Import gRPC credentials from headless config.json
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,

        /// Provide gRPC account key via stdin
        #[arg(long)]
        account_key: bool,

        /// Provide gRPC session token via stdin
        #[arg(long)]
        token: bool,
    },
}

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
    pub output: PathBuf,
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

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum ColorArg {
    Auto,
    Always,
    Never,
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
    pub json: bool,
}

pub fn parse_cli_from_env() -> Result<Cli> {
    let raw: Vec<OsString> = std::env::args_os().collect();
    validate_no_legacy_commands(&raw)?;
    match Cli::try_parse_from(&raw) {
        Ok(cli) => Ok(cli),
        Err(primary) => {
            let normalized = normalize_command_shortcuts(&raw);
            Cli::try_parse_from(&normalized).map_or_else(|_| Err(anyhow!(primary.to_string())), Ok)
        }
    }
}

fn validate_no_legacy_commands(args: &[OsString]) -> Result<()> {
    let command_index = find_top_level_command_index(args);
    let Some(idx) = command_index else {
        return Ok(());
    };
    let command = args[idx].to_string_lossy().to_string();
    let next = args.get(idx + 1).map(|v| v.to_string_lossy().to_string());
    if command == "backup" && next.as_deref() == Some("create") {
        bail!("legacy command removed: use `anyback backup ...` (without `create`)");
    }
    if command == "restore" && next.as_deref() == Some("apply") {
        bail!("legacy command removed: use `anyback restore ...` (without `apply`)");
    }
    if command == "archive" {
        match next.as_deref() {
            Some("inspect") => {
                bail!("command removed: use `anyback list` instead of `anyback archive inspect`")
            }
            Some("cmp") => {
                bail!("command removed: use `anyback diff` instead of `anyback archive cmp`")
            }
            Some("cp") => {
                bail!("command removed: use `anyback extract` instead of `anyback archive cp`")
            }
            _ => bail!(
                "command removed: `archive` subcommands replaced with `list`, `manifest`, `diff`, `extract`"
            ),
        }
    }
    if command == "info" {
        bail!(
            "command removed: use `anyback list` or `anyback manifest` instead of `anyback info`"
        );
    }
    Ok(())
}

fn find_top_level_command_index(args: &[OsString]) -> Option<usize> {
    let mut skip_value = false;
    for (idx, value) in args.iter().enumerate().skip(1) {
        if skip_value {
            skip_value = false;
            continue;
        }
        if let Some(s) = value.to_str() {
            if matches!(
                s,
                "-u" | "--url" | "--grpc" | "--keystore" | "--keystore-service" | "--color"
            ) {
                skip_value = true;
                continue;
            }
            if s.starts_with("--url=")
                || s.starts_with("--grpc=")
                || s.starts_with("--keystore=")
                || s.starts_with("--keystore-service=")
                || s.starts_with("--color=")
            {
                continue;
            }
            if !s.starts_with('-') {
                return Some(idx);
            }
        }
    }
    None
}

fn normalize_command_shortcuts(args: &[OsString]) -> Vec<OsString> {
    args.to_vec()
}

pub fn emit_json<T: Serialize>(value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    println!("{text}");
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

pub async fn run(cli: Cli) -> Result<()> {
    match &cli.command {
        Commands::List(args) => return handle_list(cli.json, args),
        Commands::Manifest(args) => return handle_manifest(cli.json, args),
        Commands::Diff(args) => return handle_diff(cli.json, args),
        Commands::Extract(args) => return handle_extract(cli.json, args),
        Commands::Inspect(args) => return inspector::run_inspector(&args.archive, args.max_cache),
        _ => {}
    }

    let ctx = AppContext {
        client: build_client(&cli)?,
        json: cli.json,
    };

    match cli.command {
        Commands::Auth(args) => auth::handle(&ctx, args).await,
        Commands::Backup(args) | Commands::Export(args) => handle_backup_create(&ctx, args).await,
        Commands::Restore(args) | Commands::Import(args) => handle_restore_apply(&ctx, args).await,
        Commands::List(_)
        | Commands::Manifest(_)
        | Commands::Diff(_)
        | Commands::Extract(_)
        | Commands::Inspect(_) => {
            unreachable!("handled above")
        }
    }
}

fn build_client(cli: &Cli) -> Result<AnytypeClient> {
    let config = ClientConfig {
        base_url: cli.url.clone(),
        keystore: cli.keystore.clone(),
        keystore_service: Some(
            cli.keystore_service
                .as_deref()
                .unwrap_or(DEFAULT_KEYRING_SERVICE)
                .into(),
        ),
        grpc_endpoint: cli.grpc.clone(),
        app_name: "anyback".into(),
        ..Default::default()
    };
    Ok(AnytypeClient::with_config(config)?)
}

async fn handle_backup_create(ctx: &AppContext, args: BackupCreateArgs) -> Result<()> {
    validate_backup_args(&args)?;
    let export_options = backup_export_options(&args);

    let progress = ProgressReporter::new(ctx.json, "Starting backup");
    let space = resolve_space(&ctx.client, &args.space).await?;
    let backup_target = resolve_backup_target(&args, &space.id)?;
    progress.set_message("Resolved destination space");

    progress.set_message("Collecting object metadata");
    let selection = resolve_backup_selection(ctx, &space, &args).await?;

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

    let backup = backup_builder
        .backup()
        .await
        .context("export request failed")?;
    finalize_backup_output_path(&backup.output_path, &backup_target.archive_path)?;
    progress.finish("Backup completed");

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
    };

    write_manifest_sidecar(&backup_target.archive_path, &manifest)?;
    // Ensure archive+manifest writes are flushed to disk before subsequent operations.
    sync_filesystem_after_archive_write();

    if ctx.json {
        emit_json(&serde_json::json!({
            "archive": backup_target.archive_path,
            "exported": backup.exported,
            "requested": manifest.objects.len(),
        }))?;
    } else {
        println!(
            "archive={} exported={}",
            backup_target.archive_path.display(),
            backup.exported
        );
    }

    Ok(())
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

fn finalize_backup_output_path(source: &Path, dest: &Path) -> Result<()> {
    if source == dest {
        return Ok(());
    }
    std::fs::rename(source, dest).with_context(|| {
        format!(
            "failed to move backup output from {} to {}",
            source.display(),
            dest.display()
        )
    })?;
    Ok(())
}

async fn handle_restore_apply(ctx: &AppContext, args: RestoreApplyArgs) -> Result<()> {
    let progress = ProgressReporter::new(ctx.json, "Starting restore");
    let (cancel_sender, mut cancel_state) = new_import_cancel_channel();
    let signal_forwarder = spawn_import_cancel_signal_forwarder(cancel_sender);
    let result = async {
        let archive = args.archive.as_path();
        let space_name_or_id = args
            .space
            .as_deref()
            .ok_or_else(|| anyhow!("--space is required"))?;
        let space = resolve_space(&ctx.client, space_name_or_id).await?;
        progress.set_message("Resolved destination space");
        let plan = build_import_plan(archive, args.objects.as_deref())?;
        if args.dry_run {
            progress.finish("Restore preflight completed");
            let payload = serde_json::json!({
                "dry_run": true,
                "archive": archive,
                "space_id": space.id,
                "requested": plan.selected_ids.len(),
                "manifest_present": plan.manifest.is_some(),
            });
            if ctx.json {
                emit_json(&payload)?;
            } else {
                println!(
                    "dry-run ok archive={} space={} requested={} manifest={}",
                    archive.display(),
                    space.id,
                    plan.selected_ids.len(),
                    if plan.manifest.is_some() {
                        "present"
                    } else {
                        "missing"
                    }
                );
            }
            return Ok(());
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
        let response = aggregate_import_responses(&execution.responses);
        report.event_progress = execution.event_progress;
        apply_import_response(
            &mut report,
            response,
            &plan.selected_ids,
            plan.manifest.as_ref(),
        );
        progress.finish("Restore completed");
        write_report(&report, args.log.as_deref())?;
        if ctx.json {
            emit_json(&report)?;
        } else {
            print_report_summary(&report);
        }
        Ok(())
    }
    .await;
    signal_forwarder.abort();
    result
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
    let manifest = read_manifest_from_archive(archive).ok();
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

fn parse_timeout_env_secs(name: &str, default: Duration) -> Result<Duration> {
    match std::env::var(name) {
        Ok(raw) => {
            let secs = raw
                .parse::<u64>()
                .with_context(|| format!("invalid {name} value: {raw}"))?;
            ensure!(secs > 0, "{name} must be > 0");
            Ok(Duration::from_secs(secs))
        }
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(err) => Err(anyhow!("failed to read {name}: {err}")),
    }
}

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

fn import_event_timeouts_from_env() -> Result<ProcessWatcherTimeouts> {
    let defaults = ProcessWatcherTimeouts::default();
    Ok(ProcessWatcherTimeouts {
        event_stream_connect_timeout: parse_timeout_env_secs(
            "ANYBACK_EVENT_STREAM_CONNECT_TIMEOUT",
            defaults.event_stream_connect_timeout,
        )?,
        process_start_timeout: parse_timeout_env_secs(
            "ANYBACK_PROCESS_START_TIMEOUT",
            defaults.process_start_timeout,
        )?,
        process_idle_timeout: parse_timeout_env_secs(
            "ANYBACK_PROCESS_IDLE_TIMEOUT",
            defaults.process_idle_timeout,
        )?,
        process_done_timeout: parse_timeout_env_secs(
            "ANYBACK_PROCESS_DONE_TIMEOUT",
            defaults.process_done_timeout,
        )?,
    })
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
    ctx: &AppContext,
    space_id: &str,
    batches: Vec<Vec<import_request::Snapshot>>,
    import_mode: ImportModeArg,
    replace_existing: bool,
    interactive_output: bool,
    cancel_state: &mut ImportCancelState,
) -> Result<ImportExecutionOutcome> {
    let grpc = ctx.client.grpc_client().await?;
    let mut commands = grpc.client_commands();
    let timeouts = import_event_timeouts_from_env()?;
    let mut tracker = ProcessWatcher::subscribe(&grpc, timeouts).await?;
    let watch_request = import_watch_request(space_id, interactive_output);
    let import_result: Result<_> = async {
        let mut responses = Vec::with_capacity(batches.len());
        for batch in batches {
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

            let response = commands
                .object_import(request)
                .await
                .context("object import RPC failed")
                .map(tonic::Response::into_inner)?;
            tracker
                .wait_for_process(&grpc, &watch_request, Some(cancel_state.receiver_mut()))
                .await
                .context("timed out waiting for import process completion event")?;
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
    ctx: &AppContext,
    space_id: &str,
    archive_path: &Path,
    import_mode: ImportModeArg,
    replace_existing: bool,
    interactive_output: bool,
    cancel_state: &mut ImportCancelState,
) -> Result<ImportExecutionOutcome> {
    let import_paths = pb_import_paths(archive_path)?;
    let grpc = ctx.client.grpc_client().await?;
    let mut commands = grpc.client_commands();
    let timeouts = import_event_timeouts_from_env()?;
    let mut tracker = ProcessWatcher::subscribe(&grpc, timeouts).await?;
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
        let request = with_token(tonic::Request::new(request), grpc.token())
            .map_err(|err| anyhow!("failed to attach gRPC token: {err}"))?;
        let response = commands
            .object_import(request)
            .await
            .context("object import RPC failed")
            .map(tonic::Response::into_inner)?;
        tracker
            .wait_for_process(&grpc, &watch_request, Some(cancel_state.receiver_mut()))
            .await
            .context("timed out waiting for import process completion event")?;
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
    ctx: &AppContext,
    space_id: &str,
    archive_path: &Path,
    explicit_object_selection: bool,
    _selected_ids: &[String],
    import_mode: ImportModeArg,
    replace_existing: bool,
    interactive_output: bool,
    cancel_state: &mut ImportCancelState,
) -> Result<ImportExecutionOutcome> {
    #[cfg(feature = "snapshot-import")]
    if explicit_object_selection {
        let limits = import_chunk_limits_from_env()?;
        let snapshots = collect_import_snapshots(archive_path, _selected_ids)?;
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
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        return Ok(vec![archive_path.to_string_lossy().to_string()]);
    }

    let mut paths = Vec::new();
    let include_files_dir = std::env::var("ANYBACK_PB_IMPORT_INCLUDE_FILES_DIR")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
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

fn handle_diff(json: bool, args: &DiffArgs) -> Result<()> {
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

    if json {
        emit_json(&report)?;
        return Ok(());
    }

    let archive1_label = archive_basename(&args.archive1);
    let archive2_label = archive_basename(&args.archive2);

    println!("< {archive1_label} only");
    for row in &report.archive1_only {
        println!(
            "< {} {} {} {} {}",
            row.object_id, row.r#type, row.name, row.size, row.last_modified
        );
    }
    println!();
    println!("> {archive2_label} only");
    for row in &report.archive2_only {
        println!(
            "> {} {} {} {} {}",
            row.object_id, row.r#type, row.name, row.size, row.last_modified
        );
    }
    println!();
    println!("* Changed");
    for row in &report.changed {
        println!(
            "< {} {} {} {} {}",
            row.left.object_id,
            row.left.r#type,
            row.left.name,
            row.left.size,
            row.left.last_modified
        );
        println!(
            "> {} {} {} {} {}",
            row.right.object_id,
            row.right.r#type,
            row.right.name,
            row.right.size,
            row.right.last_modified
        );
    }

    Ok(())
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

fn handle_list(json: bool, args: &ListArgs) -> Result<()> {
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

    if json {
        emit_json(&report)?;
        return Ok(());
    }

    print_list_summary(&report, inferred_object_ids.len());
    if args.files {
        for entry in files {
            println!("{} {}", entry.bytes, entry.path);
        }
    } else if let Some(entries) = expanded {
        print_expanded_entries(&entries);
    } else if !args.brief {
        for object_id in &inferred_object_ids {
            println!("{object_id}");
        }
    }
    Ok(())
}

fn handle_manifest(json: bool, args: &ManifestArgs) -> Result<()> {
    let reader = ArchiveReader::from_path(&args.archive)?;
    let (manifest, manifest_error) = read_manifest_prefer_sidecar(&args.archive, &reader);
    if let Some(manifest) = manifest {
        if json {
            emit_json(&manifest)?;
        } else {
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        Ok(())
    } else {
        if let Some(err) = manifest_error {
            bail!("manifest unreadable: {err}");
        }
        bail!("manifest not found in archive");
    }
}

fn print_list_summary(report: &ListReport, object_count: usize) {
    println!("archive: {}", report.archive);
    if let Some(summary) = report.manifest_summary.as_ref() {
        println!(
            "space: {} ({})",
            summary.source_space_name, summary.source_space_id
        );
        let created = summary
            .created_at_display
            .clone()
            .or_else(|| format_datetime_display(&summary.created_at))
            .unwrap_or_else(|| summary.created_at.clone());
        println!("created: {created}");
        println!("format: {}", summary.format);
    } else if let Some(err) = report.manifest_error.as_ref() {
        println!("manifest: unreadable ({err})");
    } else {
        println!("manifest: missing");
    }
    println!("objects: {object_count}");
    println!(
        "files: {} ({} bytes)",
        report.file_count, report.total_bytes
    );
}

fn print_expanded_entries(entries: &[ExpandedSnapshotEntry]) {
    let unreadable = entries.iter().filter(|e| e.status == "unreadable").count();
    println!(
        "expanded: parsed={} unreadable={}",
        entries.len().saturating_sub(unreadable),
        unreadable
    );
    for entry in entries {
        if entry.status == "unreadable" {
            println!(
                "unreadable path={} id={} reason={}",
                entry.path,
                entry.id.as_deref().unwrap_or("-"),
                entry.unreadable_reason.as_deref().unwrap_or("-")
            );
        } else {
            let object_type = entry
                .object_type
                .as_ref()
                .map_or_else(|| "null".to_string(), ToString::to_string);
            println!(
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
            );
        }
    }
}

fn handle_extract(json: bool, args: &ExtractArgs) -> Result<()> {
    let kind = save_archive_object(&args.archive, &args.object_id, &args.output)?;
    if json {
        emit_json(&serde_json::json!({
            "archive": args.archive,
            "object_id": args.object_id,
            "output": args.output,
            "kind": match kind {
                SavedObjectKind::Markdown => "markdown",
                SavedObjectKind::Raw => "raw",
            }
        }))?;
        return Ok(());
    }

    let label = match kind {
        SavedObjectKind::Markdown => "markdown",
        SavedObjectKind::Raw => "raw",
    };
    println!(
        "extracted object {} from {} to {} ({label})",
        args.object_id,
        args.archive.display(),
        args.output.display()
    );
    Ok(())
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

fn progress_enabled(json: bool, stderr_is_tty: bool) -> bool {
    !json && stderr_is_tty
}

struct ProgressReporter {
    bar: Option<ProgressBar>,
}

impl ProgressReporter {
    fn new(json: bool, message: &str) -> Self {
        let enabled = progress_enabled(json, io::stderr().is_terminal());
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

fn read_manifest_from_archive(path: &Path) -> Result<Manifest> {
    let (sidecar_manifest, sidecar_error) = read_manifest_from_sidecar(path);
    if let Some(manifest) = sidecar_manifest {
        return Ok(manifest);
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
        return Ok(manifest);
    }
    if let Some(err) = manifest_error {
        bail!("invalid manifest in archive {}: {err}", path.display());
    }
    bail!("manifest missing from archive {}", path.display())
}

fn write_manifest_sidecar(path: &Path, manifest: &Manifest) -> Result<()> {
    let text = serde_json::to_string_pretty(manifest)?;
    let sidecar_path = manifest_sidecar_path(path);
    if let Some(parent) = sidecar_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&sidecar_path, text)
        .with_context(|| format!("failed to write {}", sidecar_path.display()))?;
    Ok(())
}

fn sync_filesystem_after_archive_write() {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        nix::unistd::sync();
    }
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

fn print_report_summary(report: &ImportReport) {
    info!(
        "import summary: imported={} attempted={} failed={}",
        report.imported, report.attempted, report.failed
    );
    println!(
        "imported {}/{} objects (failed: {})",
        report.imported, report.attempted, report.failed
    );

    if !report.summary.is_empty() {
        for line in &report.summary {
            println!("- {line}");
        }
    }

    if report.failed > 0 {
        warn!("import completed with failures");
    }
}

fn write_report(report: &ImportReport, path: Option<&Path>) -> Result<()> {
    if let Some(path) = path {
        let text = serde_json::to_string_pretty(report)?;
        std::fs::write(path, text)
            .with_context(|| format!("failed to write report to {}", path.display()))?;
    }
    Ok(())
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

    use super::*;

    fn parse_user_cli(args: &[&str]) -> Cli {
        let raw: Vec<OsString> = args.iter().map(OsString::from).collect();
        let normalized = normalize_command_shortcuts(&raw);
        Cli::try_parse_from(normalized).unwrap()
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
            Commands::Backup(args) | Commands::Export(args) => args,
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
            "backup",
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
            "backup",
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
            "backup",
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
            "backup",
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
            "backup",
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
            "backup",
            "--space",
            "test",
            "--types",
            "page,note",
        ])
        .unwrap();
        if let Commands::Backup(args) = cli.command {
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
            assert_eq!(args.output, PathBuf::from("/tmp/out.md"));
        } else {
            panic!("expected extract command");
        }
    }

    #[test]
    fn validate_legacy_archive_rejected() {
        let args: Vec<OsString> = ["anyback", "archive", "inspect", "foo"]
            .iter()
            .map(OsString::from)
            .collect();
        let err = validate_no_legacy_commands(&args).unwrap_err();
        assert!(err.to_string().contains("anyback list"));
    }

    #[test]
    fn validate_legacy_info_rejected() {
        let args: Vec<OsString> = ["anyback", "info", "foo"]
            .iter()
            .map(OsString::from)
            .collect();
        let err = validate_no_legacy_commands(&args).unwrap_err();
        assert!(
            err.to_string().contains("anyback list")
                || err.to_string().contains("anyback manifest")
        );
    }

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
    fn normalize_short_command_is_identity() {
        let input = vec![
            OsString::from("anyback"),
            OsString::from("--url"),
            OsString::from("http://127.0.0.1:31009"),
            OsString::from("backup"),
            OsString::from("--space"),
            OsString::from("x"),
        ];
        let normalized = normalize_command_shortcuts(&input);
        let parts: Vec<String> = normalized
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            parts,
            vec![
                "anyback".to_string(),
                "--url".to_string(),
                "http://127.0.0.1:31009".to_string(),
                "backup".to_string(),
                "--space".to_string(),
                "x".to_string()
            ]
        );
    }

    #[test]
    fn parse_backup_create_rejects_removed_zip_flag() {
        let err =
            Cli::try_parse_from(["anyback", "backup", "--space", "test", "--zip"]).unwrap_err();
        assert!(err.to_string().contains("--zip"));
    }

    #[test]
    fn parse_backup_include_flags() {
        let cli = parse_user_cli(&[
            "anyback",
            "backup",
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
        if let Commands::Backup(args) = cli.command {
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
    fn reject_legacy_backup_create() {
        let err = validate_no_legacy_commands(&[
            OsString::from("anyback"),
            OsString::from("backup"),
            OsString::from("create"),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("legacy command removed"));
    }

    #[test]
    fn reject_legacy_restore_apply() {
        let err = validate_no_legacy_commands(&[
            OsString::from("anyback"),
            OsString::from("restore"),
            OsString::from("apply"),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("legacy command removed"));
    }

    #[test]
    fn parse_global_color_never() {
        let cli =
            Cli::try_parse_from(["anyback", "--color", "never", "list", "archive-dir"]).unwrap();
        assert_eq!(cli.color, ColorArg::Never);
    }

    #[test]
    fn parse_global_color_invalid_value() {
        let err = Cli::try_parse_from(["anyback", "--color", "badvalue", "list", "archive-dir"])
            .unwrap_err();
        assert!(err.to_string().contains("invalid value"));
    }

    #[test]
    fn progress_disabled_when_json_enabled() {
        assert!(!progress_enabled(true, true));
    }

    #[test]
    fn progress_disabled_for_non_tty() {
        assert!(!progress_enabled(false, false));
    }

    #[test]
    fn progress_enabled_for_tty_human_output() {
        assert!(progress_enabled(false, true));
    }

    #[test]
    fn progress_reporter_disabled_when_json_enabled() {
        let reporter = ProgressReporter::new(true, "hidden");
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
    fn build_import_plan_uses_archive_path_directly() {
        let temp = tempfile::tempdir().unwrap();
        let objects_dir = temp.path().join("objects");
        std::fs::create_dir_all(&objects_dir).unwrap();
        let id = "bafyreiaebddr63d7sye3eggmtkyeioqxftoaipobsynceksj6faedvd2xi";
        std::fs::write(objects_dir.join(format!("{id}.pb")), b"payload").unwrap();
        std::fs::write(temp.path().join("manifest.json"), r#"{"schema_version":1}"#).unwrap();

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

    #[test]
    fn parse_timeout_env_secs_rejects_zero() {
        let key = "ANYBACK_TEST_TIMEOUT_ENV";
        unsafe { std::env::set_var(key, "0") };
        let err = parse_timeout_env_secs(key, Duration::from_secs(5)).unwrap_err();
        unsafe { std::env::remove_var(key) };
        assert!(err.to_string().contains("must be > 0"));
    }

    #[test]
    fn parse_cache_size_defaults_to_mib() {
        assert_eq!(parse_cache_size("200").unwrap(), 200 * 1024 * 1024);
    }

    #[test]
    fn parse_cache_size_accepts_units_case_insensitive() {
        assert_eq!(parse_cache_size("1k").unwrap(), 1024);
        assert_eq!(parse_cache_size("2KB").unwrap(), 2 * 1024);
        assert_eq!(parse_cache_size("3m").unwrap(), 3 * 1024 * 1024);
        assert_eq!(parse_cache_size("4Mb").unwrap(), 4 * 1024 * 1024);
        assert_eq!(parse_cache_size("1G").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_cache_size_rejects_invalid_unit() {
        let err = parse_cache_size("10tb").unwrap_err();
        assert!(err.to_string().contains("unsupported cache size unit"));
    }

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
        assert!(paths.iter().any(|p| p.ends_with("/objects")));
        assert!(paths.iter().any(|p| p.ends_with("/top.pb")));
        assert!(!paths.iter().any(|p| p.ends_with("/manifest.json")));
    }

    #[test]
    fn pb_import_paths_skips_empty_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir(root.join("empty")).unwrap();
        std::fs::create_dir(root.join("objects")).unwrap();
        std::fs::write(root.join("objects").join("a.pb"), "pb").unwrap();

        let paths = pb_import_paths(root).unwrap();
        assert!(paths.iter().any(|p| p.ends_with("/objects")));
        assert!(!paths.iter().any(|p| p.ends_with("/empty")));
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
