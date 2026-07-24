use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};

use anytype_rpc::anytype::{
    ClientCommandsClient, rpc, rpc::account::local_link::list_apps::Request as ListAppsRequest,
    rpc::block::create::Request as BlockCreateRequest,
    rpc::block::list_delete::Request as BlockListDeleteRequest,
    rpc::block::paste::Request as BlockPasteRequest,
    rpc::object::export::Request as ObjectExportRequest,
    rpc::object::search::Request as SearchRequest, rpc::object::show::Request as ObjectShowRequest,
};
use anytype_rpc::auth::{SessionAuth, create_session_token, with_token};
use anytype_rpc::model::Block;
use anytype_rpc::model::SpaceStatus;
use anytype_rpc::model::block::content::Text as BlockContentText;
use anytype_rpc::model::block::content::dataview::{Filter, filter::Condition};
use anytype_rpc::model::block::{Align, ContentValue, Position, VerticalAlign};
use anytype_rpc::model::export::Format as ExportFormat;

const DEFAULT_GRPC_ADDR: &str = "http://127.0.0.1:31010";

// Relation keys used for filtering/sorting (from anytype-heart bundle)
const RELATION_KEY_RESOLVED_LAYOUT: &str = "resolvedLayout";
const RELATION_KEY_SPACE_LOCAL_STATUS: &str = "spaceLocalStatus";
const RELATION_KEY_SPACE_ACCOUNT_STATUS: &str = "spaceAccountStatus";
const RELATION_KEY_TARGET_SPACE_ID: &str = "targetSpaceId";
const RELATION_KEY_NAME: &str = "name";

const OBJECT_LAYOUT_SPACE_VIEW: i64 = anytype_rpc::model::object_type::Layout::SpaceView as i64;

#[derive(Parser, Debug)]
#[command(name = "anytype-rpc", about = "Anytype gRPC diagnostics")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Auth(AuthArgs),
    Object(ObjectArgs),
    Space(SpaceArgs),
}

#[derive(Args, Debug)]
struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Args, Debug)]
struct SpaceArgs {
    #[command(subcommand)]
    command: SpaceCommand,
}

#[derive(Args, Debug)]
struct ObjectArgs {
    #[command(subcommand)]
    command: ObjectCommand,
}

#[derive(Subcommand, Debug)]
enum AuthCommand {
    Status(AuthStatusArgs),
}

#[derive(Subcommand, Debug)]
enum SpaceCommand {
    List(SpaceListArgs),
    Create(SpaceCreateArgs),
    Delete(SpaceDeleteArgs),
    Invite(InviteArgs),
    EnableSharing(SpaceIdArgs),
    DisableSharing(SpaceIdArgs),
}

#[derive(Args, Debug)]
struct InviteArgs {
    #[command(subcommand)]
    command: InviteCommand,
}

#[derive(Subcommand, Debug)]
enum InviteCommand {
    /// Show the current invite for a space
    Show(InviteShowArgs),
    /// Create a new invite for a space
    Create(InviteCreateArgs),
    /// Revoke the current invite for a space
    Revoke(InviteRevokeArgs),
}

#[derive(Args, Debug)]
struct InviteShowArgs {
    #[command(flatten)]
    shared: SharedArgs,
    /// Space name or ID
    space: String,
}

#[derive(Args, Debug)]
struct InviteCreateArgs {
    #[command(flatten)]
    shared: SharedArgs,
    /// Space name or ID
    space: String,
    /// Grant reader permissions
    #[arg(long, group = "perms")]
    reader: bool,
    /// Grant writer permissions
    #[arg(long, group = "perms")]
    writer: bool,
    /// Grant owner permissions
    #[arg(long, group = "perms")]
    owner: bool,
    /// Create a guest invite
    #[arg(long, group = "approval")]
    guest: bool,
    /// Require approval before invitee can join
    #[arg(long, group = "approval")]
    with_approval: bool,
    /// Allow invitee to join without approval (default)
    #[arg(long, group = "approval")]
    auto_approve: bool,
}

#[derive(Args, Debug)]
struct InviteRevokeArgs {
    #[command(flatten)]
    shared: SharedArgs,
    /// Space name or ID
    space: String,
}

#[derive(Args, Debug)]
struct SpaceIdArgs {
    #[command(flatten)]
    shared: SharedArgs,
    /// Space name or ID
    space: String,
}

#[derive(Subcommand, Debug)]
enum ObjectCommand {
    Export(ObjectExportArgs),
    Update(ObjectUpdateArgs),
}

