use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::Value;
use tonic::transport::{Channel, Endpoint};

use crate::{
    cli::{AppContext, ensure_authenticated},
    cli::common::{resolve_space_id, resolve_type_id, resolve_view_id},
    output::{OutputFormat, render_table_dynamic},
};
use anytype::prelude::{Member, Object};
use anytype_rpc::{
    anytype::ClientCommandsClient,
    auth::{SessionAuth, create_session_token},
    model::block::content::dataview::relation::FormulaType,
    views::{GridViewInfo, fetch_grid_view_columns},
};

const DEFAULT_GRPC_ADDR: &str = "http://127.0.0.1:31010";

#[derive(Debug, Default, serde::Deserialize)]
struct AnytypeConfig {
    #[serde(rename = "accountKey")]
    account_key: Option<String>,
    #[serde(rename = "sessionToken")]
    session_token: Option<String>,
}

#[derive(Debug, Serialize)]
struct ViewColumnOutput {
    key: String,
    name: String,
}

#[derive(Debug, Serialize)]
struct ViewObjectsOutput {
    view_id: String,
    columns: Vec<ViewColumnOutput>,
    items: Vec<BTreeMap<String, Value>>,
}

pub async fn handle(ctx: &AppContext, args: super::ViewArgs) -> Result<()> {
    ensure_authenticated(&ctx.client)?;
    match args.command {
        super::ViewCommands::Objects {
            view,
            columns,
            space_id,
            type_id,
            limit,
            grpc_addr,
            grpc_token,
            grpc_config,
        } => {
            let space_id = resolve_space_id(ctx, &space_id).await?;
            let type_id = resolve_type_id(ctx, &space_id, &type_id).await?;
            let view = resolve_view_id(ctx, &space_id, &type_id, &view).await?;
            let app_key = load_app_key_from_keystore(ctx)?;
            let view_info = load_view_info(
                grpc_addr,
                grpc_token,
                grpc_config.as_deref(),
                app_key,
                &space_id,
                &type_id,
                &view,
            )
            .await?;

            let base_columns = view_info.columns.clone();
            let request = ctx
                .client
                .view_list_objects(&space_id, &type_id)
                .view(view)
                .limit(limit);
            let result = request.list().await?;

            match ctx.output.format() {
                OutputFormat::Table => {
                    let columns = match columns {
                        Some(value) => {
                            let property_names =
                                load_type_property_names(ctx, &space_id, &type_id).await?;
                            override_columns(&view_info, &property_names, &value)
                        }
                        None => base_columns.clone(),
                    };
                    let headers = columns
                        .iter()
                        .map(|col| col.name.clone())
                        .collect::<Vec<_>>();
                    let member_cache = load_member_cache(ctx, &space_id).await?;
                    let rows = view_objects_table_rows(
                        &columns,
                        &result.items,
                        &space_id,
                        &member_cache,
                        &ctx.date_format,
                    );
                    let table = render_table_dynamic(&headers, &rows);
                    ctx.output.emit_text(&table)
                }
                _ => {
                    let items = view_objects_rows(&base_columns, &result.items);
                    let output = ViewObjectsOutput {
                        view_id: view_info.view_id,
                        columns: view_info
                            .columns
                            .into_iter()
                            .map(|col| ViewColumnOutput {
                                key: col.relation_key,
                                name: col.name,
                            })
                            .collect(),
                        items,
                    };
                    ctx.output.emit_json(&output)
                }
            }
        }
    }
}

async fn load_view_info(
    grpc_addr: String,
    grpc_token: Option<String>,
    grpc_config: Option<&Path>,
    app_key: Option<String>,
    space_id: &str,
    type_id: &str,
    view_id: &str,
) -> Result<GridViewInfo> {
    let addr = if grpc_addr.is_empty() {
        DEFAULT_GRPC_ADDR.to_string()
    } else {
        grpc_addr
    };

    let channel = connect(&addr).await?;
    let token = resolve_grpc_token(&channel, grpc_token, grpc_config, app_key).await?;
    let mut client = ClientCommandsClient::new(channel);
    let info = fetch_grid_view_columns(&mut client, &token, space_id, type_id, view_id).await?;
    Ok(info)
}

async fn resolve_grpc_token(
    channel: &Channel,
    grpc_token: Option<String>,
    grpc_config: Option<&Path>,
    app_key: Option<String>,
) -> Result<String> {
    if let Some(token) = grpc_token {
        return Ok(token);
    }

    let config = load_anytype_config(grpc_config)?;
    if let Some(token) = config.session_token {
        return Ok(token);
    }
    if let Some(account_key) = config.account_key {
        return create_session_token(channel.clone(), SessionAuth::AccountKey(account_key))
            .await
            .map_err(|err| anyhow!(err.to_string()));
    }
    if let Some(app_key) = app_key {
        return create_session_token(channel.clone(), SessionAuth::AppKey(app_key))
            .await
            .map_err(|err| anyhow!(err.to_string()));
    }

    Err(anyhow!(
        "no grpc auth found: pass --grpc-token or ensure ~/.anytype/config.json has sessionToken/accountKey"
    ))
}

