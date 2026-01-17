use crate::cli::common::{resolve_property_id, resolve_space_id};
use crate::cli::{AppContext, ensure_authenticated, pagination_limit, pagination_offset};
use crate::filter::parse_filters;
use crate::output::OutputFormat;
use anyhow::Result;
use anytype::error::AnytypeError;
use anytype::validation::looks_like_object_id;

pub async fn handle(ctx: &AppContext, args: super::TagArgs) -> Result<()> {
    ensure_authenticated(&ctx.client)?;
    match args.command {
        super::TagCommands::List {
            space,
            property_id,
            pagination,
            filter,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let property_id = resolve_property_id(ctx, &space_id, &property_id).await?;
            let mut request = ctx
                .client
                .tags(space_id, property_id)
                .limit(pagination_limit(&pagination))
                .offset(pagination_offset(&pagination));

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
        super::TagCommands::Get {
            space,
            property_id,
            tag_id,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let property_id = resolve_property_id(ctx, &space_id, &property_id).await?;
            let item = if looks_like_object_id(&tag_id) {
                ctx.client.tag(space_id, property_id, tag_id).get().await?
            } else {
                let tags = ctx
                    .client
                    .tags(space_id, &property_id)
                    .list()
                    .await?
                    .collect_all()
                    .await?;
                tags.into_iter()
                    .find(|tag| tag.name == tag_id || tag.key == tag_id || tag.id == tag_id)
                    .ok_or_else(|| AnytypeError::NotFound {
                        obj_type: "Tag".to_string(),
                        key: tag_id,
                    })?
            };

            //let item = ctx.client.tag(space_id, property_id, tag_id).get().await?;
            ctx.output.emit_json(&item)
        }
        super::TagCommands::Create {
            space,
            property_id,
            name,
            color,
            key,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let property_id = resolve_property_id(ctx, &space_id, &property_id).await?;
            let mut request = ctx
                .client
                .new_tag(space_id, property_id)
                .name(name)
                .color(color.to_color());

            if let Some(key) = key {
                request = request.key(key);
            }

            let item = request.create().await?;
            ctx.output.emit_json(&item)
        }
        super::TagCommands::Update {
            space,
            property_id,
            tag_id,
            name,
            key,
            color,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let property_id = resolve_property_id(ctx, &space_id, &property_id).await?;
            let tag_id = ctx
                .client
                .lookup_property_tag(&space_id, &property_id, &tag_id)
                .await?
                .id;

            let mut request = ctx.client.update_tag(space_id, property_id, tag_id);

            if let Some(name) = name {
                request = request.name(name);
            }
            if let Some(key) = key {
                request = request.key(key);
            }
            if let Some(color) = color {
                request = request.color(color.to_color());
            }

            let item = request.update().await?;
            ctx.output.emit_json(&item)
        }
        super::TagCommands::Delete {
            space,
            property_id,
            tag_id,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let item = if looks_like_object_id(&property_id) && looks_like_object_id(&tag_id) {
                ctx.client.tag(space_id, property_id, tag_id)
            } else {
                let property_id = resolve_property_id(ctx, &space_id, property_id).await?;
                let tag = ctx
                    .client
                    .lookup_property_tag(&space_id, &property_id, &tag_id)
                    .await?;
                ctx.client.tag(&space_id, &property_id, &tag.id)
            }
            .delete()
            .await?;
            ctx.output.emit_json(&item)
        }
    }
}
