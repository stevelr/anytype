use crate::cli::common::{resolve_property_id, resolve_space_id};
use crate::cli::{AppContext, ensure_authenticated, pagination_limit, pagination_offset};
use crate::filter::parse_filters;
use crate::output::OutputFormat;
use anyhow::{Context, Result};
use anytype::prelude::*;
use anytype::validation::looks_like_object_id;

pub async fn handle(ctx: &AppContext, args: super::PropertyArgs) -> Result<()> {
    ensure_authenticated(&ctx.client)?;
    match args.command {
        super::PropertyCommands::List {
            space,
            pagination,
            filter,
            format,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let mut request = ctx
                .client
                .properties(space_id)
                .limit(pagination_limit(&pagination))
                .offset(pagination_offset(&pagination));

            for filter in parse_filters(&filter.filters)? {
                request = request.filter(filter);
            }

            if let Some(format) = format {
                request = request.filter(Filter::select_equal(
                    "format",
                    format.to_format().to_string(),
                ));
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
        super::PropertyCommands::Get { space, property } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let item = if looks_like_object_id(&property) {
                ctx.client.property(space_id, property).get().await?
            } else {
                ctx.client
                    .lookup_property_by_key(&space_id, &property)
                    .await?
            };
            ctx.output.emit_json(&item)
        }
        super::PropertyCommands::Create {
            space,
            name,
            format,
            key,
            tags,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let mut request = ctx.client.new_property(space_id, name, format.to_format());

            if let Some(key) = key {
                request = request.key(key);
            }

            for tag in tags {
                let (tag_name, color) = tag
                    .split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("invalid tag spec: {tag}"))?;
                let color = Color::try_from(color).context("invalid tag color {color}")?;
                request = request.tag(tag_name, None, color);
            }

            let item = request.create().await?;
            ctx.output.emit_json(&item)
        }
        super::PropertyCommands::Update {
            space,
            property,
            name,
            key,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let property = resolve_property_id(ctx, &space_id, &property).await?;
            let mut request = ctx.client.update_property(space_id, property);

            if let Some(name) = name {
                request = request.name(name);
            }
            if let Some(key) = key {
                request = request.key(key);
            }

            let item = request.update().await?;
            ctx.output.emit_json(&item)
        }
        super::PropertyCommands::Delete { space, property } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let property_id = resolve_property_id(ctx, &space_id, &property).await?;
            let item = ctx.client.property(space_id, &property_id).delete().await?;
            ctx.output.emit_json(&item)
        }
    }
}
