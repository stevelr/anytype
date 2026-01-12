use crate::cli::common::{resolve_space_id, resolve_type_id};
use crate::cli::{AppContext, ensure_authenticated, pagination_limit, pagination_offset};
use crate::filter::parse_filters;
use crate::output::OutputFormat;
use anyhow::Result;

pub async fn handle(ctx: &AppContext, args: super::TemplateArgs) -> Result<()> {
    ensure_authenticated(&ctx.client)?;
    match args.command {
        super::TemplateCommands::List {
            space_id,
            type_id,
            pagination,
            filter,
        } => {
            let space_id = resolve_space_id(ctx, &space_id).await?;
            let type_id = resolve_type_id(ctx, &space_id, &type_id).await?;
            let mut request = ctx
                .client
                .templates(space_id, type_id)
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
        super::TemplateCommands::Get {
            space_id,
            type_id,
            template_id,
        } => {
            let space_id = resolve_space_id(ctx, &space_id).await?;
            let type_id = resolve_type_id(ctx, &space_id, &type_id).await?;
            let item = ctx
                .client
                .template(space_id, type_id, template_id)
                .get()
                .await?;
            ctx.output.emit_json(&item)
        }
    }
}
