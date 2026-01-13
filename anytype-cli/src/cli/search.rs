use crate::cli::common::{resolve_space_id, resolve_type_ids};
use crate::cli::{AppContext, ensure_authenticated, pagination_limit, pagination_offset};
use crate::filter::parse_filters;
use crate::output::OutputFormat;
use anyhow::Result;
use anytype::prelude::*;

pub async fn handle(ctx: &AppContext, args: super::SearchArgs) -> Result<()> {
    ensure_authenticated(&ctx.client)?;

    let resolved_space_id = match args.space.as_deref() {
        Some(space_id) => Some(resolve_space_id(ctx, space_id).await?),
        None => None,
    };
    let mut request = if let Some(space_id) = resolved_space_id.as_deref() {
        ctx.client.search_in(space_id.to_string())
    } else {
        ctx.client.search_global()
    };

    if let Some(text) = args.text {
        request = request.text(text);
    }

    if !args.types.is_empty() {
        if let Some(space_id) = resolved_space_id.as_deref() {
            let resolved = resolve_type_ids(ctx, space_id, &args.types).await?;
            request = request.types(resolved);
        } else {
            request = request.types(args.types);
        }
    }

    let filters = parse_filters(&args.filter.filters)?;
    if !filters.is_empty() {
        request = request.filters(FilterExpression::from(filters));
    }

    if let Some(sort_key) = args.sort.sort {
        request = if args.sort.desc {
            request.sort_desc(sort_key)
        } else {
            request.sort_asc(sort_key)
        };
    }

    request = request
        .limit(pagination_limit(&args.pagination))
        .offset(pagination_offset(&args.pagination));

    if args.pagination.all {
        let items = request.execute().await?.collect_all().await?;
        if ctx.output.format() == OutputFormat::Table {
            return ctx.output.emit_table(&items);
        }
        return ctx.output.emit_json(&items);
    }

    let result = request.execute().await?;
    if ctx.output.format() == OutputFormat::Table {
        return ctx.output.emit_table(&result.items);
    }
    ctx.output.emit_json(&result)
}
