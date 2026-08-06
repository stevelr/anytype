/*
 * anyr - list, search, and manipulate anytype objects
 * github.com/stevelr/anytype
 *
 * SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */
use std::path::{Path, PathBuf};

#[cfg(feature = "mcp")]
use std::ffi::OsString;

use anyhow::{Context, Result, bail};
use anytype::prelude::*;
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use tracing::warn;

use crate::{
    cli::chat::{ChatReadTypeArg, MessageStyleArg, TransportArg},
    output::{Output, OutputFormat},
};

pub mod auth;
pub mod chat;
pub mod common;
pub mod file;
pub mod init_cli;
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
const HEADLESS_HTTP_URL: &str = "http://127.0.0.1:31012";
const HEADLESS_GRPC_ENDPOINT: &str = "http://127.0.0.1:31010";

/// date strftime-inspired format
/// Defined in <https://docs.rs/chrono/latest/chrono/format/strftime/index.html>
const DEFAULT_TABLE_DATE_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

#[derive(Parser, Debug)]
#[command(name = "anyr")]
#[command(author, version, about = "anyr: list, search, and manipulate Anytype objects", long_about = None)]
#[allow(clippy::struct_excessive_bools)]
pub struct Cli {
    /// API endpoint URL. Default: environment `ANYTYPE_URL` or <http://127.0.0.1:31009> (desktop app)
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

    /// keystore type or configuration
    #[arg(long, env = "ANYTYPE_KEYSTORE", global = true)]
    pub keystore: Option<String>,

    /// Override service name (default "anyr")
    #[arg(long, env = "ANYTYPE_KEYSTORE_SERVICE")]
    pub keystore_service: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Initialize credentials from a running Anytype CLI server
    InitCli {
        /// Join a space with this invitation link after credentials are stored
        #[arg(long, value_name = "INVITE_LINK")]
        join: Option<String>,
    },

    /// Authentication commands
    Auth(AuthArgs),

    /// Chat commands (gRPC)
    #[command(alias = "chats")]
    Chat(ChatArgs),

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

    /// Markdown document editing commands
    #[command(subcommand)]
    Md(any_edit::Commands),

    /// Backup, restore, and inspect archive commands
    #[cfg(feature = "backup")]
    #[command(subcommand)]
    Backup(anyback_reader::cli::Commands),

    /// Run the bounded Anytype MCP server or its maintenance commands
    #[cfg(feature = "mcp")]
    Mcp(McpArgs),
}

/// Arguments passed through unchanged to the embedded any-mcp process.
#[cfg(feature = "mcp")]
#[derive(Args, Debug, Clone)]
pub struct McpArgs {
    /// any-mcp process arguments (`init` and `check` are aliases for `config init` and `config check`)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub arguments: Vec<OsString>,
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

        /// Derive gRPC credentials from BIP39 mnemonic (12 words via stdin)
        #[arg(long)]
        bip39: bool,
    },

    /// Discover Anytype gRPC listening port
    FindGrpc {
        /// Program name prefix to match in lsof (default: "anytype")
        #[arg(long, default_value = "anytype")]
        program: String,
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

        /// create a chat space instead of a regular space
        #[arg(long)]
        chat: bool,
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
    /// Count archived objects in a space
    CountArchived {
        /// space id or name
        space: String,
    },
    /// Permanently delete all archived objects in a space
    DeleteArchived {
        /// space id or name
        space: String,

        /// skip confirmation prompt
        #[arg(long)]
        confirm: bool,
    },

    /// Permanently delete a space after interactive confirmation
    Delete {
        /// space id or name
        space: String,
    },

    /// Manage space invitations
    Invite(InviteArgs),

    /// Enable sharing for a space
    EnableSharing {
        /// space id or name
        space: String,
    },

    /// Disable sharing for a space
    DisableSharing {
        /// space id or name
        space: String,
    },
}

/// Arguments for space invitation operations.
#[derive(Args, Debug)]
pub struct InviteArgs {
    #[command(subcommand)]
    pub command: InviteCommands,
}

/// Space invitation operations.
#[derive(Subcommand, Debug)]
pub enum InviteCommands {
    /// Show active member and guest invitations
    Show {
        /// space id or name
        space: String,
    },