#[derive(Args, Debug)]
struct SharedArgs {
    /// gRPC server address
    #[arg(long, default_value = DEFAULT_GRPC_ADDR, env="ANYTYPE_GRPC_ENDPOINT")]
    addr: String,
    /// Path to config.json (defaults to ~/.anytype/config.json)
    #[arg(long)]
    config: Option<PathBuf>,
    /// LocalLink app key
    #[arg(long)]
    app_key: Option<String>,
    /// Account key (headless CLI)
    #[arg(long)]
    account_key: Option<String>,
    /// Existing session token
    #[arg(long)]
    token: Option<String>,
}

#[derive(Args, Debug)]
struct AuthStatusArgs {
    #[command(flatten)]
    shared: SharedArgs,
}

#[derive(Args, Debug)]
struct SpaceListArgs {
    #[command(flatten)]
    shared: SharedArgs,
    /// Print debug info and raw record fields
    #[arg(long)]
    debug: bool,
    /// Only filter by layout (skip status filters)
    #[arg(long)]
    layout_only: bool,
    /// Do not apply any filters
    #[arg(long)]
    no_filters: bool,
    /// Override resolvedLayout filter value
    #[arg(long)]
    layout_value: Option<i64>,
}

#[derive(Args, Debug)]
struct SpaceCreateArgs {
    #[command(flatten)]
    shared: SharedArgs,
    /// Name for the new space
    name: String,
    /// Create a chat space
    #[arg(long)]
    chat: bool,
}

#[derive(Args, Debug)]
struct SpaceDeleteArgs {
    #[command(flatten)]
    shared: SharedArgs,
    /// Space name or ID to delete
    name_or_id: String,
}

#[derive(Args, Debug)]
struct ObjectExportArgs {
    #[command(flatten)]
    shared: SharedArgs,
    /// Space ID containing the object
    space_id: String,
    /// Object ID to export
    object_id: String,
    /// Output markdown file
    #[arg(long)]
    output: PathBuf,
    /// Export format for the header
    #[arg(long, value_enum, default_value = "yaml")]
    format: ExportHeaderFormat,
}