async fn connect(addr: &str) -> Result<Channel> {
    let channel = Endpoint::from_shared(addr.to_string())?
        .connect()
        .await
        .map_err(|err| anyhow!("Failed to connect to {}: {}", addr, err))?;
    Ok(channel)
}

fn load_anytype_config(path: Option<&Path>) -> Result<AnytypeConfig> {
    let path = match path {
        Some(path) => path.to_path_buf(),
        None => default_anytype_config_path()?,
    };
    if !path.exists() {
        return Ok(AnytypeConfig::default());
    }
    let content = fs::read_to_string(&path)?;
    let config = serde_json::from_str(&content)?;
    Ok(config)
}

fn default_anytype_config_path() -> Result<PathBuf> {
    let home = env::var("HOME").map_err(|_| anyhow!("HOME environment variable not set"))?;
    Ok(PathBuf::from(home).join(".anytype").join("config.json"))
}

async fn load_type_property_names(
    ctx: &AppContext,
    space_id: &str,
    type_id: &str,
) -> Result<HashMap<String, String>> {
    let typ = ctx.client.get_type(space_id, type_id).get().await?;
    Ok(typ
        .properties
        .into_iter()
        .map(|prop| (prop.key, prop.name))
        .collect())
}

fn load_app_key_from_keystore(ctx: &AppContext) -> Result<Option<String>> {
    let keystore = ctx.client.get_key_store();
    let key = keystore
        .load_key()
        .map_err(|err| anyhow!(err.to_string()))?;
    Ok(key.map(|secret| secret.get_key().to_string()))
}

// fn visible_columns(view: &GridViewInfo) -> Vec<anytype_rpc::views::GridViewColumn> {
//     let mut columns: Vec<anytype_rpc::views::GridViewColumn> = view
//         .columns
//         .iter()
//         .filter(|col| col.is_visible || col.relation_key == "name")
//         .cloned()
//         .collect();

//     if !columns.iter().any(|col| col.relation_key == "name") {
//         columns.insert(
//             0,
//             anytype_rpc::views::GridViewColumn {
//                 relation_key: "name".to_string(),
//                 name: "Name".to_string(),
//                 format: None,
//                 formula: FormulaType::None,
//                 is_visible: true,
//                 width: 0,
//             },
//         );
//     }

//     columns
// }

fn override_columns(
    view: &GridViewInfo,
    property_names: &HashMap<String, String>,
    columns: &str,
) -> Vec<anytype_rpc::views::GridViewColumn> {
    columns
        .split(',')
        .map(|key| key.trim())
        .filter(|key| !key.is_empty())
        .map(|key| match key {
            "id" => anytype_rpc::views::GridViewColumn {
                relation_key: "id".to_string(),
                name: "Id".to_string(),
                format: None,
                formula: FormulaType::None,
                is_visible: true,
                width: 0,
            },
            "name" => anytype_rpc::views::GridViewColumn {
                relation_key: "name".to_string(),
                name: "Name".to_string(),
                format: None,
                formula: FormulaType::None,
                is_visible: true,
                width: 0,
            },
            _ => resolve_column_for_key(view, property_names, key).unwrap_or_else(|| {
                anytype_rpc::views::GridViewColumn {
                    relation_key: key.to_string(),
                    name: key.to_string(),
                    format: None,
                    formula: FormulaType::None,
                    is_visible: true,
                    width: 0,
                }
            }),
        })
        .collect()
}

fn resolve_column_for_key(
    view: &GridViewInfo,
    property_names: &HashMap<String, String>,
    key: &str,
) -> Option<anytype_rpc::views::GridViewColumn> {
    if let Some(column) = view.columns.iter().find(|col| col.relation_key == key) {
        return Some(with_property_name(column.clone(), property_names));
    }

    if let Some(name) = property_names.get(key) {
        return Some(anytype_rpc::views::GridViewColumn {
            relation_key: key.to_string(),
            name: name.clone(),
            format: None,
            formula: FormulaType::None,
            is_visible: true,
            width: 0,
        });
    }

    None
}

fn with_property_name(
    mut column: anytype_rpc::views::GridViewColumn,
    property_names: &HashMap<String, String>,
) -> anytype_rpc::views::GridViewColumn {
    if column.name == column.relation_key
        && let Some(name) = property_names.get(&column.relation_key)
    {
        column.name = name.clone();
    }
    column
}

fn view_objects_rows(
    columns: &[anytype_rpc::views::GridViewColumn],
    items: &[Object],
) -> Vec<BTreeMap<String, Value>> {
    items
        .iter()
        .map(|object| {
            let mut row = BTreeMap::new();
            for column in columns {
                let value = object_value_for_relation(object, &column.relation_key);
                row.insert(column.relation_key.clone(), value);
            }
            row
        })
        .collect()
}