    /// Create a new space invitation
    Create {
        /// space id or name
        space: String,

        /// Grant reader permissions
        #[arg(long, group = "permissions")]
        reader: bool,

        /// Grant writer permissions
        #[arg(long, group = "permissions")]
        writer: bool,

        /// Grant owner permissions
        #[arg(long, group = "permissions")]
        owner: bool,

        /// Create a guest invitation
        #[arg(long, group = "approval")]
        guest: bool,

        /// Require approval before a member joins
        #[arg(long, group = "approval")]
        with_approval: bool,

        /// Allow a member to join without approval
        #[arg(long, group = "approval")]
        auto_approve: bool,
    },

    /// Revoke the active space invitation
    Revoke {
        /// space id or name
        space: String,
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

        /// sort results by property key (for example `name` or `last_modified_date`)
        #[arg(long, value_name = "PROPERTY")]
        sort: Option<String>,

        /// sort in descending order (default ascending); requires --sort
        #[arg(long, requires = "sort")]
        desc: bool,

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

        /// permanently delete the file, bypassing the bin
        #[arg(long)]
        permanent: bool,
    },
    /// Download a file's bytes over the REST HTTP API, writing them in the anyr
    /// process to `--file`, into `--dir`, or to `<object_id>` in the current
    /// directory. This is the default download path.
    #[command(
        alias = "down",
        group = ArgGroup::new("download_destination")
            .args(["dir", "file"])
            .multiple(false)
    )]
    Download {
        /// space id or name
        space: String,

        /// id of file object to download
        object_id: String,

        /// output directory (optional)
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,

        /// output file path (optional)
        #[arg(short = 'f', long, value_name = "FILE")]
        file: Option<PathBuf>,

        /// pre-rendered image variant width in pixels
        #[arg(long, value_name = "PIXELS")]
        width: Option<u32>,

        /// HTTP byte range, e.g. `bytes=0-499`
        #[arg(long, value_name = "HTTP_RANGE")]
        range: Option<String>,

        /// `If-Match` precondition entity tag
        #[arg(long, value_name = "ETAG")]
        if_match: Option<String>,

        /// `If-None-Match` cache validator entity tag
        #[arg(long, value_name = "ETAG")]
        if_none_match: Option<String>,

        /// `If-Modified-Since` HTTP-date
        #[arg(long, value_name = "HTTP_DATE")]
        if_modified_since: Option<String>,

        /// `If-Unmodified-Since` HTTP-date precondition
        #[arg(long, value_name = "HTTP_DATE")]
        if_unmodified_since: Option<String>,

        /// `If-Range` validator for a ranged request
        #[arg(long, value_name = "VALUE")]
        if_range: Option<String>,
    },
    /// Fetch file HTTP metadata with a REST `HEAD` request (no body).
    #[command(alias = "meta")]
    Metadata {
        /// space id or name
        space: String,

        /// id of file object to inspect
        object_id: String,

        /// pre-rendered image variant width in pixels
        #[arg(long, value_name = "PIXELS")]
        width: Option<u32>,

        /// `If-Match` precondition entity tag
        #[arg(long, value_name = "ETAG")]
        if_match: Option<String>,

        /// `If-None-Match` cache validator entity tag
        #[arg(long, value_name = "ETAG")]
        if_none_match: Option<String>,

        /// `If-Modified-Since` HTTP-date
        #[arg(long, value_name = "HTTP_DATE")]
        if_modified_since: Option<String>,

        /// `If-Unmodified-Since` HTTP-date precondition
        #[arg(long, value_name = "HTTP_DATE")]
        if_unmodified_since: Option<String>,
    },
    #[command(
        alias = "up",
        group = ArgGroup::new("upload_source")
            .args(["file", "url", "stdin"])
            .required(true)
            .multiple(false)
    )]
    Upload {
        /// space id or name
        space: String,

        /// input file path (REST unless a gRPC-only option is set)
        #[arg(short = 'f', long, value_name = "FILE")]
        file: Option<PathBuf>,

        /// remote URL to fetch and upload (selects the gRPC backend)
        #[arg(long, value_name = "URL")]
        url: Option<String>,

        /// read the file bytes from stdin (requires --name)
        #[arg(long, requires = "name")]
        stdin: bool,

        /// file name to record for a --stdin upload (only used with --stdin)
        #[arg(long, value_name = "NAME")]
        name: Option<String>,

        /// MIME type used by a REST (path/stdin) upload
        #[arg(long, value_name = "MIME")]
        mime: Option<String>,

        /// file type hint (selects the gRPC backend)
        #[arg(long, value_enum)]
        file_type: Option<FileTypeArg>,

        /// file style: auto, link, or embed (selects the gRPC backend)
        #[arg(long, value_enum)]
        style: Option<FileStyleArg>,

        /// extra object details as JSON or `@FILE` (selects the gRPC backend)
        #[arg(long, value_name = "JSON_OR_@FILE")]
        details: Option<String>,

        /// object id the file is created in context of (selects the gRPC backend)
        #[arg(long, value_name = "OBJECT_ID")]
        created_in_context: Option<String>,

        /// block id the file is created in context of (selects the gRPC backend)
        #[arg(long, value_name = "BLOCK_ID")]
        created_in_context_ref: Option<String>,

        /// (deprecated) no-op: a plain upload already uses REST; errors if combined with a gRPC-only option
        #[arg(long)]
        http: bool,
    },
    /// Preload a file for a later object (gRPC), returning a preload id.
    #[command(
        group = ArgGroup::new("preload_source")
            .args(["file", "url"])
            .required(true)
            .multiple(false)
    )]
    Preload {
        /// space id or name
        space: String,

        /// input file path
        #[arg(short = 'f', long, value_name = "FILE")]
        file: Option<PathBuf>,

        /// remote URL to fetch and preload
        #[arg(long, value_name = "URL")]
        url: Option<String>,

        /// file type hint
        #[arg(long, value_enum)]
        file_type: Option<FileTypeArg>,

        /// object id the file is created in context of
        #[arg(long, value_name = "OBJECT_ID")]
        created_in_context: Option<String>,

        /// block id the file is created in context of
        #[arg(long, value_name = "BLOCK_ID")]
        created_in_context_ref: Option<String>,
    },
    /// Discard a previously preloaded file (gRPC).
    DiscardPreload {
        /// space id or name
        space: String,

        /// preload file id to discard
        file_id: String,
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
    Link {
        /// space id or name
        space: String,

        /// id of object to link
        object_id: String,

        /// invite cid (must be used with --key)
        #[arg(long)]
        cid: Option<String>,

        /// invite key (must be used with --cid)
        #[arg(long)]
        key: Option<String>,
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

        /// set object's icon from file (path must be utf8 string)
        #[arg(long)]
        icon_file: Option<String>,

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
        icon_file: Option<String>,

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
    #[command(
        group = ArgGroup::new("type_property_mode")
            .args(["add_properties", "set_properties", "clear_properties"])
            .multiple(false)
    )]
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

        /// add property to the exact non-featured list (requires HTTP and gRPC)
        #[arg(long = "add-property", value_name = "PROP_NAME_OR_ID")]
        add_properties: Vec<String>,

        /// replace the complete non-featured property list (KEY:FORMAT:NAME)
        #[arg(long = "set-property", value_name = "KEY:FORMAT:NAME")]
        set_properties: Vec<String>,

        /// remove all non-featured recommended properties
        #[arg(long = "clear-properties")]
        clear_properties: bool,
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

