use crate::cli::{AppContext, ensure_authenticated, pagination_limit, pagination_offset};
use crate::cli::common::resolve_space_id;
use crate::filter::parse_filters;
use crate::output::OutputFormat;
use anyhow::Result;
use anytype::prelude::Filter;

pub async fn handle(ctx: &AppContext, args: super::MemberArgs) -> Result<()> {
    ensure_authenticated(&ctx.client)?;
    match args.command {
        super::MemberCommands::List {
            space_id,
            pagination,
            filter,
            role,
            status,
        } => {
            let space_id = resolve_space_id(ctx, &space_id).await?;
            let mut request = ctx
                .client
                .members(space_id)
                .limit(pagination_limit(&pagination))
                .offset(pagination_offset(&pagination));

            for filter in parse_filters(&filter.filters)? {
                request = request.filter(filter);
            }

            if let Some(role) = role {
                request = request.filter(Filter::text_equal("role", role.to_role().to_string()));
            }

            if let Some(status) = status {
                request = request.filter(Filter::select_in(
                    "status",
                    vec![status.to_status().to_string()],
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
        super::MemberCommands::Get {
            space_id,
            member_id,
        } => {
            let space_id = resolve_space_id(ctx, &space_id).await?;
            let item = ctx.client.member(space_id, member_id).get().await?;
            ctx.output.emit_json(&item)
        }
    }
}
