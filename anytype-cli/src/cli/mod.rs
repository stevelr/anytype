use crate::config::CliConfig;
use crate::output::{Output, OutputFormat};
use anyhow::{Result, bail};
use anytype::prelude::*;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use tracing::warn;

pub mod auth;
pub mod common;
pub mod config;
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
const DEFAULT_KEYRING_SERVICE: &str = env!("CARGO_BIN_NAME");

#[derive(Parser, Debug)]
#[command(name = "anyr")]
#[command(author, version, about = "Anytype CLI", long_about = None)]
#[command(
    after_help = "Logging:\n  RUST_LOG=warn,anytype::http_json=debug   Log JSON requests/responses\n  RUST_LOG=info                               Default CLI info logs\n"
)]
pub struct Cli {
    /// API endpoint URL
    #[arg(short = 'u', long, env = "ANYTYPE_URL")]
    pub url: Option<String>,

    /// Write output to file (default: stdout)
    #[arg(short, long, value_name = "FILE", global = true)]
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

    /// Date format for table output
    #[arg(long, env = "ANYTYPE_DATE_FORMAT", global = true)]
    pub date_format: Option<String>,

    /// Quiet mode - suppress output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Verbose mode (repeat for more: -v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global=true)]
    pub verbose: u8,

    /// API Key storage
    #[command(flatten)]
    pub keystore: KeystoreArgs,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Args, Debug)]
#[group(multiple = false)]
pub struct KeystoreArgs {
    /// Use default keyfile (~/.config/anytype/credentials)
    #[arg(long)]
    pub keyfile: bool,

    /// Use keyfile at specified path
    #[arg(long, value_name = "PATH")]
    pub keyfile_path: Option<PathBuf>,

    /// Use OS keyring with default service ("anytype_rust")
    #[arg(long)]
    pub keyring: bool,

    /// Use OS keyring with specified service name
    #[arg(long, value_name = "SERVICE")]
    pub keyring_service: Option<String>,
}

impl KeystoreArgs {
    pub fn resolve(&self, config: Option<&CliConfig>) -> KeystoreConfig {
        if self.keyfile {
            return KeystoreConfig::File(default_keyfile_path());
        }
        if let Some(path) = &self.keyfile_path {
            return KeystoreConfig::File(path.clone());
        }
        if self.keyring {
            return KeystoreConfig::Keyring(DEFAULT_KEYRING_SERVICE.to_string());
        }
        if let Some(service) = &self.keyring_service {
            return KeystoreConfig::Keyring(service.clone());
        }

        if let Ok(val) = std::env::var("ANYTYPE_KEY_FILE") {
            if val == "1" || val.eq_ignore_ascii_case("true") {
                return KeystoreConfig::File(default_keyfile_path());
            }
            return KeystoreConfig::File(PathBuf::from(val));
        }
        if let Ok(val) = std::env::var("ANYTYPE_KEYSTORE_KEYRING") {
            if val == "1" || val.eq_ignore_ascii_case("true") {
                return KeystoreConfig::Keyring(DEFAULT_KEYRING_SERVICE.to_string());
            }
            return KeystoreConfig::Keyring(val);
        }

        if let Some(config) = config
            && let Some(keystore) = config.keystore.as_deref()
        {
            if keystore.eq_ignore_ascii_case("file") {
                return KeystoreConfig::File(default_keyfile_path());
            }
            if let Some(rest) = keystore.strip_prefix("file:") {
                return KeystoreConfig::File(PathBuf::from(rest));
            }
            if keystore.eq_ignore_ascii_case("keyring") {
                return KeystoreConfig::Keyring(DEFAULT_KEYRING_SERVICE.to_string());
            }
            if let Some(rest) = keystore.strip_prefix("keyring:") {
                return KeystoreConfig::Keyring(rest.to_string());
            }
        }

        KeystoreConfig::File(default_keyfile_path())
    }
}

#[derive(Debug, Clone)]
pub enum KeystoreConfig {
    File(PathBuf),
    Keyring(String),
}