#[derive(Clone, ValueEnum, Debug)]
pub enum FileStyleArg {
    /// let the server choose how the file is embedded
    Auto,
    /// reference the file as a link block
    Link,
    /// embed the file inline
    Embed,
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

        /// property key (recommended), `snake_case`
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
    Admin,
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

        /// tag key (recommended), `snake_case`
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
        limit: u32,
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

    /// Limit search to types (`type_key`). Repeat to include multiple types
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

#[derive(Args, Debug)]
pub struct ChatArgs {
    /// transport policy for chat operations: auto (per-operation policy),
    /// rest (reject gRPC-only operations/options), or grpc. rest's rejection
    /// guard is enforced today; per-operation REST routing is staged for
    /// follow-up work, so auto/grpc do not yet change the executed backend.
    #[arg(long, value_enum, default_value = "auto")]
    pub transport: TransportArg,

    #[command(subcommand)]
    pub command: Box<ChatCommands>,
}

#[derive(Subcommand, Debug)]
pub enum ChatCommands {
    /// List chats
    List {
        /// space id or name (optional)
        #[arg(long)]
        space: Option<String>,

        /// search text (name/title)
        #[arg(long)]
        text: Option<String>,

        /// property filter(s); only for a space-scoped REST listing (no --text)
        #[command(flatten)]
        filter: FilterArgs,

        #[command(flatten)]
        pagination: PaginationArgs,
    },

