/*
 * anyr - list, search, and manipulate anytype objects
 * github.com/stevelr/anytype
 *
 * SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */
use crate::output::{Output, OutputFormat};
use anyhow::{Result, bail};
use anytype::prelude::*;
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use tracing::warn;

pub mod auth;
pub mod common;
pub mod file;
pub mod list;
pub mod member;
pub mod object;
pub mod property;
pub mod search;
pub mod space;
pub mod tag;
pub mod template;
pub mod types;
pub mod view;

// default keyring service and default config subdir for storing key file
const DEFAULT_KEYRING_SERVICE: &str = "anyr"; // env!("CARGO_BIN_NAME");

/// date strftime-inspired format
/// Defined in https://docs.rs/chrono/latest/chrono/format/strftime/index.html
const DEFAULT_TABLE_DATE_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

#[derive(Parser, Debug)]
#[command(name = "anyr")]
#[command(author, version, about = "anyr: list, search, and manipulate Anytype objects", long_about = None)]
pub struct Cli {
    /// API endpoint URL. Default: environment $ANYTYPE_URL or http://127.0.0.1:31009 (desktop app)
    #[arg(short = 'u', long, env = "ANYTYPE_URL")]
    pub url: Option<String>,

    /// gRPC endpoint URL (overrides defaults)
    #[arg(long, env = "ANYTYPE_GRPC_ENDPOINT")]
    pub grpc: Option<String>,

    /// Write output to file (default: stdout)
    #[arg(short = 'o', long, value_name = "FILE", global = true)]
    pub output: Option<PathBuf>,

    /// JSON output (default)
    #[arg(short, long, global = true)]
    pub json: bool,

    /// Pretty-print JSON output
    #[arg(long, global = true)]
    pub pretty: bool,

    /// Table output format
    #[arg(short, long, global = true)]
    pub table: bool,

    /// Date format for table output, defined by [chrono-strftime format](https://docs.rs/chrono/latest/chrono/format/strftime/index.html). Defaults to "%Y-%m-%d %H:%M:%S"
    #[arg(long, env = "ANYTYPE_DATE_FORMAT", global = true)]
    pub date_format: Option<String>,

    /// Quiet mode - suppress output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Verbose mode (repeat for more: -v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global=true)]
    pub verbose: u8,

    /// keystore type or configuraion
    #[arg(long, env = "ANYTYPE_KEYSTORE")]
    pub keystore: Option<String>,

    /// Override service name (default "anyr")
    #[arg(long, env = "ANYTYPE_KEYSTORE_SERVICE")]
    pub keystore_service: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

// #[derive(Debug, Clone)]
// pub enum KeystoreConfig {
//     File(PathBuf),
//     Keyring(String),
// }

// impl KeystoreConfig {
//     pub fn description(&self) -> String {
//         match self {
//             KeystoreConfig::File(path) => format!("file ({})", path.display()),
//             KeystoreConfig::Keyring(service) => format!("keyring ({service})"),
//         }
//     }
// }

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Authentication commands
    Auth(AuthArgs),

    /// Space list and CRUD operations
    #[command(alias = "spaces")]
    Space(SpaceArgs),

    /// Object list and CRUD operations
    #[command(alias = "objects")]
    Object(ObjectArgs),

    /// File list and operations
    #[command(alias = "files")]
    File(FileArgs),

    /// Type list and CRUD operations
    #[command(alias = "types")]
    Type(TypeArgs),

    /// Property list and CRUD operations
    #[command(alias = "properties")]
    Property(PropertyArgs),

    /// Member operations
    #[command(alias = "members")]
    Member(MemberArgs),

    /// Tag list and CRUD operations
    #[command(alias = "tags")]
    Tag(TagArgs),

    /// Template list and operations
    #[command(alias = "templates")]
    Template(TemplateArgs),

    /// View operations
    #[command(alias = "views")]
    View(ViewArgs),

    /// Search - global or in-space
    Search(SearchArgs),

