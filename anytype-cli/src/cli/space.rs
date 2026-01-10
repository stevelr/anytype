use crate::cli::{AppContext, ensure_authenticated, pagination_limit, pagination_offset};
use crate::filter::parse_filters;
use crate::output::OutputFormat;
use anyhow::Result;

pub async fn handle(ctx: &AppContext, args: super::SpaceArgs) -> Result<()> {
    ensure_authenticated(&ctx.client)?;
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
        super::SpaceCommands::Get { space_id } => {
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
            space_id,
            name,
            description,
        } => {
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
    }
}