    /// Create a chat object in a space
    Create {
        /// space id or name
        space: String,

        /// chat name
        name: String,

        /// icon emoji (mutually exclusive with --icon-file)
        #[arg(long, group = "chat_icon")]
        icon_emoji: Option<String>,

        /// icon file path (mutually exclusive with --icon-emoji)
        #[arg(long, group = "chat_icon")]
        icon_file: Option<String>,
    },

    /// Get chat object
    Get {
        /// space id or name
        space: String,

        /// chat id or name/title
        chat: String,
    },

    /// Message operations
    #[command(alias = "msg", alias = "m")]
    Messages(ChatMessagesArgs),

    /// Mark messages as read
    Read {
        /// space id or name
        space: String,

        /// chat id or name/title
        chat: String,

        /// read type (messages or mentions)
        #[arg(long, value_enum)]
        read_type: Option<ChatReadTypeArg>,

        /// mark read after order id
        #[arg(long)]
        after: Option<String>,

        /// mark read before order id
        #[arg(long)]
        before: Option<String>,

        /// last chat state id
        #[arg(long)]
        last_state_id: Option<String>,
    },

    /// Mark reactions as read (REST)
    ReadReactions {
        /// space id or name
        space: String,

        /// chat id or name/title
        chat: String,

        /// mark reactions read through this order id
        #[arg(long)]
        order_id: Option<String>,
    },

    /// Mark every message in a chat as read (REST)
    ReadAll {
        /// space id or name
        space: String,

        /// chat id or name/title
        chat: String,
    },

    /// Mark messages as unread
    Unread {
        /// space id or name
        space: String,

        /// chat id or name/title
        chat: String,

        /// unread type (messages or mentions)
        #[arg(long, value_enum)]
        read_type: Option<ChatReadTypeArg>,

        /// mark unread after order id
        #[arg(long)]
        after: Option<String>,
    },

    /// Listen for new chat messages
    Listen {
        /// chat id or name/title (repeatable)
        #[arg(long = "chat")]
        chats: Vec<String>,

        /// space id or name (required when chat is name/title unless chat is a space name/id)
        #[arg(long)]
        space: Option<String>,

        /// preload last N messages per chat before streaming
        #[arg(long)]
        include_history: Option<usize>,

        /// start watermark for preload/listing
        #[arg(long)]
        after: Option<String>,

        /// include stream lifecycle events in output
        #[arg(long)]
        show_events: bool,

        /// (REST SSE) replay the last N messages when the stream opens
        #[arg(long)]
        initial_limit: Option<u32>,

        /// (REST SSE) heartbeat interval in seconds (1-60)
        #[arg(long)]
        heartbeat: Option<u32>,

        /// (gRPC) subscribe to cross-chat message previews
        #[arg(long)]
        previews: bool,

        /// (gRPC) event buffer capacity
        #[arg(long)]
        buffer: Option<usize>,
    },
}

#[derive(Args, Debug)]
pub struct ChatMessagesArgs {
    #[command(subcommand)]
    pub command: ChatMessagesCommands,
}

#[derive(Subcommand, Debug)]
pub enum ChatMessagesCommands {
    /// List messages for a chat
    List {
        /// space id or name
        space: String,

        /// chat id or name/title
        chat: String,

        /// show messages after order id
        #[arg(long)]
        after: Option<String>,

        /// show messages before order id
        #[arg(long)]
        before: Option<String>,

        /// include boundary order id
        #[arg(long)]
        include_boundary: bool,

        /// limit messages (default 100)
        #[arg(long, default_value = "100")]
        limit: usize,

        /// list unread-only messages or mentions
        #[arg(long, value_enum)]
        unread_only: Option<ChatReadTypeArg>,
    },

    /// Get messages by id
    Get {
        /// space id or name
        space: String,

        /// chat id or name/title
        chat: String,

        /// message ids or order ids
        #[arg(required = true)]
        message_ids: Vec<String>,
    },

