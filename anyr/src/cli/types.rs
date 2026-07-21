use std::collections::HashSet;

use anyhow::{Result, bail};
use anytype::validation::looks_like_object_id;

use crate::{
    cli::{AppContext, pagination_limit, pagination_offset, resolve_icon_exists},
    filter::{parse_filters, parse_type_property},
    output::OutputFormat,
};

const EXCLUDED_TYPE_RELATION_KEYS: [&str; 6] = [
    "type",
    "tag",
    "backlinks",
    "last_modified_date",
    "last_modified_by",
    "last_opened_date",
];

#[allow(clippy::too_many_lines)]
pub async fn handle(ctx: &AppContext, args: super::TypeArgs) -> Result<()> {
    match args.command {
        super::TypeCommands::List {
            space,
            pagination,
            filter,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let mut request = ctx
                .client
                .types(space_id)
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
        super::TypeCommands::Get { space, type_id } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let type_id = ctx.client.resolve_type_id(&space_id, &type_id).await?;
            let item = ctx.client.get_type(space_id, type_id).get().await?;
            ctx.output.emit_json(&item)
        }
        super::TypeCommands::Create {
            space,
            key,
            name,
            plural,
            icon_emoji,
            layout,
            properties,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let mut request = ctx.client.new_type(space_id, name).key(key);
            if let Some(plural) = plural {
                request = request.plural_name(plural);
            }
            if let Some(icon) = resolve_icon_exists(icon_emoji, None)? {
                request = request.icon(icon);
            }

            request = request.layout(layout.to_layout());

            for prop in properties {
                let parsed = parse_type_property(&prop)?;
                request = request.property(parsed.name, parsed.key, parsed.format);
            }
            let item = request.create().await?;
            ctx.output.emit_json(&item)
        }
        super::TypeCommands::Update {
            space,
            type_id,
            key,
            name,
            plural,
            icon_emoji,
            layout,
            add_properties,
            set_properties,
            clear_properties,
        } => {
            // Decide the property mutation mode before any network I/O; the clap
            // group already guarantees these three inputs are mutually exclusive.
            let property_mode =
                type_property_mode(add_properties, set_properties, clear_properties)?;

            let space_id = ctx.client.resolve_space_id(&space).await?;
            let type_id = ctx.client.resolve_type_id(&space_id, &type_id).await?;
            let mut request = ctx.client.update_type(&space_id, &type_id);

            if let Some(key) = key {
                request = request.key(key);
            }
            if let Some(name) = name {
                request = request.name(name);
            }
            if let Some(plural) = plural {
                request = request.plural_name(plural);
            }
            if let Some(icon) = resolve_icon_exists(icon_emoji, None)? {
                request = request.icon(icon);
            }
            if let Some(layout) = layout {
                request = request.layout(layout.to_layout());
            }

            match property_mode {
                TypePropertyMode::Unchanged => {}
                TypePropertyMode::Clear => {
                    request = request.clear_properties();
                }
                TypePropertyMode::Replace(properties) => {
                    request = request.properties(properties);
                }
                TypePropertyMode::Merge(add_properties) => {
                    let current_type = ctx.client.get_type(&space_id, &type_id).get().await?;
                    let mut seen_keys = HashSet::new();
                    let mut all_properties = Vec::new();

                    for prop in &current_type.properties {
                        if EXCLUDED_TYPE_RELATION_KEYS.contains(&prop.key.as_str()) {
                            continue;
                        }
                        if seen_keys.insert(prop.key.clone()) {
                            all_properties.push(anytype::types::CreateTypeProperty {
                                name: prop.name.clone(),
                                key: prop.key.clone(),
                                format: prop.format(),
                            });
                        }
                    }

                    for prop_ref in add_properties {
                        let prop = if looks_like_object_id(&prop_ref) {
                            ctx.client.property(&space_id, &prop_ref).get().await?
                        } else {
                            let mut matches =
                                ctx.client.lookup_properties(&space_id, &prop_ref).await?;
                            if matches.len() != 1 {
                                bail!("property is ambiguous: {prop_ref}");
                            }
                            matches.remove(0)
                        };
                        if seen_keys.insert(prop.key.clone()) {
                            all_properties.push(anytype::types::CreateTypeProperty {
                                name: prop.name.clone(),
                                key: prop.key.clone(),
                                format: prop.format(),
                            });
                        }
                    }

                    request = request.properties(all_properties);
                }
            }

            let item = request.update().await?;
            ctx.output.emit_json(&item)
        }
        super::TypeCommands::Delete { space, type_id } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let type_id = ctx.client.resolve_type_id(&space_id, &type_id).await?;
            let item = ctx.client.get_type(space_id, type_id).delete().await?;
            ctx.output.emit_json(&item)
        }
    }
}

