use anyhow::{Context, Result};
use anytype::{prelude::*, validation::looks_like_object_id};

use crate::{
    cli::{AppContext, pagination_limit, pagination_offset},
    filter::parse_filters,
    output::OutputFormat,
};

pub async fn handle(ctx: &AppContext, args: super::PropertyArgs) -> Result<()> {
    match args.command {
        super::PropertyCommands::List {
            space,
            pagination,
            filter,
            format,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
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
                let items = ctx
                    .collect_all(async { request.list().await?.collect_all().await })
                    .await?;
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
            let space_id = ctx.client.resolve_space_id(&space).await?;
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
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let mut request = ctx.client.new_property(space_id, name, format.to_format());

            if let Some(key) = key {
                request = request.key(key);
            }

            for tag in tags {
                let (tag_name, color) = tag
                    .split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("invalid tag spec: {tag}"))?;
                let color = Color::try_from(color).context(format!("invalid tag color {color}"))?;
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
        } => handle_update(ctx, space, property, name, key).await,
        super::PropertyCommands::Delete { space, property } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let property_id = ctx.client.resolve_property_id(&space_id, &property).await?;
            let item = ctx.client.property(space_id, &property_id).delete().await?;
            ctx.output.emit_json(&item)
        }
    }
}

/// Handles `property update`, resolving the property and supplying its current
/// name when `--name` is omitted so the key-only form still meets the REST
/// contract.
async fn handle_update(
    ctx: &AppContext,
    space: String,
    property: String,
    name: Option<String>,
    key: Option<String>,
) -> Result<()> {
    // Reject a no-op invocation before any network I/O.
    validate_property_update(name.as_deref(), key.as_deref())?;

    let space_id = ctx.client.resolve_space_id(&space).await?;
    let property_id = ctx.client.resolve_property_id(&space_id, &property).await?;

    // The REST endpoint requires a name on every update. When --name is
    // omitted, fetch the property and reuse its existing name so a key-only
    // update still validates.
    let current_name = if name.is_none() {
        Some(
            ctx.client
                .property(&space_id, &property_id)
                .get()
                .await?
                .name,
        )
    } else {
        None
    };
    let effective_name = choose_property_name(name, current_name);

    let mut request = ctx
        .client
        .update_property(space_id, property_id)
        .name(effective_name);
    if let Some(key) = key {
        request = request.key(key);
    }

    let item = request.update().await?;
    ctx.output.emit_json(&item)
}

/// Rejects a `property update` invocation that supplies neither `--name` nor
/// `--key`, so the no-op is caught before any network I/O.
fn validate_property_update(name: Option<&str>, key: Option<&str>) -> Result<()> {
    if name.is_none() && key.is_none() {
        anyhow::bail!("property update requires at least one of --name or --key");
    }
    Ok(())
}

/// Chooses the name to send with a property update: the explicit `--name` when
/// provided, otherwise the property's current name fetched from the server.
fn choose_property_name(explicit: Option<String>, current: Option<String>) -> String {
    explicit.or(current).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{choose_property_name, validate_property_update};
    use crate::cli::{Cli, Commands, PropertyCommands};

    fn property_command(args: &[&str]) -> Result<PropertyCommands, clap::Error> {
        let cli = Cli::try_parse_from(args)?;
        match cli.command {
            Commands::Property(property) => Ok(property.command),
            other => panic!("expected property command, got {other:?}"),
        }
    }

    #[test]
    fn update_rejects_no_flags() {
        assert!(validate_property_update(None, None).is_err());
    }

    #[test]
    fn update_accepts_name_only() {
        assert!(validate_property_update(Some("New"), None).is_ok());
    }

    #[test]
    fn update_accepts_key_only() {
        assert!(validate_property_update(None, Some("new_key")).is_ok());
    }

    #[test]
    fn key_only_update_supplies_current_name() {
        // --name omitted: fall back to the fetched current name.
        assert_eq!(
            choose_property_name(None, Some("Existing".to_string())),
            "Existing"
        );
    }

    #[test]
    fn explicit_name_overrides_current() {
        // --name provided: the current name is never fetched (None) and ignored.
        assert_eq!(choose_property_name(Some("New".to_string()), None), "New");
    }

    #[test]
    fn update_parses_name_and_key_forms() {
        let command = property_command(&[
            "anyr", "property", "update", "space", "prop", "--key", "new_key",
        ])
        .expect("key-only form parses");
        match command {
            PropertyCommands::Update { name, key, .. } => {
                assert!(name.is_none());
                assert_eq!(key.as_deref(), Some("new_key"));
            }
            other => panic!("expected update command, got {other:?}"),
        }

        let command = property_command(&[
            "anyr", "property", "update", "space", "prop", "--name", "New",
        ])
        .expect("name-only form parses");
        match command {
            PropertyCommands::Update { name, key, .. } => {
                assert_eq!(name.as_deref(), Some("New"));
                assert!(key.is_none());
            }
            other => panic!("expected update command, got {other:?}"),
        }
    }
}
