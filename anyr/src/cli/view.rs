use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use anytype::prelude::Object;
use serde::Serialize;
use serde_json::Value;

use crate::{
    cli::{
        AppContext,
        common::{
            MemberCache, load_member_cache, resolve_member_name, resolve_space_id, resolve_type_id,
            resolve_view_id,
        },
    },
    output::{OutputFormat, render_table_dynamic},
};

#[derive(Debug, Clone)]
struct ViewColumn {
    relation_key: String,
    name: String,
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
    match args.command {
        super::ViewCommands::Objects {
            view,
            columns,
            space,
            type_id,
            limit,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let type_id = resolve_type_id(ctx, &space_id, &type_id).await?;
            let view_id = resolve_view_id(ctx, &space_id, &type_id, &view).await?;
            let base_columns = default_columns();
            let request = ctx
                .client
                .view_list_objects(&space_id, &type_id)
                .view(view_id.clone())
                .limit(limit);
            let result = request.list().await?;
            let property_names = load_property_names(ctx, &space_id).await?;

            if ctx.output.format() == OutputFormat::Table {
                let columns = columns.map_or_else(
                    || base_columns.clone(),
                    |value| override_columns(&property_names, &value),
                );
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
            } else {
                let json_columns = columns_for_items(&result.items, &property_names);
                let items = view_objects_rows(&json_columns, &result.items);
                let output = ViewObjectsOutput {
                    view_id,
                    columns: json_columns
                        .iter()
                        .map(|col| ViewColumnOutput {
                            key: col.relation_key.clone(),
                            name: col.name.clone(),
                        })
                        .collect(),
                    items,
                };
                ctx.output.emit_json(&output)
            }
        }
    }
}

async fn load_property_names(ctx: &AppContext, space_id: &str) -> Result<HashMap<String, String>> {
    let properties = ctx
        .client
        .properties(space_id)
        .list()
        .await?
        .collect_all()
        .await?;
    Ok(properties
        .into_iter()
        .map(|prop| (prop.key, prop.name))
        .collect())
}

fn default_columns() -> Vec<ViewColumn> {
    vec![ViewColumn {
        relation_key: "name".to_string(),
        name: "Name".to_string(),
    }]
}

fn override_columns(property_names: &HashMap<String, String>, columns: &str) -> Vec<ViewColumn> {
    columns
        .split(',')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(|key| match key {
            "id" => ViewColumn {
                relation_key: "id".to_string(),
                name: "Id".to_string(),
            },
            "name" => ViewColumn {
                relation_key: "name".to_string(),
                name: "Name".to_string(),
            },
            _ => ViewColumn {
                relation_key: key.to_string(),
                name: property_names
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| key.to_string()),
            },
        })
        .collect()
}

fn columns_for_items(
    items: &[Object],
    property_names: &HashMap<String, String>,
) -> Vec<ViewColumn> {
    let mut keys = BTreeSet::new();
    for object in items {
        for prop in &object.properties {
            keys.insert(prop.key.clone());
        }
    }

    let mut columns = Vec::with_capacity(keys.len() + 2);
    columns.push(ViewColumn {
        relation_key: "name".to_string(),
        name: "Name".to_string(),
    });
    columns.push(ViewColumn {
        relation_key: "id".to_string(),
        name: "Id".to_string(),
    });
    for key in keys {
        columns.push(ViewColumn {
            relation_key: key.clone(),
            name: property_names.get(&key).cloned().unwrap_or(key),
        });
    }
    columns
}

fn view_objects_rows(columns: &[ViewColumn], items: &[Object]) -> Vec<BTreeMap<String, Value>> {
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
    columns: &[ViewColumn],
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
        anytype::properties::PropertyValue::Date { date } => {
            object.get_property_date(relation_key).map_or_else(
                || date.clone(),
                |value| value.format(date_format).to_string(),
            )
        }
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