    /// Send a message
    Send {
        /// space id or name
        space: String,

        /// chat id or name/title
        chat: String,

        /// message text (overrides positional TEXT)
        #[arg(long)]
        text: Option<String>,

        /// message style
        #[arg(long, value_enum, default_value = "paragraph")]
        style: Option<MessageStyleArg>,

        /// message marks (format `type[:from:to[:param]]`)
        #[arg(long = "mark", value_name = "SPEC")]
        mark: Vec<String>,

        /// attachments (format `type:target_id`)
        #[arg(long = "attachment", value_name = "SPEC")]
        attachment: Vec<String>,

        /// raw JSON `MessageContent` (@file, @-, or -)
        #[arg(long)]
        content_json: Option<String>,

        /// plain text message (@file, @-, or -)
        #[arg(long)]
        content_text: Option<String>,

        /// reply to an existing message (id or order id)
        #[arg(long)]
        reply_to: Option<String>,

        /// structured message blocks as a JSON array (@file, @-, or -); requires gRPC
        #[arg(long)]
        blocks_json: Option<String>,

        /// message text if --text is not provided
        #[arg(value_name = "TEXT", trailing_var_arg = true)]
        text_args: Vec<String>,
    },

    /// Edit a message
    Edit {
        /// space id or name
        space: String,

        /// chat id or name/title
        chat: String,

        /// message id or order id
        message_id: String,

        /// message text
        #[arg(long)]
        text: Option<String>,

        /// message style
        #[arg(long, value_enum, default_value = "paragraph")]
        style: Option<MessageStyleArg>,

        /// message marks (format `type[:from:to[:param]]`)
        #[arg(long = "mark", value_name = "SPEC")]
        mark: Vec<String>,

        /// replacement attachments (format `type:target_id`); complete replacement list
        #[arg(long = "attachment", value_name = "SPEC")]
        attachment: Vec<String>,

        /// raw JSON `MessageContent` (@file, @-, or -)
        #[arg(long)]
        content_json: Option<String>,

        /// structured message blocks as a JSON array (@file, @-, or -); requires gRPC
        #[arg(long)]
        blocks_json: Option<String>,
    },

    /// Delete a message
    Delete {
        /// space id or name
        space: String,

        /// chat id or name/title
        chat: String,

        /// message id or order id
        message_id: String,
    },

    /// Search messages in a chat (REST-only)
    Search {
        /// space id or name
        space: String,

        /// chat id or name/title
        chat: String,

        /// full-text search query
        query: String,

        #[command(flatten)]
        pagination: PaginationArgs,
    },