impl KeystoreConfig {
    pub fn description(&self) -> String {
        match self {
            KeystoreConfig::File(path) => format!("file ({})", path.display()),
            KeystoreConfig::Keyring(service) => format!("keyring ({service})"),
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Authentication
    Auth(AuthArgs),

    /// Space list and CRUD operations
    #[command(alias = "spaces")]
    Space(SpaceArgs),

    /// Object list and CRUD operations
    #[command(alias = "objects")]
    Object(ObjectArgs),

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

    /// List operations
    #[command(alias = "lists")]
    List(ListArgs),
    Config(ConfigArgs),
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
        space_id: String,
    },
    Create {
        name: String,

        #[arg(long)]
        description: Option<String>,
    },
    Update {
        space_id: String,

        #[arg(long)]
        name: Option<String>,

        #[arg(long)]
        description: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct ObjectArgs {
    #[command(subcommand)]
    pub command: ObjectCommands,
}

#[derive(Subcommand, Debug)]
pub enum ObjectCommands {
    List {
        space_id: String,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,

        #[arg(long = "type", value_name = "TYPE_KEY")]
        types: Vec<String>,
    },
    Get {
        space_id: String,
        object_id: String,
    },
    Create {
        space_id: String,
        type_key: String,

        #[arg(long)]
        name: Option<String>,

        #[arg(long)]
        body: Option<String>,

        #[arg(long)]
        body_file: Option<PathBuf>,

        #[arg(long)]
        icon_emoji: Option<String>,

        #[arg(long)]
        icon_file: Option<PathBuf>,

        #[arg(long)]
        template: Option<String>,

        #[arg(long)]
        description: Option<String>,

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
        space_id: String,
        object_id: String,

        #[arg(long)]
        name: Option<String>,

        #[arg(long)]
        body: Option<String>,

        #[arg(long)]
        body_file: Option<PathBuf>,

        #[arg(long)]
        icon_emoji: Option<String>,

        #[arg(long)]
        icon_file: Option<PathBuf>,

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
        space_id: String,
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
        space_id: String,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,
    },
    Get {
        space_id: String,
        type_id: String,
    },
    Create {
        space_id: String,
        key: String,
        name: String,

        #[arg(long)]
        plural: Option<String>,

        #[arg(long)]
        icon_emoji: Option<String>,

        #[arg(long, value_enum, default_value = "basic")]
        layout: TypeLayoutArg,

        #[arg(short = 'p', long = "prop", alias = "property", value_name = "SPEC")]
        properties: Vec<String>,
    },
    Update {
        space_id: String,
        type_id: String,

        #[arg(long)]
        key: Option<String>,

        #[arg(long)]
        name: Option<String>,

        #[arg(long)]
        plural: Option<String>,

        #[arg(long)]
        icon_emoji: Option<String>,

        #[arg(long, value_enum)]
        layout: Option<TypeLayoutArg>,
    },
    Delete {
        space_id: String,
        type_id: String,
    },
}

#[derive(Clone, ValueEnum, Debug)]
pub enum TypeLayoutArg {
    Basic,
    Profile,
    Action,
    Note,
}

#[derive(Args, Debug)]
pub struct PropertyArgs {
    #[command(subcommand)]
    pub command: PropertyCommands,
}

#[derive(Subcommand, Debug)]
pub enum PropertyCommands {
    List {
        space_id: String,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,

        #[arg(long, value_enum)]
        format: Option<PropertyFormatArg>,
    },
    Get {
        space_id: String,
        property_id: String,
    },
    Create {
        space_id: String,
        name: String,
        #[arg(value_enum)]
        format: PropertyFormatArg,

        #[arg(long)]
        key: Option<String>,

        #[arg(long = "tag", value_name = "NAME:COLOR")]
        tags: Vec<String>,
    },
    Update {
        space_id: String,
        property_id: String,

        #[arg(long)]
        name: Option<String>,

        #[arg(long)]
        key: Option<String>,
    },
    Delete {
        space_id: String,
        property_id: String,
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
        space_id: String,

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
        space_id: String,
        member_id: String,
    },
}

#[derive(Clone, ValueEnum, Debug)]
pub enum MemberRoleArg {
    Viewer,
    Editor,
    Owner,
}

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
        space_id: String,
        property_id: String,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,
    },
    Get {
        space_id: String,
        /// property id or key
        property_id: String,
        /// tag id or Name
        tag_id: String,
    },
    Create {
        space_id: String,
        property_id: String,
        name: String,
        #[arg(value_enum)]
        color: TagColorArg,

        #[arg(long)]
        key: Option<String>,
    },
    Update {
        space_id: String,
        property_id: String,
        tag_id: String,

        #[arg(long)]
        name: Option<String>,

        #[arg(long)]
        key: Option<String>,

        #[arg(long, value_enum)]
        color: Option<TagColorArg>,
    },
    Delete {
        space_id: String,
        property_id: String,
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
        /// List objects for a view, showing only view columns
        Objects {
            /// View ID
            #[arg(long)]
            view: String,
            /// Column keys for table output (comma-separated)
            #[arg(long, alias = "cols")]
            columns: Option<String>,
            /// Space ID
            space_id: String,
            /// Type ID (list id)
            type_id: String,
            /// Limit number of items
        #[arg(long, default_value = "100")]
        limit: usize,
        /// gRPC server address
        #[arg(long, default_value = "http://127.0.0.1:31010")]
        grpc_addr: String,
        /// gRPC session token (overrides config lookup)
        #[arg(long)]
        grpc_token: Option<String>,
        /// Path to Anytype config.json (defaults to ~/.anytype/config.json)
        #[arg(long)]
        grpc_config: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum TemplateCommands {
    List {
        space_id: String,
        type_id: String,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,
    },
    Get {
        space_id: String,
        type_id: String,
        template_id: String,
    },
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    #[arg(long)]
    pub space_id: Option<String>,

    #[arg(long)]
    pub text: Option<String>,

    #[arg(long = "type", value_name = "TYPE_KEY")]
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
        space_id: String,
        list_id: String,

        #[arg(long)]
        view: Option<String>,

        #[command(flatten)]
        pagination: PaginationArgs,

        #[command(flatten)]
        filter: FilterArgs,
    },
    Views {
        space_id: String,
        list_id: String,

        #[command(flatten)]
        pagination: PaginationArgs,
    },
    Add {
        space_id: String,
        list_id: String,
        #[arg(required = true)]
        object_ids: Vec<String>,
    },
    Remove {
        space_id: String,
        list_id: String,
        object_id: String,
    },
}

#[derive(Args, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommands,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommands {
    Show,
    Set { key: ConfigKeyArg, value: String },
    Reset,
}

#[derive(Clone, ValueEnum, Debug)]
pub enum ConfigKeyArg {
    Url,
    Keystore,
    DefaultSpace,
}

#[derive(Args, Debug)]
pub struct PaginationArgs {
    #[arg(long, default_value = "100")]
    pub limit: usize,

    #[arg(long, default_value = "0")]
    pub offset: usize,

    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct FilterArgs {
    #[arg(long = "filter", value_name = "FILTER")]
    pub filters: Vec<String>,
}

#[derive(Args, Debug)]
pub struct SortArgs {
    #[arg(long)]
    pub sort: Option<String>,

    #[arg(long)]
    pub desc: bool,
}

pub struct AppContext {
    pub client: AnytypeClient,
    pub output: Output,
    pub base_url: String,
    pub keystore: KeystoreConfig,
    pub date_format: String,
}

pub async fn run(cli: Cli) -> Result<()> {
    let config = CliConfig::load()?;
    let output = Output::new(resolve_output_format(&cli), cli.output.clone());
    let date_format = resolve_table_date_format(&cli);

    if let Commands::Config(args) = &cli.command {
        return config::handle(args, &output).await;
    }

    let base_url = cli
        .url
        .clone()
        .or_else(|| config.url.clone())
        .unwrap_or_else(|| ANYTYPE_DESKTOP_URL.to_string());

    let keystore = cli.keystore.resolve(Some(&config));
    let client = build_client(&base_url, &keystore)?;
    let ctx = AppContext {
        client,
        output,
        base_url,
        keystore,
        date_format,
    };

    match cli.command {
        Commands::Auth(args) => auth::handle(&ctx, args).await,
        Commands::Space(args) => space::handle(&ctx, args).await,
        Commands::Object(args) => object::handle(&ctx, args).await,
        Commands::Type(args) => types::handle(&ctx, args).await,
        Commands::Property(args) => property::handle(&ctx, args).await,
        Commands::Member(args) => member::handle(&ctx, args).await,
        Commands::Tag(args) => tag::handle(&ctx, args).await,
        Commands::Template(args) => template::handle(&ctx, args).await,
        Commands::View(args) => view::handle(&ctx, args).await,
        Commands::Search(args) => search::handle(&ctx, args).await,
        Commands::List(args) => list::handle(&ctx, args).await,
        Commands::Config(_) => Ok(()),
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

const DEFAULT_TABLE_DATE_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

fn resolve_table_date_format(cli: &Cli) -> String {
    cli.date_format
        .clone()
        .unwrap_or_else(|| DEFAULT_TABLE_DATE_FORMAT.to_string())
}

fn build_client(base_url: &str, keystore: &KeystoreConfig) -> Result<AnytypeClient> {
    let mut config = ClientConfig::default().app_name("anytype-cli");
    config.base_url = base_url.to_string();

    let client = AnytypeClient::with_config(config)?;
    let client = match keystore {
        KeystoreConfig::File(path) => {
            let store = KeyStoreFile::from_path(path)?;
            client.set_key_store(store)
        }
        KeystoreConfig::Keyring(service) => {
            client.set_key_store(KeyStoreKeyring::new(service, None))
        }
    };

    Ok(client)
}

pub fn ensure_authenticated(client: &AnytypeClient) -> Result<()> {
    if client.load_key(false)? {
        return Ok(());
    }
    Err(AnytypeError::Unauthorized.into())
}

fn default_keyfile_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_KEYRING_SERVICE)
        .join("api.key")
}

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