    /// List (collection or query) operations
    #[command(alias = "lists")]
    List(ListArgs),
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
pub struct SpaceArgs {
    #[command(subcommand)]
    pub command: SpaceCommands,
}

#[derive(Subcommand, Debug)]
pub enum SpaceCommands {
    List {
        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,
    },
    Get {
        /// space id or name (required)
        space: String,
    },
    Create {
        /// new space name (required)
        name: String,

        /// space description
        #[arg(long)]
        description: Option<String>,
    },
    Update {
        /// space id or name
        space: String,

        /// new space name
        #[arg(long)]
        name: Option<String>,

        /// new space description
        #[arg(long)]
        description: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct ObjectArgs {
    #[command(subcommand)]
    pub command: ObjectCommands,
}

#[derive(Args, Debug)]
pub struct FileArgs {
    #[command(subcommand)]
    pub command: FileCommands,
}

#[derive(Subcommand, Debug)]
pub enum FileCommands {
    List {
        /// space id or name
        space: String,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filters: FileFilterArgs,

        #[command(flatten)]
        filter: FilterArgs,
    },
    Search {
        /// space id or name
        space: String,

        /// search text (optional)
        #[arg(long)]
        text: Option<String>,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filters: FileFilterArgs,

        #[command(flatten)]
        filter: FilterArgs,
    },
    Get {
        /// space id or name
        space: String,

        /// id of file object to get
        object_id: String,
    },
    Update {
        /// space id or name
        space: String,

        /// id of file object to update
        object_id: String,

        /// new file name
        #[arg(long)]
        name: Option<String>,

        /// Set property (format: key=value)
        #[arg(short = 'p', long = "prop", value_name = "KEY=VALUE")]
        properties: Vec<String>,

        /// Set property (format: key=value)
        #[arg(value_name = "KEY=VALUE")]
        property_args: Vec<String>,
    },
    Delete {
        /// space id or name
        space: String,

        /// id of file object to delete
        object_id: String,
    },
    #[command(
        alias = "down",
        group = ArgGroup::new("download_destination")
            .args(["dir", "file"])
            .multiple(false)
    )]
    Download {
        /// id of file object to download
        object_id: String,

        /// output directory (optional)
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,

        /// output file path (optional)
        #[arg(short = 'f', long, value_name = "FILE")]
        file: Option<PathBuf>,
    },
    #[command(alias = "up")]
    Upload {
        /// space id or name
        space: String,

        /// input file path
        #[arg(short = 'f', long, value_name = "FILE")]
        file: PathBuf,

        /// file type hint
        #[arg(long, value_enum)]
        file_type: Option<FileTypeArg>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ObjectCommands {
    List {
        /// space id or name
        space: String,

        #[command(flatten)]
        pagination: PaginationArgs,

        /// filters to limit results
        #[command(flatten)]
        filter: FilterArgs,

        /// types to limit results
        #[arg(long = "type", value_name = "TYPE_KEY")]
        types: Vec<String>,
    },
    Get {
        /// space id or name
        space: String,

        /// id of object to get
        object_id: String,
    },
    Create {
        /// space id or name
        space: String,

        /// type of object to create. Must already be defined in space
        type_key: String,

        /// object name
        #[arg(long)]
        name: Option<String>,

        /// markdown body
        #[arg(long)]
        body: Option<String>,

        /// read markdown body from file
        #[arg(long)]
        body_file: Option<PathBuf>,

        /// set object's icon to an emoji
        #[arg(long)]
        icon_emoji: Option<String>,

        /// set object's icon from file
        #[arg(long)]
        icon_file: Option<PathBuf>,

        /// use template
        #[arg(long)]
        template: Option<String>,

        /// set description
        #[arg(long)]
        description: Option<String>,

        /// sets object's url (required for bookmark objects)
        #[arg(long)]
        url: Option<String>,

        /// Set property (format: key=value)
        #[arg(short = 'p', long = "prop", value_name = "KEY=VALUE")]
        properties: Vec<String>,

        /// Set property (format: key=value)
        #[arg(value_name = "KEY=VALUE")]
        property_args: Vec<String>,
    },
    Update {
        /// space id or name
        space: String,

        /// id of object to modify
        object_id: String,

        /// new object name
        #[arg(long)]
        name: Option<String>,

        /// new object markdown body
        #[arg(long)]
        body: Option<String>,

        /// new markdown from file
        #[arg(long)]
        body_file: Option<PathBuf>,

        /// new icon emoji
        #[arg(long)]
        icon_emoji: Option<String>,

        /// new icon from file
        #[arg(long)]
        icon_file: Option<PathBuf>,

        /// change object's type
        #[arg(long = "type")]
        type_key: Option<String>,

        /// Set property (format: key=value)
        #[arg(short = 'p', long = "prop", value_name = "KEY=VALUE")]
        properties: Vec<String>,

        /// Set property (format: key=value)
        #[arg(value_name = "KEY=VALUE")]
        property_args: Vec<String>,
    },
    Delete {
        /// space id or name
        space: String,

        /// id of object to delete
        object_id: String,
    },
}

#[derive(Args, Debug)]
pub struct TypeArgs {
    #[command(subcommand)]
    pub command: TypeCommands,
}

#[derive(Subcommand, Debug)]
pub enum TypeCommands {
    List {
        /// space id or name
        space: String,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,
    },
    Get {
        /// space id or name
        space: String,

        /// type id, name, or key
        type_id: String,
    },
    Create {
        /// space id or name
        space: String,

        /// type key (required)
        key: String,

        /// type name (required)
        name: String,

        /// plural name (defaults to name + 's')
        #[arg(long)]
        plural: Option<String>,

        /// set type emoji icon
        #[arg(long)]
        icon_emoji: Option<String>,

        /// set type layout
        #[arg(long, value_enum, default_value = "basic")]
        layout: TypeLayoutArg,

        /// set type properties
        #[arg(short = 'p', long = "prop", alias = "property", value_name = "SPEC")]
        properties: Vec<String>,
    },
    Update {
        /// space id or name
        space: String,

        /// id of type to update
        type_id: String,

        /// change type key
        #[arg(long)]
        key: Option<String>,

        /// change type name
        #[arg(long)]
        name: Option<String>,

        /// change type plural name
        #[arg(long)]
        plural: Option<String>,

        /// change type emoji icon
        #[arg(long)]
        icon_emoji: Option<String>,

        /// change type layout
        #[arg(long, value_enum)]
        layout: Option<TypeLayoutArg>,
    },
    Delete {
        /// space id or name
        space: String,

        /// id of type to delete
        type_id: String,
    },
}

#[derive(Clone, ValueEnum, Debug)]
pub enum TypeLayoutArg {
    /// standard object layout
    Basic,
    /// profile layout for user/contact information
    Profile,
    /// action/task layout
    Action,
    /// simplified note layout
    Note,
}

#[derive(Clone, ValueEnum, Debug)]
pub enum FileTypeArg {
    File,
    Image,
    Video,
    Audio,
    Pdf,
}

#[derive(Args, Debug)]
pub struct PropertyArgs {
    #[command(subcommand)]
    pub command: PropertyCommands,
}

#[derive(Subcommand, Debug)]
pub enum PropertyCommands {
    List {
        /// space id or name
        space: String,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,

        #[arg(long, value_enum)]
        format: Option<PropertyFormatArg>,
    },
    Get {
        /// space id or name
        space: String,

        /// property id or key
        property: String,
    },
    Create {
        /// space id or name
        space: String,
        /// new property name
        name: String,

        /// property format
        #[arg(value_enum)]
        format: PropertyFormatArg,

        /// property key (recommended), snake_case
        #[arg(long)]
        key: Option<String>,

        /// tags
        #[arg(long = "tag", value_name = "NAME:COLOR")]
        tags: Vec<String>,
    },
    Update {
        /// space id or name
        space: String,

        /// id or key of property to update
        property: String,

        /// change property name
        #[arg(long)]
        name: Option<String>,

        /// change property key
        #[arg(long)]
        key: Option<String>,
    },
    Delete {
        /// space id or name
        space: String,

        /// id or key of property to delete
        property: String,
    },
}

#[derive(Clone, ValueEnum, Debug)]
pub enum PropertyFormatArg {
    Text,
    Number,
    Select,
    #[value(alias = "multi_select")]
    MultiSelect,
    Date,
    Files,
    Checkbox,
    Url,
    Email,
    Phone,
    Objects,
}

#[derive(Args, Debug)]
pub struct MemberArgs {
    #[command(subcommand)]
    pub command: MemberCommands,
}

#[derive(Subcommand, Debug)]
pub enum MemberCommands {
    List {
        /// space id or name
        space: String,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,

        #[arg(long, value_enum)]
        role: Option<MemberRoleArg>,

        #[arg(long, value_enum)]
        status: Option<MemberStatusArg>,
    },
    Get {
        /// space id or name
        space: String,
        /// member id
        member_id: String,
    },
}

/// member role
#[derive(Clone, ValueEnum, Debug)]
pub enum MemberRoleArg {
    Viewer,
    Editor,
    Owner,
}

/// member status
#[derive(Clone, ValueEnum, Debug)]
pub enum MemberStatusArg {
    Joining,
    Active,
    Removed,
    Declined,
    Removing,
    Canceled,
}

#[derive(Args, Debug)]
pub struct TagArgs {
    #[command(subcommand)]
    pub command: TagCommands,
}

#[derive(Subcommand, Debug)]
pub enum TagCommands {
    List {
        /// space id or name
        space: String,

        /// property
        property_id: String,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,
    },
    Get {
        /// space id or name
        space: String,

        /// property id or key
        property_id: String,

        /// tag id or Name
        tag_id: String,
    },
    Create {
        /// space id or name
        space: String,

        /// property id
        property_id: String,

        /// tag name
        name: String,

        /// tag color
        #[arg(value_enum)]
        color: TagColorArg,

        /// tag key (recommended), snake_case
        #[arg(long)]
        key: Option<String>,
    },
    Update {
        /// space id or name
        space: String,

        /// property id
        property_id: String,

        /// tag id
        tag_id: String,

        /// change tag name
        #[arg(long)]
        name: Option<String>,

        /// change tag key
        #[arg(long)]
        key: Option<String>,

        /// change tag color
        #[arg(long, value_enum)]
        color: Option<TagColorArg>,
    },
    Delete {
        /// space id or name
        space: String,
        /// property id
        property_id: String,
        /// tag id
        tag_id: String,
    },
}

#[derive(Clone, ValueEnum, Debug)]
pub enum TagColorArg {
    Grey,
    Yellow,
    Orange,
    Red,
    Pink,
    Purple,
    Blue,
    Ice,
    Teal,
    Lime,
}

#[derive(Args, Debug)]
pub struct TemplateArgs {
    #[command(subcommand)]
    pub command: TemplateCommands,
}

#[derive(Args, Debug)]
pub struct ViewArgs {
    #[command(subcommand)]
    pub command: ViewCommands,
}

#[derive(Subcommand, Debug)]
pub enum ViewCommands {
    /// List objects for a view
    Objects {
        /// View ID
        #[arg(long)]
        view: String,
        /// Column keys for table output (comma-separated)
        #[arg(long, alias = "cols")]
        columns: Option<String>,
        /// Space ID
        space: String,
        /// Type ID (list id)
        type_id: String,
        /// Limit number of items
        #[arg(long, default_value = "100")]
        limit: usize,
    },
}

#[derive(Subcommand, Debug)]
pub enum TemplateCommands {
    List {
        /// space id or name
        space: String,

        /// type the template applies to
        type_id: String,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,
    },
    Get {
        /// space id or name
        space: String,
        /// type the template applies to
        type_id: String,
        /// template id
        template_id: String,
    },
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// search within a space (default: global across all available spaces)
    #[arg(long)]
    pub space: Option<String>,

    /// search for text in title or markdown body
    #[arg(long)]
    pub text: Option<String>,

    /// Limit search to types (type_key). Repeat to include multiple types
    #[arg(long = "type", value_name = "type")]
    pub types: Vec<String>,

    #[command(flatten)]
    pub pagination: PaginationArgs,

    #[command(flatten)]
    pub filter: FilterArgs,

    #[command(flatten)]
    pub sort: SortArgs,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    #[command(subcommand)]
    pub command: ListCommands,
}

#[derive(Subcommand, Debug)]
pub enum ListCommands {
    Objects {
        /// space id or name (required)
        space: String,

        /// list or collection id, or type id/name/key
        list_id: String,

        /// optional view name or id
        #[arg(long)]
        view: Option<String>,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,
    },
    Views {
        /// space id or name (required)
        space: String,

        /// list/collection id, or type id/name/key (required)
        list_id: String,

        #[command(flatten)]
        pagination: PaginationArgs,
    },
    Add {
        /// space id or name (required)
        space: String,

        /// list (collection) id
        list_id: String,

        /// ids of objects to add
        #[arg(required = true)]
        object_ids: Vec<String>,
    },
    Remove {
        /// space id or name (required)
        space: String,

        /// list (collection) id
        list_id: String,

        /// id of object to remove (required)
        object_id: String,
    },
}

#[derive(Args, Debug)]
pub struct PaginationArgs {
    /// limit results to n items (default 100, max 1000)
    #[arg(long, default_value = "100")]
    pub limit: usize,

    /// return results starting with offset (for continuation of previous search)
    #[arg(long, default_value = "0")]
    pub offset: usize,

    /// collect all results from all pages
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct FilterArgs {
    /// add filter(s) to results
    #[arg(long = "filter", value_name = "FILTER")]
    pub filters: Vec<String>,
}

#[derive(Args, Debug)]
pub struct FileFilterArgs {
    /// filter by name substring
    #[arg(long)]
    pub name_contains: Option<String>,

    /// filter by file type
    #[arg(long, value_enum)]
    pub file_type: Option<FileTypeArg>,

    /// filter by file extension
    #[arg(long, value_name = "EXT")]
    pub ext: Option<String>,

    /// filter by file extension list
    #[arg(long, value_name = "EXT", value_delimiter = ',')]
    pub ext_in: Vec<String>,

    /// filter by excluding file extension list
    #[arg(long, value_name = "EXT", value_delimiter = ',')]
    pub ext_nin: Vec<String>,

    /// filter by size equals (bytes)
    #[arg(long, value_name = "BYTES")]
    pub size_eq: Option<i64>,

    /// filter by size not equals (bytes)
    #[arg(long, value_name = "BYTES")]
    pub size_neq: Option<i64>,

    /// filter by size less than (bytes)
    #[arg(long, value_name = "BYTES")]
    pub size_lt: Option<i64>,

    /// filter by size less than or equal (bytes)
    #[arg(long, value_name = "BYTES")]
    pub size_lte: Option<i64>,

    /// filter by size greater than (bytes)
    #[arg(long, value_name = "BYTES")]
    pub size_gt: Option<i64>,

    /// filter by size greater than or equal (bytes)
    #[arg(long, value_name = "BYTES")]
    pub size_gte: Option<i64>,
}

#[derive(Args, Debug)]
pub struct SortArgs {
    /// sort results by property key
    #[arg(long, value_name = "property_key")]
    pub sort: Option<String>,

    /// descending sort (default: ascending)
    #[arg(long)]
    pub desc: bool,
}

pub struct AppContext {
    pub client: AnytypeClient,
    pub output: Output,
    //pub base_url: String,
    pub date_format: String,
}

pub async fn run(cli: Cli) -> Result<()> {
    let output = Output::new(resolve_output_format(&cli), cli.output.clone());
    let date_format = resolve_table_date_format(&cli);

    let client = build_client(&cli)?;

    let ctx = AppContext {
        //base_url: client.get_http_endpoint().to_string(),
        client,
        output,
        date_format,
    };

    match cli.command {
        Commands::Auth(args) => auth::handle(&ctx, args).await,
        Commands::Space(args) => space::handle(&ctx, args).await,
        Commands::Object(args) => object::handle(&ctx, args).await,
        Commands::File(args) => file::handle(&ctx, args).await,
        Commands::Type(args) => types::handle(&ctx, args).await,
        Commands::Property(args) => property::handle(&ctx, args).await,
        Commands::Member(args) => member::handle(&ctx, args).await,
        Commands::Tag(args) => tag::handle(&ctx, args).await,
        Commands::Template(args) => template::handle(&ctx, args).await,
        Commands::View(args) => view::handle(&ctx, args).await,
        Commands::Search(args) => search::handle(&ctx, args).await,
        Commands::List(args) => list::handle(&ctx, args).await,
    }
}

fn resolve_output_format(cli: &Cli) -> OutputFormat {
    if cli.quiet {
        OutputFormat::Quiet
    } else if cli.pretty {
        if cli.table {
            warn!("--pretty conflicts with --table. Using json pretty format");
        }
        OutputFormat::Pretty
    } else if cli.json {
        if cli.table {
            warn!("--json conflicts with --table. Using json format");
        }
        OutputFormat::Json
    } else if cli.table {
        OutputFormat::Table
    } else {
        OutputFormat::Json
    }
}

fn resolve_table_date_format(cli: &Cli) -> String {
    cli.date_format
        .clone()
        .unwrap_or_else(|| DEFAULT_TABLE_DATE_FORMAT.to_string())
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
        app_name: "anyr".into(), // env!("CARGO_BIN_NAME"),
        ..Default::default()
    };
    let client = AnytypeClient::with_config(config)?;
    Ok(client)
}

pub fn ensure_authenticated(_client: &AnytypeClient) -> Result<()> {
    // TODO: do we need this method anymore?
    // if client.load_key(false)? {
    //     return Ok(());
    // }
    // Err(AnytypeError::Unauthorized.into())
    Ok(())
}

// fn default_keyfile_path() -> PathBuf {
//     dirs::config_dir()
//         .unwrap_or_else(|| PathBuf::from("."))
//         .join(DEFAULT_KEYRING_SERVICE)
//         .join("api.key")
// }

impl TypeLayoutArg {
    pub fn to_layout(&self) -> TypeLayout {
        match self {
            TypeLayoutArg::Basic => TypeLayout::Basic,
            TypeLayoutArg::Profile => TypeLayout::Profile,
            TypeLayoutArg::Action => TypeLayout::Action,
            TypeLayoutArg::Note => TypeLayout::Note,
        }
    }
}

impl PropertyFormatArg {
    pub fn to_format(&self) -> PropertyFormat {
        match self {
            PropertyFormatArg::Text => PropertyFormat::Text,
            PropertyFormatArg::Number => PropertyFormat::Number,
            PropertyFormatArg::Select => PropertyFormat::Select,
            PropertyFormatArg::MultiSelect => PropertyFormat::MultiSelect,
            PropertyFormatArg::Date => PropertyFormat::Date,
            PropertyFormatArg::Files => PropertyFormat::Files,
            PropertyFormatArg::Checkbox => PropertyFormat::Checkbox,
            PropertyFormatArg::Url => PropertyFormat::Url,
            PropertyFormatArg::Email => PropertyFormat::Email,
            PropertyFormatArg::Phone => PropertyFormat::Phone,
            PropertyFormatArg::Objects => PropertyFormat::Objects,
        }
    }
}

impl MemberRoleArg {
    pub fn to_role(&self) -> MemberRole {
        match self {
            MemberRoleArg::Viewer => MemberRole::Viewer,
            MemberRoleArg::Editor => MemberRole::Editor,
            MemberRoleArg::Owner => MemberRole::Owner,
        }
    }
}

impl MemberStatusArg {
    pub fn to_status(&self) -> MemberStatus {
        match self {
            MemberStatusArg::Joining => MemberStatus::Joining,
            MemberStatusArg::Active => MemberStatus::Active,
            MemberStatusArg::Removed => MemberStatus::Removed,
            MemberStatusArg::Declined => MemberStatus::Declined,
            MemberStatusArg::Removing => MemberStatus::Removing,
            MemberStatusArg::Canceled => MemberStatus::Canceled,
        }
    }
}

impl TagColorArg {
    pub fn to_color(&self) -> Color {
        match self {
            TagColorArg::Grey => Color::Grey,
            TagColorArg::Yellow => Color::Yellow,
            TagColorArg::Orange => Color::Orange,
            TagColorArg::Red => Color::Red,
            TagColorArg::Pink => Color::Pink,
            TagColorArg::Purple => Color::Purple,
            TagColorArg::Blue => Color::Blue,
            TagColorArg::Ice => Color::Ice,
            TagColorArg::Teal => Color::Teal,
            TagColorArg::Lime => Color::Lime,
        }
    }
}

pub fn pagination_limit(pagination: &PaginationArgs) -> usize {
    if pagination.all {
        1000
    } else {
        pagination.limit
    }
}

pub fn pagination_offset(pagination: &PaginationArgs) -> usize {
    pagination.offset
}

pub fn must_have_body(
    body: &Option<String>,
    body_file: &Option<PathBuf>,
) -> Result<Option<String>> {
    if body.is_some() && body_file.is_some() {
        bail!("--body and --body-file are mutually exclusive");
    }
    if let Some(body) = body {
        return Ok(Some(body.clone()));
    }
    if let Some(path) = body_file {
        let content = std::fs::read_to_string(path)
            .map_err(|err| anyhow::anyhow!("read {}: {err}", path.display()))?;
        return Ok(Some(content));
    }
    Ok(None)
}

pub fn resolve_icon(emoji: &Option<String>, file: &Option<PathBuf>) -> Result<Option<Icon>> {
    if emoji.is_some() && file.is_some() {
        bail!("--icon-emoji and --icon-file are mutually exclusive");
    }
    if let Some(emoji) = emoji {
        return Ok(Some(Icon::Emoji {
            emoji: emoji.clone(),
        }));
    }
    if let Some(path) = file {
        return Ok(Some(Icon::File {
            file: path.display().to_string(),
        }));
    }
    Ok(None)
}
