use anyhow::Result;
use anytype::prelude::*;

use crate::{
    cli::{
        AppContext,
        common::{resolve_space_id, resolve_type, resolve_type_ids, resolve_type_key},
        must_have_body, pagination_limit, pagination_offset, resolve_icon_exists,
    },
    filter::{parse_filters, parse_property},
    output::OutputFormat,
};

#[allow(clippy::too_many_lines)]
pub async fn handle(ctx: &AppContext, args: super::ObjectArgs) -> Result<()> {
    match args.command {
        super::ObjectCommands::List {
            space,
            pagination,
            filter,
            types,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let mut request = ctx
                .client
                .objects(&space_id)
                .limit(pagination_limit(&pagination))
                .offset(pagination_offset(&pagination));

            if !types.is_empty() {
                let resolved = resolve_type_ids(ctx, &space_id, &types).await?;
                request = request.filter(Filter::Objects {
                    condition: Condition::In,
                    property_key: "type".to_string(),
                    objects: resolved,
                });
            }

            for filter in parse_filters(&filter.filters)? {
                request = request.filter(filter);
            }

            if pagination.all {
                let items = request.list().await?.collect_all().await?;
                if ctx.output.format() == OutputFormat::Table {
                    return ctx.output.emit_table(&items);
                }
                return ctx.output.emit_json(&items);
            }

            let result = request.list().await?;
            if ctx.output.format() == OutputFormat::Table {
                return ctx.output.emit_table(&result.items);
            }
            ctx.output.emit_json(&result)
        }
        super::ObjectCommands::Get { space, object_id } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let object = ctx.client.object(space_id, object_id).get().await?;
            ctx.output.emit_json(&object)
        }
        super::ObjectCommands::Link {
            space,
            object_id,
            cid,
            key,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let link = match (cid, key) {
                (Some(cid), Some(key)) => object_link_shared(&space_id, &object_id, &cid, &key),
                (None, None) => object_link(&space_id, &object_id),
                _ => anyhow::bail!("--cid and --key must both be provided, or neither"),
            };
            ctx.output.emit_text(&link)
        }
        super::ObjectCommands::Create {
            space,
            type_key,
            name,
            body,
            body_file,
            icon_emoji,
            icon_file,
            template,
            description,
            url,
            properties,
            property_args,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let type_key = resolve_type_key(ctx, &space_id, type_key).await?;
            let mut request = ctx.client.new_object(&space_id, type_key);

            if let Some(name) = name {
                request = request.name(name);
            }

            if let Some(body) = must_have_body(body, body_file)? {
                request = request.body(body);
            }

            if let Some(icon) = resolve_icon_exists(icon_emoji, icon_file)? {
                request = request.icon(icon);
            }

            if let Some(template) = template {
                request = request.template(template);
            }

            if let Some(description) = description {
                request = request.description(description);
            }

            if let Some(url) = url {
                request = request.url(url);
            }

            let props = merge_properties(properties, property_args);
            if !props.is_empty() {
                let parsed = parse_properties(&props)?;
                let typ = resolve_type(ctx, &space_id, request.get_type_key()).await?;
                request = ctx
                    .client
                    .set_properties(&space_id, request, &typ, &parsed)
                    .await?;
            }

            let object = request.create().await?;
            ctx.output.emit_json(&object)
        }
        super::ObjectCommands::Update {
            space,
            object_id,
            name,
            body,
            body_file,
            icon_emoji,
            icon_file,
            type_key,
            properties,
            property_args,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let mut request = ctx.client.update_object(&space_id, &object_id);

            if let Some(name) = name {
                request = request.name(name);
            }

            if let Some(body) = must_have_body(body, body_file)? {
                request = request.body(body);
            }

            if let Some(icon) = resolve_icon_exists(icon_emoji, icon_file)? {
                request = request.icon(icon);
            }

            if let Some(type_key) = type_key {
                let type_key = resolve_type_key(ctx, &space_id, type_key).await?;
                request = request.type_key(type_key);
            }

            let props = merge_properties(properties, property_args);
            if !props.is_empty() {
                let parsed = parse_properties(&props)?;
                let typ = if let Some(type_key) = request.get_type_key() {
                    resolve_type(ctx, &space_id, &type_key).await?
                } else {
                    let object = ctx.client.object(&space_id, &object_id).get().await?;
                    object.get_type().ok_or_else(|| {
                        anyhow::anyhow!("object has no type; cannot set properties")
                    })?
                };
                request = ctx
                    .client
                    .set_properties(&space_id, request, &typ, &parsed)
                    .await?;
            }

            let object = request.update().await?;
            ctx.output.emit_json(&object)
        }
        super::ObjectCommands::Delete { space, object_id } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let object = ctx.client.object(space_id, object_id).delete().await?;
            ctx.output.emit_json(&object)
        }
    }
}

fn merge_properties(mut properties: Vec<String>, property_args: Vec<String>) -> Vec<String> {
    properties.extend(property_args);
    properties
}

fn parse_properties(props: &[String]) -> Result<Vec<(String, String)>> {
    props.iter().map(|prop| parse_property(prop)).collect()
}
