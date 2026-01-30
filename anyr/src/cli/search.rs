use anyhow::Result;
use anytype::prelude::*;

use crate::{
    cli::{
        AppContext,
        common::{resolve_space_id, resolve_type_ids},
        pagination_limit, pagination_offset,
    },
    filter::parse_filters,
    output::OutputFormat,
};

pub async fn handle(ctx: &AppContext, args: super::SearchArgs) -> Result<()> {
    let resolved_space_id = match args.space.as_deref() {
        Some(space_id) => Some(resolve_space_id(ctx, space_id).await?),
        None => None,
    };
    let mut request = resolved_space_id.as_deref().map_or_else(
        || ctx.client.search_global(),
        |space_id| ctx.client.search_in(space_id.to_string()),
    );

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