#[derive(Args, Debug)]
struct ObjectUpdateArgs {
    #[command(flatten)]
    shared: SharedArgs,
    /// Input markdown file with YAML header
    #[arg(long)]
    input: PathBuf,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum ExportHeaderFormat {
    Yaml,
}

#[derive(Debug, Default, serde::Deserialize)]
struct ConfigFile {
    #[serde(rename = "accountId")]
    account_id: Option<String>,
    #[serde(rename = "techSpaceId")]
    tech_space_id: Option<String>,
    #[serde(rename = "accountKey")]
    account_key: Option<String>,
    #[serde(rename = "sessionToken")]
    session_token: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum AuthSource {
    AppKey,
    AccountKey,
    Token,
    ConfigAccountKey,
    ConfigSessionToken,
}

#[derive(Debug)]
struct ResolvedAuth {
    source: AuthSource,
    value: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Auth(args) => match args.command {
            AuthCommand::Status(args) => auth_status(args).await?,
        },
        Command::Object(args) => match args.command {
            ObjectCommand::Export(args) => object_export(args).await?,
            ObjectCommand::Update(args) => object_update(args).await?,
        },
        Command::Space(args) => match args.command {
            SpaceCommand::List(args) => space_list(args).await?,
            SpaceCommand::Create(args) => space_create(args).await?,
            SpaceCommand::Delete(args) => space_delete(args).await?,
            SpaceCommand::Invite(args) => match args.command {
                InviteCommand::Show(args) => invite_show(args).await?,
                InviteCommand::Create(args) => invite_create(args).await?,
                InviteCommand::Revoke(args) => invite_revoke(args).await?,
            },
            SpaceCommand::EnableSharing(args) => enable_sharing(args).await?,
            SpaceCommand::DisableSharing(args) => disable_sharing(args).await?,
        },
    }
    Ok(())
}

async fn auth_status(args: AuthStatusArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = resolve_config_path(args.shared.config.as_deref())?;
    let config = load_config(&config_path).unwrap_or_default();

    println!("Config path: {}", config_path.display());
    println!(
        "accountId: {}",
        config.account_id.as_deref().unwrap_or("none")
    );
    println!(
        "techSpaceId: {}",
        config.tech_space_id.as_deref().unwrap_or("none")
    );
    println!("accountKey: {}", redact(config.account_key.as_deref()));
    println!("sessionToken: {}", redact(config.session_token.as_deref()));

    let resolved = resolve_auth(&args.shared, &config)?;
    println!("auth source: {}", resolved.source);
    println!("auth value: {}", redact(Some(&resolved.value)));

    let channel = connect(&args.shared.addr).await?;

    let (session_token, session_source) = match resolved.source {
        AuthSource::Token | AuthSource::ConfigSessionToken => (resolved.value, resolved.source),
        AuthSource::AppKey | AuthSource::AccountKey | AuthSource::ConfigAccountKey => {
            let auth = match resolved.source {
                AuthSource::AppKey => SessionAuth::AppKey(resolved.value),
                _ => SessionAuth::AccountKey(resolved.value),
            };
            let token = create_session_token(channel.clone(), auth).await?;
            (token, resolved.source)
        }
    };

    println!("session token: {}", redact(Some(&session_token)));
    println!(
        "scope: {}",
        detect_scope(&channel, &session_token, session_source, &config).await?
    );

    Ok(())
}

async fn space_list(args: SpaceListArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = resolve_config_path(args.shared.config.as_deref())?;
    let config = load_config(&config_path).unwrap_or_default();
    let tech_space_id = config
        .tech_space_id
        .as_deref()
        .ok_or("techSpaceId not found in config.json")?;
    let channel = connect(&args.shared.addr).await?;
    let (session_token, source) = get_session_token(&args.shared, &config, &channel).await?;

    if args.debug {
        println!("Config path: {}", config_path.display());
        println!("techSpaceId: {}", tech_space_id);
        println!(
            "scope: {}",
            detect_scope(&channel, &session_token, source, &config).await?
        );
    }

    let spaces = list_spaces(channel, &session_token, tech_space_id, &args).await?;
    println!("Spaces ({} total):", spaces.len());
    for space in spaces {
        println!("  {} - {}", space.id, space.name);
    }

    Ok(())
}

/// Returns true if the string looks like a space ID (bafyrei... format).
fn is_space_id(s: &str) -> bool {
    let Some((prefix, suffix)) = s.split_once('.') else {
        return false;
    };
    prefix.len() == 59 && prefix.starts_with("bafyrei") && !suffix.is_empty() && suffix.len() <= 13
}

async fn space_create(args: SpaceCreateArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config =
        load_config(&resolve_config_path(args.shared.config.as_deref())?).unwrap_or_default();
    let channel = connect(&args.shared.addr).await?;
    let (session_token, _) = get_session_token(&args.shared, &config, &channel).await?;

    let mut fields = BTreeMap::new();
    fields.insert(
        "name".to_string(),
        prost_types::Value {
            kind: Some(prost_types::value::Kind::StringValue(args.name.clone())),
        },
    );

    let request = rpc::workspace::create::Request {
        details: Some(prost_types::Struct { fields }),
        use_case: rpc::object::import_use_case::request::UseCase::None as i32,
        with_chat: args.chat,
    };
    let mut client = ClientCommandsClient::new(channel);
    let response = client
        .workspace_create(with_token(Request::new(request), &session_token)?)
        .await?
        .into_inner();

    if let Some(error) = &response.error
        && error.code != 0
    {
        return Err(format!(
            "WorkspaceCreate failed: {} (code {})",
            error.description, error.code
        )
        .into());
    }

    println!("Created space: id={} name={}", response.space_id, args.name);
    Ok(())
}

/// Resolve a space name or ID to a space ID.
/// If the input looks like a space ID, returns it as-is.
/// Otherwise, looks up the name via ObjectSearch and returns the matching ID.
async fn resolve_space_id(
    name_or_id: &str,
    shared: &SharedArgs,
    session_token: &str,
    tech_space_id: &str,
    channel: Channel,
) -> Result<String, Box<dyn std::error::Error>> {
    if is_space_id(name_or_id) {
        return Ok(name_or_id.to_string());
    }
    let list_args = SpaceListArgs {
        shared: SharedArgs {
            addr: shared.addr.clone(),
            config: shared.config.clone(),
            app_key: shared.app_key.clone(),
            account_key: shared.account_key.clone(),
            token: shared.token.clone(),
        },
        debug: false,
        layout_only: false,
        no_filters: false,
        layout_value: None,
    };
    let spaces = list_spaces(channel, session_token, tech_space_id, &list_args).await?;
    let matching: Vec<_> = spaces.iter().filter(|s| s.name == name_or_id).collect();
    match matching.len() {
        0 => Err(format!("No space found with name '{name_or_id}'").into()),
        1 => Ok(matching[0].id.clone()),
        n => {
            eprintln!("Multiple spaces ({n}) match name '{name_or_id}':");
            for s in &matching {
                eprintln!("  {}", s.id);
            }
            Err(format!(
                "Ambiguous name '{name_or_id}': {n} spaces match. Use the space ID instead."
            )
            .into())
        }
    }
}

/// Common setup: load config, connect, get session token, resolve space name/ID.
async fn setup_with_space(
    shared: &SharedArgs,
    name_or_id: &str,
) -> Result<(Channel, String, String, ConfigFile), Box<dyn std::error::Error>> {
    let config_path = resolve_config_path(shared.config.as_deref())?;
    let config = load_config(&config_path).unwrap_or_default();
    let tech_space_id = config
        .tech_space_id
        .as_deref()
        .ok_or("techSpaceId not found in config.json")?
        .to_string();
    let channel = connect(&shared.addr).await?;
    let (session_token, _) = get_session_token(shared, &config, &channel).await?;
    let space_id = resolve_space_id(
        name_or_id,
        shared,
        &session_token,
        &tech_space_id,
        channel.clone(),
    )
    .await?;
    Ok((channel, session_token, space_id, config))
}

async fn space_delete(args: SpaceDeleteArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (channel, session_token, space_id, _) =
        setup_with_space(&args.shared, &args.name_or_id).await?;

    let mut client = ClientCommandsClient::new(channel);
    let response = client
        .space_delete(with_token(
            Request::new(rpc::space::delete::Request {
                space_id: space_id.clone(),
            }),
            &session_token,
        )?)
        .await?
        .into_inner();

    if let Some(error) = &response.error
        && error.code != 0
    {
        return Err(format!(
            "SpaceDelete failed: {} (code {})",
            error.description, error.code
        )
        .into());
    }

    println!("Deleted space: {space_id}");
    Ok(())
}

fn invite_type_name(value: i32) -> &'static str {
    match value {
        0 => "member",
        1 => "guest",
        2 => "auto-approve",
        _ => "unknown",
    }
}