    /// Toggle a reaction on a message
    React {
        /// space id or name
        space: String,

        /// chat id or name/title
        chat: String,

        /// message id or order id
        message_id: String,

        /// reaction emoji
        emoji: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ListCommands {
    Objects {
        /// space id or name (required)
        space: String,

        /// list or collection id, or type id/name/key
        list_id: String,

        /// view name or id (required)
        #[arg(long)]
        view: String,

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
    pub limit: u32,

    /// return results starting with offset (for continuation of previous search)
    #[arg(long, default_value = "0")]
    pub offset: u32,

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

pub async fn run(mut cli: Cli) -> Result<()> {
    apply_init_cli_endpoint_defaults(&mut cli);

    #[cfg(feature = "backup")]
    if let Commands::Backup(ref command) = cli.command {
        validate_backup_output_flags(&cli, command)?;
    }

    let output = Output::new(resolve_output_format(&cli), cli.output.clone());

    // Handle commands that don't need a client or keystore
    if let Commands::Auth(AuthArgs {
        command: AuthCommands::FindGrpc { ref program },
    }) = cli.command
    {
        return auth::find_grpc_cmd(&output, program).await;
    }

    let date_format = resolve_table_date_format(&cli);

    let client = build_client(&cli)?;

    let ctx = AppContext {
        //base_url: client.get_http_endpoint().to_string(),
        client,
        output,
        date_format,
    };

    match cli.command {
        Commands::InitCli { join } => init_cli::handle(&ctx, join.as_deref()).await,
        Commands::Auth(args) => auth::handle(&ctx, args).await,
        Commands::Chat(args) => chat::handle(&ctx, args).await,
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
        Commands::Md(args) => any_edit::run(args, ctx.client).await,
        #[cfg(feature = "backup")]
        Commands::Backup(args) => {
            let output = backup_output(&ctx.output);
            anyback_reader::cli::run_command(args, ctx.client, output).await
        }
        #[cfg(feature = "mcp")]
        Commands::Mcp(_) => unreachable!("MCP is dispatched before the standard runtime"),
    }
}

/// Dispatch the embedded MCP process before any standard Tokio runtime starts.
#[cfg(feature = "mcp")]
pub fn run_mcp(args: &McpArgs, keystore: Option<String>) -> std::process::ExitCode {
    let mut arguments = args.arguments.clone();
    if arguments.len() == 1
        && arguments
            .first()
            .is_some_and(|value| value == "-V" || value == "--version")
    {
        eprintln!("use `anyr --version` for the anyr binary version");
        return std::process::ExitCode::FAILURE;
    }
    let alias = arguments
        .first()
        .and_then(|value| value.to_str())
        .filter(|command| matches!(*command, "init" | "check"))
        .map(ToOwned::to_owned);
    if let Some(command) = alias {
        arguments.remove(0);
        arguments.insert(0, OsString::from(command));
        arguments.insert(0, OsString::from("config"));
    }
    any_mcp::run_process_with_keystore_override(arguments, keystore)
}

#[cfg(all(test, feature = "mcp"))]
mod mcp_tests {
    use clap::Parser;

    use super::{Cli, Commands};

    #[test]
    fn mcp_accepts_keystore_options_before_and_after_the_subcommand() {
        for arguments in [
            [
                "anyr",
                "--keystore=file:path=/tmp/keys.db",
                "mcp",
                "--config",
                "/tmp/config.toml",
            ],
            [
                "anyr",
                "mcp",
                "--keystore=file:path=/tmp/keys.db",
                "--config",
                "/tmp/config.toml",
            ],
        ] {
            let cli = Cli::try_parse_from(arguments).expect("MCP command parses");
            assert_eq!(cli.keystore.as_deref(), Some("file:path=/tmp/keys.db"));
            let Commands::Mcp(mcp) = cli.command else {
                panic!("expected MCP command");
            };
            assert_eq!(mcp.arguments, ["--config", "/tmp/config.toml"]);
        }
    }
}

fn apply_init_cli_endpoint_defaults(cli: &mut Cli) {
    if matches!(cli.command, Commands::InitCli { .. }) {
        cli.url.get_or_insert_with(|| HEADLESS_HTTP_URL.to_owned());
        cli.grpc
            .get_or_insert_with(|| HEADLESS_GRPC_ENDPOINT.to_owned());
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

/// Maps the resolved Anyr output format onto the backup command output contract.
///
/// Anyr's table presentation maps to the backup commands' human text summaries;
/// the backup reports are documents rather than uniform rows.
#[cfg(feature = "backup")]
fn backup_output(output: &Output) -> anyback_reader::cli::CommandOutput {
    use anyback_reader::cli::{CommandOutput, OutputMode};

    let mode = match output.format() {
        OutputFormat::Json => OutputMode::Json,
        OutputFormat::Pretty => OutputMode::Pretty,
        OutputFormat::Table => OutputMode::Human,
        OutputFormat::Quiet => OutputMode::Quiet,
    };
    CommandOutput::new(mode, output.path().map(Path::to_path_buf))
}

/// Rejects global output flag combinations that a backup command cannot honor.
///
/// Anyr warns and picks a winner for conflicting format flags on most
/// commands. Backup commands produce archives and import reports whose
/// presentation is load-bearing for scripts, so an ambiguous or impossible
/// request is an error instead of a silently downgraded run.
#[cfg(feature = "backup")]
fn validate_backup_output_flags(cli: &Cli, command: &anyback_reader::cli::Commands) -> Result<()> {
    use anyback_reader::cli::{command_is_interactive, command_name};

    let name = command_name(command);

    let mut requested: Vec<&str> = Vec::new();
    if cli.json {
        requested.push("--json");
    }
    if cli.pretty {
        requested.push("--pretty");
    }
    if cli.table {
        requested.push("--table");
    }
    if cli.quiet {
        requested.push("--quiet");
    }
    if requested.len() > 1 {
        bail!(
            "conflicting output formats for `backup {name}`: {} - choose one",
            requested.join(", ")
        );
    }

    if cli.quiet && cli.output.is_some() {
        bail!(
            "conflicting output options for `backup {name}`: --quiet suppresses the output that --output would write"
        );
    }

    if command_is_interactive(command) {
        if let Some(flag) = requested.first() {
            bail!("`backup {name}` renders an interactive terminal UI and does not support {flag}");
        }
        if cli.output.is_some() {
            bail!(
                "`backup {name}` renders an interactive terminal UI and does not support --output"
            );
        }
    }

    let output = Output::new(resolve_output_format(cli), cli.output.clone());
    anyback_reader::cli::validate_command_output(command, &backup_output(&output))
        .with_context(|| format!("invalid output path for `backup {name}`"))
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

impl TypeLayoutArg {
    pub fn to_layout(&self) -> TypeLayout {
        match self {
            Self::Basic => TypeLayout::Basic,
            Self::Profile => TypeLayout::Profile,
            Self::Action => TypeLayout::Action,
            Self::Note => TypeLayout::Note,
        }
    }
}

impl PropertyFormatArg {
    pub fn to_format(&self) -> PropertyFormat {
        match self {
            Self::Text => PropertyFormat::Text,
            Self::Number => PropertyFormat::Number,
            Self::Select => PropertyFormat::Select,
            Self::MultiSelect => PropertyFormat::MultiSelect,
            Self::Date => PropertyFormat::Date,
            Self::Files => PropertyFormat::Files,
            Self::Checkbox => PropertyFormat::Checkbox,
            Self::Url => PropertyFormat::Url,
            Self::Email => PropertyFormat::Email,
            Self::Phone => PropertyFormat::Phone,
            Self::Objects => PropertyFormat::Objects,
        }
    }
}

impl MemberRoleArg {
    pub fn to_role(&self) -> MemberRole {
        match self {
            Self::Viewer => MemberRole::Viewer,
            Self::Editor => MemberRole::Editor,
            Self::Admin => MemberRole::Admin,
            Self::Owner => MemberRole::Owner,
        }
    }
}

impl MemberStatusArg {
    pub fn to_status(&self) -> MemberStatus {
        match self {
            Self::Joining => MemberStatus::Joining,
            Self::Active => MemberStatus::Active,
            Self::Removed => MemberStatus::Removed,
            Self::Declined => MemberStatus::Declined,
            Self::Removing => MemberStatus::Removing,
            Self::Canceled => MemberStatus::Canceled,
        }
    }
}

impl TagColorArg {
    pub fn to_color(&self) -> Color {
        match self {
            Self::Grey => Color::Grey,
            Self::Yellow => Color::Yellow,
            Self::Orange => Color::Orange,
            Self::Red => Color::Red,
            Self::Pink => Color::Pink,
            Self::Purple => Color::Purple,
            Self::Blue => Color::Blue,
            Self::Ice => Color::Ice,
            Self::Teal => Color::Teal,
            Self::Lime => Color::Lime,
        }
    }
}

pub fn pagination_limit(pagination: &PaginationArgs) -> u32 {
    if pagination.all {
        1000
    } else {
        pagination.limit
    }
}

pub fn pagination_offset(pagination: &PaginationArgs) -> u32 {
    pagination.offset
}

pub fn must_have_body(
    body: Option<impl Into<String>>,
    body_file: Option<impl AsRef<Path>>,
) -> Result<Option<String>> {
    if body.is_some() && body_file.is_some() {
        bail!("--body and --body-file are mutually exclusive");
    }
    if let Some(body) = body {
        return Ok(Some(body.into()));
    }
    if let Some(path) = body_file {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .map_err(|err| anyhow::anyhow!("read {}: {err}", path.display()))?;
        return Ok(Some(content));
    }
    Ok(None)
}

pub fn resolve_icon_exists(
    emoji: Option<impl Into<String>>,
    file: Option<String>,
) -> Result<Option<Icon>> {
    if emoji.is_some() && file.is_some() {
        bail!("--icon-emoji and --icon-file are mutually exclusive");
    }
    if let Some(emoji) = emoji {
        return Ok(Some(Icon::Emoji {
            emoji: emoji.into(),
        }));
    }
    if let Some(path) = file {
        if !PathBuf::from(&path).is_file() {
            bail!("icon file does not exist:{path}");
        }
        return Ok(Some(Icon::File { file: path }));
    }
    Ok(None)
}

#[cfg(all(test, feature = "backup"))]
mod backup_output_tests {
    use super::*;
    use anyback_reader::cli::OutputMode;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("arguments should parse")
    }

    fn validate(args: &[&str]) -> Result<()> {
        let cli = parse(args);
        let Commands::Backup(ref command) = cli.command else {
            panic!("expected a backup command");
        };
        validate_backup_output_flags(&cli, command)
    }

    fn mode_for(args: &[&str]) -> OutputMode {
        let cli = parse(args);
        backup_output(&Output::new(
            resolve_output_format(&cli),
            cli.output.clone(),
        ))
        .mode()
    }

    #[test]
    fn default_backup_output_is_compact_json() {
        assert_eq!(
            mode_for(&["anyr", "backup", "list", "archive-dir"]),
            OutputMode::Json
        );
    }

    #[test]
    fn global_format_flags_reach_the_backup_dispatcher() {
        assert_eq!(
            mode_for(&["anyr", "--pretty", "backup", "list", "archive-dir"]),
            OutputMode::Pretty
        );
        assert_eq!(
            mode_for(&["anyr", "--table", "backup", "list", "archive-dir"]),
            OutputMode::Human
        );
        assert_eq!(
            mode_for(&["anyr", "--quiet", "backup", "list", "archive-dir"]),
            OutputMode::Quiet
        );
    }

    #[test]
    fn output_file_reaches_the_backup_dispatcher() {
        let cli = parse(&[
            "anyr",
            "backup",
            "list",
            "archive-dir",
            "--output",
            "report.json",
        ]);
        let output = backup_output(&Output::new(
            resolve_output_format(&cli),
            cli.output.clone(),
        ));
        assert_eq!(output.path(), Some(Path::new("report.json")));
    }

    #[test]
    fn output_file_aliases_are_rejected_before_dispatch() {
        for args in [
            vec![
                "anyr",
                "backup",
                "list",
                "archive.zip",
                "--output",
                "./archive.zip",
            ],
            vec![
                "anyr",
                "backup",
                "create",
                "--space",
                "space",
                "--dest",
                "archive.zip",
                "--output",
                "archive.zip",
            ],
            vec![
                "anyr",
                "backup",
                "extract",
                "archive.zip",
                "object-id",
                "object.md",
                "--output",
                "object.md",
            ],
            vec![
                "anyr",
                "backup",
                "restore",
                "archive.zip",
                "--space",
                "space",
                "--log",
                "report.json",
                "--output",
                "report.json",
            ],
        ] {
            let err = validate(&args).expect_err("aliased output must fail");
            let message = format!("{err:#}");
            assert!(message.contains("aliases"), "{message}");
        }
    }

    #[test]
    fn conflicting_format_flags_are_rejected() {
        let err = validate(&["anyr", "--json", "--table", "backup", "list", "archive-dir"])
            .expect_err("conflicting formats must fail");
        let message = err.to_string();
        assert!(message.contains("conflicting output formats"), "{message}");
        assert!(message.contains("--json"), "{message}");
        assert!(message.contains("--table"), "{message}");
    }

    #[test]
    fn quiet_with_output_file_is_rejected() {
        let err = validate(&[
            "anyr",
            "--quiet",
            "backup",
            "list",
            "archive-dir",
            "--output",
            "report.json",
        ])
        .expect_err("quiet plus output file must fail");
        assert!(
            err.to_string().contains("--quiet suppresses the output"),
            "{err}"
        );
    }

    #[test]
    fn single_format_flags_are_accepted() {
        for flag in ["--json", "--pretty", "--table", "--quiet"] {
            validate(&["anyr", flag, "backup", "list", "archive-dir"])
                .unwrap_or_else(|err| panic!("{flag} should be accepted: {err}"));
        }
        validate(&[
            "anyr",
            "--pretty",
            "backup",
            "list",
            "archive-dir",
            "--output",
            "report.json",
        ])
        .expect("pretty with an output file should be accepted");
    }

    #[cfg(feature = "inspect")]
    #[test]
    fn interactive_inspect_rejects_output_redirection() {
        let err = validate(&[
            "anyr",
            "backup",
            "inspect",
            "archive-dir",
            "--output",
            "report.json",
        ])
        .expect_err("inspect must reject --output");
        assert!(err.to_string().contains("interactive terminal UI"), "{err}");

        let err = validate(&["anyr", "--quiet", "backup", "inspect", "archive-dir"])
            .expect_err("inspect must reject --quiet");
        assert!(err.to_string().contains("interactive terminal UI"), "{err}");

        validate(&["anyr", "backup", "inspect", "archive-dir"])
            .expect("plain inspect should be accepted");
    }
}
