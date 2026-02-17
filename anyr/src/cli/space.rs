use anyhow::Result;

use crate::{
    cli::{AppContext, common::resolve_space_id, pagination_limit, pagination_offset},
    filter::parse_filters,
    output::OutputFormat,
};

pub async fn handle(ctx: &AppContext, args: super::SpaceArgs) -> Result<()> {
    match args.command {
        super::SpaceCommands::List { pagination, filter } => {
            let mut request = ctx
                .client
                .spaces()
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
        super::SpaceCommands::Get { space } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let space = ctx.client.space(space_id).get().await?;
            ctx.output.emit_json(&space)
        }
        super::SpaceCommands::Create { name, description } => {
            let mut request = ctx.client.new_space(name);
            if let Some(description) = description {
                request = request.description(description);
            }
            let space = request.create().await?;
            ctx.output.emit_json(&space)
        }
        super::SpaceCommands::Update {
            space,
            name,
            description,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let mut request = ctx.client.update_space(space_id);
            if let Some(name) = name {
                request = request.name(name);
            }
            if let Some(description) = description {
                request = request.description(description);
            }
            let space = request.update().await?;
            ctx.output.emit_json(&space)
        }
        super::SpaceCommands::CountArchived { space } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let count = ctx.client.count_archived(&space_id).await?;
            ctx.output.emit_text(&format!("{count} archived object(s)"))
        }
        super::SpaceCommands::DeleteArchived { space, confirm } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            if !confirm {
                let count = ctx.client.count_archived(&space_id).await?;
                if count == 0 {
                    return ctx.output.emit_text("no archived objects to delete");
                }
                anyhow::bail!(
                    "{count} archived object(s) in space \"{space}\". \
                     Re-run with --confirm to delete them permanently."
                );
            }
            let result = ctx.client.delete_all_archived(&space_id).await?;
            if result.failed_ids.is_empty() {
                ctx.output
                    .emit_text(&format!("deleted {} archived object(s)", result.deleted))
            } else {
                ctx.output.emit_text(&format!(
                    "deleted {}, failed to delete {}",
                    result.deleted,
                    result.failed_ids.len()
                ))
            }
        }
    }
}
