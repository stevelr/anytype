use anyhow::Result;

use crate::{
    cli::{AppContext, pagination_limit, pagination_offset},
    filter::parse_filters,
    output::OutputFormat,
};

pub async fn handle(ctx: &AppContext, args: super::ListArgs) -> Result<()> {
    match args.command {
        super::ListCommands::Objects {
            space,
            list_id,
            view,
            pagination,
            filter,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let mut request = ctx
                .client
                .view_list_objects(space_id, list_id)
                .view(view)
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
        super::ListCommands::Views {
            space,
            list_id,
            pagination,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
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
            space,
            list_id,
            object_ids,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let result = ctx
                .client
                .view_add_objects(space_id, list_id, object_ids)
                .await?;
            ctx.output
                .emit_json(&serde_json::json!({ "result": result }))
        }
        super::ListCommands::Remove {
            space,
            list_id,
            object_id,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let result = ctx
                .client
                .view_remove_object(space_id, list_id, object_id)
                .await?;
            ctx.output
                .emit_json(&serde_json::json!({ "result": result }))
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::{Cli, Commands, ListCommands};

    fn list_command(args: &[&str]) -> Result<ListCommands, clap::Error> {
        let cli = Cli::try_parse_from(args)?;
        match cli.command {
            Commands::List(list) => Ok(list.command),
            other => panic!("expected list command, got {other:?}"),
        }
    }

    #[test]
    fn list_objects_requires_view() {
        // Missing --view is now rejected at parse time.
        let err = list_command(&["anyr", "list", "objects", "space", "list"])
            .expect_err("missing --view should fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn list_objects_accepts_view() {
        let command = list_command(&["anyr", "list", "objects", "space", "list", "--view", "grid"])
            .expect("view provided should parse");
        match command {
            ListCommands::Objects {
                space,
                list_id,
                view,
                ..
            } => {
                assert_eq!(space, "space");
                assert_eq!(list_id, "list");
                assert_eq!(view, "grid");
            }
            other => panic!("expected objects command, got {other:?}"),
        }
    }
}