fn permissions_name(value: i32) -> &'static str {
    match value {
        0 => "reader",
        1 => "writer",
        2 => "owner",
        3 => "none",
        _ => "unknown",
    }
}

fn invite_url(cid: &str, key: &str) -> String {
    format!("https://invite.any.coop/{cid}#{key}")
}

async fn invite_show(args: InviteShowArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (channel, session_token, space_id, _) = setup_with_space(&args.shared, &args.space).await?;

    let mut client = ClientCommandsClient::new(channel);

    // Get the current member invite
    let member_response = client
        .space_invite_get_current(with_token(
            Request::new(rpc::space::invite_get_current::Request {
                space_id: space_id.clone(),
            }),
            &session_token,
        )?)
        .await?
        .into_inner();

    // Get the guest invite (may not exist for all space types)
    let guest_response = client
        .space_invite_get_guest(with_token(
            Request::new(rpc::space::invite_get_guest::Request {
                space_id: space_id.clone(),
            }),
            &session_token,
        )?)
        .await?
        .into_inner();

    let mut invites = Vec::<serde_json::Value>::new();

    let member_has_error = member_response.error.as_ref().is_some_and(|e| e.code != 0);
    if !member_has_error && !member_response.invite_cid.is_empty() {
        invites.push(serde_json::json!({
            "type": invite_type_name(member_response.invite_type),
            "permissions": permissions_name(member_response.permissions),
            "cid": member_response.invite_cid,
            "key": member_response.invite_file_key,
            "url": invite_url(&member_response.invite_cid, &member_response.invite_file_key),
        }));
    }

    let guest_has_error = guest_response.error.as_ref().is_some_and(|e| e.code != 0);
    if !guest_has_error && !guest_response.invite_cid.is_empty() {
        invites.push(serde_json::json!({
            "type": "guest",
            "cid": guest_response.invite_cid,
            "key": guest_response.invite_file_key,
            "url": invite_url(&guest_response.invite_cid, &guest_response.invite_file_key),
        }));
    }

    if invites.is_empty() {
        println!("No active invites for space {space_id}");
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "space_id": space_id,
                "invites": invites,
            }))?
        );
    }

    Ok(())
}