/// How `type update` should mutate the type's non-featured property list.
///
/// The variants correspond to the mutually exclusive `--add-property`,
/// `--set-property`, and `--clear-properties` flags; the absence of all three
/// leaves the list unchanged.
enum TypePropertyMode {
    /// No property flag: `properties` is omitted and the list is unchanged.
    Unchanged,
    /// `--clear-properties`: remove all non-featured recommended properties.
    Clear,
    /// `--set-property`: replace the complete non-featured property list.
    Replace(Vec<anytype::types::CreateTypeProperty>),
    /// `--add-property`: read the current list and merge these references in.
    Merge(Vec<String>),
}

/// Resolves the mutually exclusive property flags into a single [`TypePropertyMode`].
///
/// The clap `type_property_mode` group already guarantees at most one input is
/// populated; the precedence here is defensive.
fn type_property_mode(
    add_properties: Vec<String>,
    set_properties: Vec<String>,
    clear_properties: bool,
) -> Result<TypePropertyMode> {
    if clear_properties {
        return Ok(TypePropertyMode::Clear);
    }
    if !set_properties.is_empty() {
        let parsed = set_properties
            .into_iter()
            .map(|spec| parse_type_property(&spec))
            .collect::<Result<Vec<_>>>()?;
        return Ok(TypePropertyMode::Replace(parsed));
    }
    if !add_properties.is_empty() {
        return Ok(TypePropertyMode::Merge(add_properties));
    }
    Ok(TypePropertyMode::Unchanged)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{TypePropertyMode, type_property_mode};
    use crate::cli::{Cli, Commands, TypeCommands};

    fn type_command(args: &[&str]) -> Result<TypeCommands, clap::Error> {
        let cli = Cli::try_parse_from(args)?;
        match cli.command {
            Commands::Type(type_args) => Ok(type_args.command),
            other => panic!("expected type command, got {other:?}"),
        }
    }

    #[test]
    fn mode_unchanged_when_no_flags() {
        let mode = type_property_mode(vec![], vec![], false).expect("valid");
        assert!(matches!(mode, TypePropertyMode::Unchanged));
    }

    #[test]
    fn mode_clear_when_clear_flag() {
        let mode = type_property_mode(vec![], vec![], true).expect("valid");
        assert!(matches!(mode, TypePropertyMode::Clear));
    }

    #[test]
    fn mode_replace_parses_set_properties() {
        let mode = type_property_mode(vec![], vec!["age:number:Age".to_string()], false)
            .expect("valid spec");
        match mode {
            TypePropertyMode::Replace(props) => {
                assert_eq!(props.len(), 1);
                assert_eq!(props[0].key, "age");
                assert_eq!(props[0].name, "Age");
            }
            _ => panic!("expected replace mode"),
        }
    }

    #[test]
    fn mode_replace_rejects_invalid_spec() {
        assert!(type_property_mode(vec![], vec!["not-a-spec".to_string()], false).is_err());
    }

    #[test]
    fn mode_merge_when_add_properties() {
        let mode = type_property_mode(vec!["Status".to_string()], vec![], false).expect("valid");
        match mode {
            TypePropertyMode::Merge(refs) => assert_eq!(refs, vec!["Status".to_string()]),
            _ => panic!("expected merge mode"),
        }
    }

    #[test]
    fn update_add_and_set_conflict() {
        let err = type_command(&[
            "anyr",
            "type",
            "update",
            "space",
            "type",
            "--add-property",
            "Status",
            "--set-property",
            "age:number:Age",
        ])
        .expect_err("add and set are mutually exclusive");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn update_set_and_clear_conflict() {
        let err = type_command(&[
            "anyr",
            "type",
            "update",
            "space",
            "type",
            "--set-property",
            "age:number:Age",
            "--clear-properties",
        ])
        .expect_err("set and clear are mutually exclusive");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn update_each_property_mode_parses_alone() {
        type_command(&[
            "anyr",
            "type",
            "update",
            "space",
            "type",
            "--add-property",
            "Status",
        ])
        .expect("add alone parses");
        type_command(&[
            "anyr",
            "type",
            "update",
            "space",
            "type",
            "--set-property",
            "age:number:Age",
        ])
        .expect("set alone parses");
        type_command(&[
            "anyr",
            "type",
            "update",
            "space",
            "type",
            "--clear-properties",
        ])
        .expect("clear alone parses");
    }
}
