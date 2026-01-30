use anyhow::Result;

use crate::{
    cli::{
        AppContext,
        common::{resolve_space_id, resolve_type_id},
        pagination_limit, pagination_offset,
    },
    filter::parse_filters,
    output::OutputFormat,
};

pub async fn handle(ctx: &AppContext, args: super::TemplateArgs) -> Result<()> {
    match args.command {
        super::TemplateCommands::List {
            space,
            type_id,
            pagination,
            filter,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
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
            space,
            type_id,
            template_id,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
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
