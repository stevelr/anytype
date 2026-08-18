use anyhow::{Result, bail};
use anytype::{prelude::*, validation::looks_like_object_id};

use crate::{
    cli::{
        AppContext, discussion, must_have_body, pagination_limit, pagination_offset,
        resolve_icon_exists,
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
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let mut request = ctx
                .client
                .objects(&space_id)
                .limit(pagination_limit(&pagination))
                .offset(pagination_offset(&pagination));

            if !types.is_empty() {
                let resolved = ctx.client.resolve_type_ids(&space_id, &types).await?;
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
                let items = ctx
                    .collect_all(async { request.list().await?.collect_all().await })
                    .await?;
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
            // `get` addresses an object by id only. Point a caller who passed
            // a name or type at the commands that turn one into an id.
            if !looks_like_object_id(&object_id) {
                bail!(
                    "`object get` takes an object id, not a name: \"{object_id}\"\n  \
                     hint: run `anyr search --space {space} --text {object_id} -t` \
                     or `anyr object list {space} -t` to find the id"
                );
            }
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let object = ctx.client.object(space_id, object_id).get().await?;
            ctx.output.emit_json(&object)
        }
        super::ObjectCommands::Link {
            space,
            object_id,
            cid,
            key,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
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
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let type_key = ctx.client.resolve_type_key(&space_id, type_key).await?;
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
                let typ = ctx
                    .client
                    .resolve_type(&space_id, request.get_type_key())
                    .await?;
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
            let space_id = ctx.client.resolve_space_id(&space).await?;
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
                let type_key = ctx.client.resolve_type_key(&space_id, type_key).await?;
                request = request.type_key(type_key);
            }

            let props = merge_properties(properties, property_args);
            if !props.is_empty() {
                let parsed = parse_properties(&props)?;
                let typ = if let Some(type_key) = request.get_type_key() {
                    ctx.client.resolve_type(&space_id, &type_key).await?
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
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let object = ctx.client.object(space_id, object_id).delete().await?;
            ctx.output.emit_json(&object)
        }
        super::ObjectCommands::Discussion(args) => discussion::handle(ctx, args).await,
    }
}

fn merge_properties(mut properties: Vec<String>, property_args: Vec<String>) -> Vec<String> {
    properties.extend(property_args);
    properties
}

fn parse_properties(props: &[String]) -> Result<Vec<(String, String)>> {
    props.iter().map(|prop| parse_property(prop)).collect()
}