async fn invite_create(args: InviteCreateArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (channel, session_token, space_id, _) = setup_with_space(&args.shared, &args.space).await?;

    use anytype_rpc::model::{InviteType, ParticipantPermissions};

    let permissions = if args.owner {
        ParticipantPermissions::Owner as i32
    } else if args.writer {
        ParticipantPermissions::Writer as i32
    } else {
        ParticipantPermissions::Reader as i32
    };

    let invite_type = if args.guest {
        InviteType::Guest as i32
    } else if args.with_approval {
        InviteType::Member as i32
    } else {
        // default: auto-approve
        InviteType::WithoutApprove as i32
    };

    let mut client = ClientCommandsClient::new(channel);
    let response = client
        .space_invite_generate(with_token(
            Request::new(rpc::space::invite_generate::Request {
                space_id: space_id.clone(),
                invite_type,
                permissions,
            }),
            &session_token,
        )?)
        .await?
        .into_inner();

    if let Some(error) = &response.error
        && error.code != 0
    {
        return Err(format!(
            "InviteGenerate failed: {} (code {})",
            error.description, error.code
        )
        .into());
    }

    let output = serde_json::json!({
        "cid": response.invite_cid,
        "key": response.invite_file_key,
        "url": invite_url(&response.invite_cid, &response.invite_file_key),
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

async fn invite_revoke(args: InviteRevokeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (channel, session_token, space_id, _) = setup_with_space(&args.shared, &args.space).await?;

    let mut client = ClientCommandsClient::new(channel);
    let response = client
        .space_invite_revoke(with_token(
            Request::new(rpc::space::invite_revoke::Request {
                space_id: space_id.clone(),
            }),
            &session_token,
        )?)
        .await?
        .into_inner();

    if let Some(error) = &response.error
        && error.code != 0
    {
        return Err(format!(
            "InviteRevoke failed: {} (code {})",
            error.description, error.code
        )
        .into());
    }

    println!("Revoked invite for space {space_id}");
    Ok(())
}

async fn enable_sharing(args: SpaceIdArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (channel, session_token, space_id, _) = setup_with_space(&args.shared, &args.space).await?;

    let mut client = ClientCommandsClient::new(channel);
    let response = client
        .space_make_shareable(with_token(
            Request::new(rpc::space::make_shareable::Request {
                space_id: space_id.clone(),
            }),
            &session_token,
        )?)
        .await?
        .into_inner();

    if let Some(error) = &response.error
        && error.code != 0
    {
        return Err(format!(
            "MakeShareable failed: {} (code {})",
            error.description, error.code
        )
        .into());
    }

    println!("Sharing enabled for space {space_id}");
    Ok(())
}

async fn disable_sharing(args: SpaceIdArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (channel, session_token, space_id, _) = setup_with_space(&args.shared, &args.space).await?;

    let mut client = ClientCommandsClient::new(channel);
    let response = client
        .space_stop_sharing(with_token(
            Request::new(rpc::space::stop_sharing::Request {
                space_id: space_id.clone(),
            }),
            &session_token,
        )?)
        .await?
        .into_inner();

    if let Some(error) = &response.error
        && error.code != 0
    {
        return Err(format!(
            "StopSharing failed: {} (code {})",
            error.description, error.code
        )
        .into());
    }

    println!("Sharing disabled for space {space_id}");
    Ok(())
}

async fn object_export(args: ObjectExportArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config =
        load_config(&resolve_config_path(args.shared.config.as_deref())?).unwrap_or_default();
    let channel = connect(&args.shared.addr).await?;
    let (session_token, _) = get_session_token(&args.shared, &config, &channel).await?;

    let mut client = ClientCommandsClient::new(channel);
    let request = ObjectExportRequest {
        space_id: args.space_id.clone(),
        object_id: args.object_id.clone(),
        format: ExportFormat::Markdown as i32,
    };
    let request = with_token(Request::new(request), &session_token)?;

    let response = client.object_export(request).await?.into_inner();
    if let Some(error) = response.error
        && error.code != 0
    {
        return Err(format!(
            "ObjectExport failed: {} (code: {})",
            error.description, error.code
        )
        .into());
    }

    let output = match args.format {
        ExportHeaderFormat::Yaml => {
            build_yaml_export(&args.space_id, &args.object_id, &response.result)
        }
    };
    fs::write(&args.output, output)?;
    println!("Exported to {}", args.output.display());
    Ok(())
}

async fn object_update(args: ObjectUpdateArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config =
        load_config(&resolve_config_path(args.shared.config.as_deref())?).unwrap_or_default();
    let contents = fs::read_to_string(&args.input)?;
    let (header, body) = parse_yaml_front_matter(&contents)?;

    let space_id = header
        .get("spaceId")
        .and_then(|v| v.as_str())
        .or_else(|| header.get("space_id").and_then(|v| v.as_str()))
        .ok_or("missing spaceId in YAML header")?
        .to_string();
    let object_id = header
        .get("objectId")
        .and_then(|v| v.as_str())
        .or_else(|| header.get("object_id").and_then(|v| v.as_str()))
        .ok_or("missing objectId in YAML header")?
        .to_string();

    let channel = connect(&args.shared.addr).await?;
    let (session_token, _) = get_session_token(&args.shared, &config, &channel).await?;

    replace_object_markdown(channel, &session_token, &space_id, &object_id, &body).await?;
    println!("Updated object {}", object_id);
    Ok(())
}

fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    let home = env::var("HOME").map_err(|_| "HOME environment variable not set")?;
    Ok(PathBuf::from(home).join(".anytype").join("config.json"))
}

fn load_config(path: &Path) -> Result<ConfigFile, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let config = serde_json::from_str(&content)?;
    Ok(config)
}

async fn get_session_token(
    shared: &SharedArgs,
    config: &ConfigFile,
    channel: &Channel,
) -> Result<(String, AuthSource), Box<dyn std::error::Error>> {
    let resolved = resolve_auth(shared, config)?;
    let token = match resolved.source {
        AuthSource::Token | AuthSource::ConfigSessionToken => resolved.value,
        AuthSource::AppKey | AuthSource::AccountKey | AuthSource::ConfigAccountKey => {
            let auth = match resolved.source {
                AuthSource::AppKey => SessionAuth::AppKey(resolved.value),
                _ => SessionAuth::AccountKey(resolved.value),
            };
            create_session_token(channel.clone(), auth).await?
        }
    };
    Ok((token, resolved.source))
}

fn resolve_auth(shared: &SharedArgs, config: &ConfigFile) -> Result<ResolvedAuth, String> {
    let mut auth_count = 0;
    if shared.app_key.is_some() {
        auth_count += 1;
    }
    if shared.account_key.is_some() {
        auth_count += 1;
    }
    if shared.token.is_some() {
        auth_count += 1;
    }
    if auth_count > 1 {
        return Err("Specify only one of --app-key, --account-key, or --token".into());
    }

    if let Some(value) = shared.app_key.as_ref() {
        return Ok(ResolvedAuth {
            source: AuthSource::AppKey,
            value: value.clone(),
        });
    }
    if let Some(value) = shared.account_key.as_ref() {
        return Ok(ResolvedAuth {
            source: AuthSource::AccountKey,
            value: value.clone(),
        });
    }
    if let Some(value) = shared.token.as_ref() {
        return Ok(ResolvedAuth {
            source: AuthSource::Token,
            value: value.clone(),
        });
    }

    if let Some(value) = config.account_key.as_ref() {
        return Ok(ResolvedAuth {
            source: AuthSource::ConfigAccountKey,
            value: value.clone(),
        });
    }
    if let Some(value) = config.session_token.as_ref() {
        return Ok(ResolvedAuth {
            source: AuthSource::ConfigSessionToken,
            value: value.clone(),
        });
    }

    Err("No auth found. Provide --app-key, --account-key, or --token, or ensure config.json has credentials.".into())
}

async fn connect(addr: &str) -> Result<Channel, Box<dyn std::error::Error>> {
    let channel = Endpoint::from_shared(addr.to_string())?
        .connect()
        .await
        .map_err(|e| format!("Failed to connect to {}: {}", addr, e))?;
    Ok(channel)
}

fn redact(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "none".to_string();
    };
    if value.is_empty() {
        return "none".to_string();
    }
    let prefix: String = value.chars().take(4).collect();
    format!("{}...", prefix)
}

