use crate::cli::{AppContext, ensure_authenticated, pagination_limit, pagination_offset};
use crate::cli::common::resolve_space_id;
use crate::filter::parse_filters;
use crate::output::OutputFormat;
use anyhow::Result;

pub async fn handle(ctx: &AppContext, args: super::ListArgs) -> Result<()> {
    ensure_authenticated(&ctx.client)?;
    match args.command {
        super::ListCommands::Objects {
            space_id,
            list_id,
            view,
            pagination,
            filter,
        } => {
            let space_id = resolve_space_id(ctx, &space_id).await?;
            let mut request = ctx
                .client
                .view_list_objects(space_id, list_id)
                .limit(pagination_limit(&pagination))
                .offset(pagination_offset(&pagination));

            if let Some(view_id) = view {
                request = request.view(view_id);
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
        super::ListCommands::Views {
            space_id,
            list_id,
            pagination,
        } => {
            let space_id = resolve_space_id(ctx, &space_id).await?;
            let request = ctx
                .client
                .list_views(space_id, list_id)
                .limit(pagination_limit(&pagination))
                .offset(pagination_offset(&pagination));

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
        super::ListCommands::Add {
            space_id,
            list_id,
            object_ids,
        } => {
            let space_id = resolve_space_id(ctx, &space_id).await?;
            let result = ctx
                .client
                .view_add_objects(space_id, list_id, object_ids)
                .await?;
            ctx.output
                .emit_json(&serde_json::json!({ "result": result }))
        }
        super::ListCommands::Remove {
            space_id,
            list_id,
            object_id,
        } => {
            let space_id = resolve_space_id(ctx, &space_id).await?;
            let result = ctx
                .client
                .view_remove_object(space_id, list_id, object_id)
                .await?;
            ctx.output
                .emit_json(&serde_json::json!({ "result": result }))
        }
    }
}