fn view_objects_table_rows(
    columns: &[anytype_rpc::views::GridViewColumn],
    items: &[Object],
    space_id: &str,
    member_cache: &MemberCache,
    date_format: &str,
) -> Vec<Vec<String>> {
    items
        .iter()
        .map(|object| {
            columns
                .iter()
                .map(|column| {
                    table_cell_for_relation(
                        object,
                        &column.relation_key,
                        space_id,
                        member_cache,
                        date_format,
                    )
                })
                .collect()
        })
        .collect()
}

fn object_value_for_relation(object: &Object, relation_key: &str) -> Value {
    if relation_key == "name"
        && let Some(name) = object.name.as_deref()
    {
        return Value::String(name.to_string());
    }
    if relation_key == "id" {
        return Value::String(object.id.clone());
    }

    let Some(prop) = object.get_property(relation_key) else {
        return Value::Null;
    };

    match &prop.value {
        anytype::properties::PropertyValue::Text { text } => Value::String(text.clone()),
        anytype::properties::PropertyValue::Number { number } => Value::Number(number.clone()),
        anytype::properties::PropertyValue::Select { select } => Value::String(select.key.clone()),
        anytype::properties::PropertyValue::MultiSelect { multi_select } => Value::Array(
            multi_select
                .iter()
                .map(|tag| Value::String(tag.key.clone()))
                .collect(),
        ),
        anytype::properties::PropertyValue::Date { date } => Value::String(date.clone()),
        anytype::properties::PropertyValue::Files { files } => {
            Value::Array(files.iter().cloned().map(Value::String).collect())
        }
        anytype::properties::PropertyValue::Checkbox { checkbox } => Value::Bool(*checkbox),
        anytype::properties::PropertyValue::Url { url } => Value::String(url.clone()),
        anytype::properties::PropertyValue::Email { email } => Value::String(email.clone()),
        anytype::properties::PropertyValue::Phone { phone } => Value::String(phone.clone()),
        anytype::properties::PropertyValue::Objects { objects } => {
            Value::Array(objects.iter().cloned().map(Value::String).collect())
        }
    }
}

fn table_cell_for_relation(
    object: &Object,
    relation_key: &str,
    space_id: &str,
    member_cache: &MemberCache,
    date_format: &str,
) -> String {
    if relation_key == "name" {
        return object.name.clone().unwrap_or_default();
    }
    if relation_key == "id" {
        return object.id.clone();
    }

    let Some(prop) = object.get_property(relation_key) else {
        return String::new();
    };

    match &prop.value {
        anytype::properties::PropertyValue::Text { text } => {
            resolve_member_name(space_id, member_cache, text)
        }
        anytype::properties::PropertyValue::Number { number } => number.to_string(),
        anytype::properties::PropertyValue::Select { select } => {
            resolve_member_name(space_id, member_cache, &select.key)
        }
        anytype::properties::PropertyValue::MultiSelect { multi_select } => multi_select
            .iter()
            .map(|tag| resolve_member_name(space_id, member_cache, &tag.key))
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        anytype::properties::PropertyValue::Date { date } => object
            .get_property_date(relation_key)
            .map(|value| value.format(date_format).to_string())
            .unwrap_or_else(|| date.clone()),
        anytype::properties::PropertyValue::Files { files } => files
            .iter()
            .map(|value| resolve_member_name(space_id, member_cache, value))
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        anytype::properties::PropertyValue::Checkbox { checkbox } => checkbox.to_string(),
        anytype::properties::PropertyValue::Url { url } => {
            resolve_member_name(space_id, member_cache, url)
        }
        anytype::properties::PropertyValue::Email { email } => {
            resolve_member_name(space_id, member_cache, email)
        }
        anytype::properties::PropertyValue::Phone { phone } => {
            resolve_member_name(space_id, member_cache, phone)
        }
        anytype::properties::PropertyValue::Objects { objects } => objects
            .iter()
            .map(|value| resolve_member_name(space_id, member_cache, value))
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
    }
}

struct MemberCache {
    identities: HashMap<String, String>,
}

async fn load_member_cache(ctx: &AppContext, space_id: &str) -> Result<MemberCache> {
    let members = ctx
        .client
        .members(space_id)
        .list()
        .await?
        .collect_all()
        .await?;
    Ok(MemberCache {
        identities: build_member_identity_map(&members),
    })
}

fn build_member_identity_map(members: &[Member]) -> HashMap<String, String> {
    let mut identities = HashMap::new();
    for member in members {
        if let Some(identity) = member.identity.as_deref() {
            identities.insert(identity.to_string(), member.display_name().to_string());
        }
    }
    identities
}

fn resolve_member_name(space_id: &str, member_cache: &MemberCache, value: &str) -> String {
    let Some(identity) = parse_member_identity(space_id, value) else {
        return value.to_string();
    };

    if let Some(name) = member_cache.identities.get(identity) {
        return name.clone();
    }

    identity.chars().take(8).collect()
}

fn parse_member_identity<'a>(space_id: &str, value: &'a str) -> Option<&'a str> {
    let space_fragment = space_id.replace('.', "_");
    let prefix = format!("_participant_{space_fragment}_");
    let identity = value.strip_prefix(&prefix)?;
    if identity.len() == 48 {
        Some(identity)
    } else {
        None
    }
}