#[derive(Debug)]
struct SpaceInfo {
    id: String,
    name: String,
}

async fn list_spaces(
    channel: Channel,
    token: &str,
    tech_space_id: &str,
    args: &SpaceListArgs,
) -> Result<Vec<SpaceInfo>, Box<dyn std::error::Error>> {
    let mut client = ClientCommandsClient::new(channel);

    let filters = if args.no_filters {
        Vec::new()
    } else if args.layout_only {
        vec![layout_filter(args.layout_value)]
    } else {
        vec![
            layout_filter(args.layout_value),
            Filter {
                relation_key: RELATION_KEY_SPACE_LOCAL_STATUS.to_string(),
                condition: Condition::In as i32,
                value: Some(int_list(&[
                    SpaceStatus::Unknown as i64,
                    SpaceStatus::Ok as i64,
                ])),
                ..Default::default()
            },
            Filter {
                relation_key: RELATION_KEY_SPACE_ACCOUNT_STATUS.to_string(),
                condition: Condition::In as i32,
                value: Some(int_list(&[
                    SpaceStatus::Unknown as i64,
                    SpaceStatus::SpaceActive as i64,
                ])),
                ..Default::default()
            },
        ]
    };

    let request = SearchRequest {
        space_id: tech_space_id.to_string(),
        filters,
        keys: vec![
            RELATION_KEY_TARGET_SPACE_ID.to_string(),
            RELATION_KEY_NAME.to_string(),
            RELATION_KEY_SPACE_LOCAL_STATUS.to_string(),
            RELATION_KEY_SPACE_ACCOUNT_STATUS.to_string(),
            RELATION_KEY_RESOLVED_LAYOUT.to_string(),
        ],
        ..Default::default()
    };

    let request = Request::new(request);
    let request = with_token(request, token)?;

    let response = client.object_search(request).await?;
    let response = response.into_inner();

    if let Some(error) = response.error
        && error.code != 0
    {
        return Err(format!(
            "ObjectSearch failed: {} (code: {})",
            error.description, error.code
        )
        .into());
    }

    if args.debug {
        println!("Raw records: {}", response.records.len());
    }

    let mut spaces = Vec::new();
    for record in response.records {
        let fields = record.fields;

        let id = fields
            .get(RELATION_KEY_TARGET_SPACE_ID)
            .and_then(|v| match &v.kind {
                Some(prost_types::value::Kind::StringValue(s)) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let name = fields
            .get(RELATION_KEY_NAME)
            .and_then(|v| match &v.kind {
                Some(prost_types::value::Kind::StringValue(s)) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "Unnamed".to_string());

        if args.debug {
            let local_status = get_i64(&fields, RELATION_KEY_SPACE_LOCAL_STATUS);
            let account_status = get_i64(&fields, RELATION_KEY_SPACE_ACCOUNT_STATUS);
            let layout = get_i64(&fields, RELATION_KEY_RESOLVED_LAYOUT);
            println!(
                "record id={} name={} local_status={:?} account_status={:?} layout={:?}",
                id, name, local_status, account_status, layout
            );
        }

        if !id.is_empty() {
            spaces.push(SpaceInfo { id, name });
        }
    }

    Ok(spaces)
}

fn int_list(values: &[i64]) -> prost_types::Value {
    prost_types::Value {
        kind: Some(prost_types::value::Kind::ListValue(
            prost_types::ListValue {
                values: values
                    .iter()
                    .map(|value| prost_types::Value {
                        kind: Some(prost_types::value::Kind::NumberValue(*value as f64)),
                    })
                    .collect(),
            },
        )),
    }
}

fn layout_filter(override_value: Option<i64>) -> Filter {
    let value = override_value.unwrap_or(OBJECT_LAYOUT_SPACE_VIEW);
    Filter {
        relation_key: RELATION_KEY_RESOLVED_LAYOUT.to_string(),
        condition: Condition::Equal as i32,
        value: Some(prost_types::Value {
            kind: Some(prost_types::value::Kind::NumberValue(value as f64)),
        }),
        ..Default::default()
    }
}

fn get_i64(
    fields: &std::collections::BTreeMap<String, prost_types::Value>,
    key: &str,
) -> Option<i64> {
    fields.get(key).and_then(|v| match &v.kind {
        Some(prost_types::value::Kind::NumberValue(n)) => Some(*n as i64),
        _ => None,
    })
}

fn build_yaml_export(space_id: &str, object_id: &str, markdown: &str) -> String {
    let mut output = String::new();
    output.push_str("---\n");
    output.push_str(&format!("spaceId: {}\n", space_id));
    output.push_str(&format!("objectId: {}\n", object_id));
    output.push_str("---\n\n");
    output.push_str(markdown);
    output
}

fn parse_yaml_front_matter(
    contents: &str,
) -> Result<(HashMap<String, serde_yaml_ng::Value>, String), Box<dyn std::error::Error>> {
    let mut lines = contents.lines();
    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        _ => return Err("missing YAML front matter".into()),
    }

    let mut yaml_lines = Vec::new();
    let mut found_end = false;
    for line in &mut lines {
        if line.trim() == "---" {
            found_end = true;
            break;
        }
        yaml_lines.push(line);
    }
    if !found_end {
        return Err("unterminated YAML front matter".into());
    }

    let yaml_str = yaml_lines.join("\n");
    let header: HashMap<String, serde_yaml_ng::Value> = serde_yaml_ng::from_str(&yaml_str)?;
    let body = lines.collect::<Vec<_>>().join("\n");
    Ok((header, body))
}

async fn replace_object_markdown(
    channel: Channel,
    token: &str,
    space_id: &str,
    object_id: &str,
    markdown: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = ClientCommandsClient::new(channel);

    let show_request = ObjectShowRequest {
        object_id: object_id.to_string(),
        space_id: space_id.to_string(),
        ..Default::default()
    };
    let show_request = with_token(Request::new(show_request), token)?;
    let show_response = client.object_show(show_request).await?.into_inner();

    if let Some(error) = show_response.error
        && error.code != 0
    {
        return Err(format!(
            "ObjectShow failed: {} (code: {})",
            error.description, error.code
        )
        .into());
    }

    let object_view = show_response
        .object_view
        .ok_or("ObjectShow returned empty object view")?;
    let root_id = object_view.root_id;
    let root_block = object_view
        .blocks
        .iter()
        .find(|block| block.id == root_id)
        .or_else(|| object_view.blocks.first())
        .ok_or("ObjectShow returned no blocks")?;

    let to_delete: Vec<String> = root_block
        .children_ids
        .iter()
        .filter(|id| id.as_str() != "header")
        .cloned()
        .collect();

    if !to_delete.is_empty() {
        let delete_request = BlockListDeleteRequest {
            context_id: object_id.to_string(),
            block_ids: to_delete,
        };
        let delete_request = with_token(Request::new(delete_request), token)?;
        let delete_response = client.block_list_delete(delete_request).await?.into_inner();
        if let Some(error) = delete_response.error
            && error.code != 0
        {
            return Err(format!(
                "BlockListDelete failed: {} (code: {})",
                error.description, error.code
            )
            .into());
        }
    }

    if markdown.is_empty() {
        return Ok(());
    }

    let block = Block {
        id: "".to_string(),
        align: Align::Left as i32,
        vertical_align: VerticalAlign::Top as i32,
        content_value: Some(ContentValue::Text(BlockContentText {
            text: "".to_string(),
            style: 0,
            ..Default::default()
        })),
        ..Default::default()
    };

    let create_request = BlockCreateRequest {
        context_id: object_id.to_string(),
        target_id: "".to_string(),
        block: Some(block),
        position: Position::Bottom as i32,
    };
    let create_request = with_token(Request::new(create_request), token)?;
    let create_response = client.block_create(create_request).await?.into_inner();

    if let Some(error) = create_response.error
        && error.code != 0
    {
        return Err(format!(
            "BlockCreate failed: {} (code: {})",
            error.description, error.code
        )
        .into());
    }
    let block_id = create_response.block_id;
    if block_id.is_empty() {
        return Err("BlockCreate returned empty block_id".into());
    }

    let paste_request = BlockPasteRequest {
        context_id: object_id.to_string(),
        focused_block_id: block_id,
        text_slot: markdown.to_string(),
        ..Default::default()
    };
    let paste_request = with_token(Request::new(paste_request), token)?;
    let paste_response = client.block_paste(paste_request).await?.into_inner();
    if let Some(error) = paste_response.error
        && error.code != 0
    {
        return Err(format!(
            "BlockPaste failed: {} (code: {})",
            error.description, error.code
        )
        .into());
    }

    Ok(())
}

async fn detect_scope(
    channel: &Channel,
    token: &str,
    source: AuthSource,
    config: &ConfigFile,
) -> Result<String, Box<dyn std::error::Error>> {
    match source {
        AuthSource::AccountKey | AuthSource::ConfigAccountKey => {
            return Ok("Full (account key)".to_string());
        }
        _ => {}
    }

    let list_apps_status = try_list_apps(channel.clone(), token).await;
    if let Ok(()) = list_apps_status {
        return Ok("Full (list apps permitted)".to_string());
    }
    if let Err(status) = list_apps_status
        && status.code() != tonic::Code::PermissionDenied
    {
        return Ok(format!("Unknown (list apps failed: {})", status.code()));
    }

    if let Some(tech_space_id) = config.tech_space_id.as_deref() {
        let search_status = try_object_search(channel.clone(), token, tech_space_id).await;
        match search_status {
            Ok(()) => return Ok("Limited (object search permitted)".to_string()),
            Err(status) => {
                if status.code() == tonic::Code::PermissionDenied {
                    if status.message().contains("JsonAPI") {
                        return Ok("JsonAPI (grpc denied)".to_string());
                    }
                    return Ok("Limited (permission denied on object search)".to_string());
                }
                return Ok(format!("Unknown (object search failed: {})", status.code()));
            }
        }
    }

    Ok("Unknown".to_string())
}

async fn try_list_apps(channel: Channel, token: &str) -> Result<(), Status> {
    let mut client = ClientCommandsClient::new(channel);
    let request = Request::new(ListAppsRequest {});
    let request = with_token(request, token).map_err(to_status)?;
    let response = client.account_local_link_list_apps(request).await?;
    let response = response.into_inner();
    if let Some(error) = response.error
        && error.code != 0
    {
        return Err(Status::permission_denied(error.description));
    }
    Ok(())
}

async fn try_object_search(
    channel: Channel,
    token: &str,
    tech_space_id: &str,
) -> Result<(), Status> {
    let mut client = ClientCommandsClient::new(channel);
    let request = SearchRequest {
        space_id: tech_space_id.to_string(),
        ..Default::default()
    };
    let request = Request::new(request);
    let request = with_token(request, token).map_err(to_status)?;
    let response = client.object_search(request).await?;
    let response = response.into_inner();
    if let Some(error) = response.error
        && error.code != 0
    {
        return Err(Status::permission_denied(error.description));
    }
    Ok(())
}

fn to_status(err: anytype_rpc::error::AuthError) -> Status {
    Status::unknown(err.to_string())
}

impl fmt::Display for AuthSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthSource::AppKey => write!(f, "app key (cli)"),
            AuthSource::AccountKey => write!(f, "account key (cli)"),
            AuthSource::Token => write!(f, "session token (cli)"),
            AuthSource::ConfigAccountKey => write!(f, "account key (config)"),
            AuthSource::ConfigSessionToken => write!(f, "session token (config)"),
        }
    }
}
