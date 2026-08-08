use std::collections::HashSet;

use anyhow::{Result, bail};
use anytype::{
    properties::Property,
    types::{CreateTypeProperty, TypePropertyClassification},
    validation::looks_like_object_id,
};

use crate::{
    cli::{AppContext, pagination_limit, pagination_offset, resolve_icon_exists},
    filter::{parse_filters, parse_type_property},
    output::OutputFormat,
};

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
                    let mut additions = Vec::with_capacity(add_properties.len());
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
                        additions.push(prop);
                    }
                    // Classify as late as possible so the read/merge snapshot is
                    // close to the single replacement request.
                    let classification = ctx
                        .client
                        .get_type(&space_id, &type_id)
                        .classify_properties()
                        .await?;

                    request = request
                        .properties(merge_replaceable_properties(&classification, &additions));
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

/// Merges resolved additions into the exact source-classified replaceable list.
///
/// The first property for each key wins, preserving the server's source order
/// followed by the caller's argument order.
fn merge_replaceable_properties(
    classification: &TypePropertyClassification,
    additions: &[Property],
) -> Vec<CreateTypeProperty> {
    let mut seen_keys = HashSet::new();
    let mut merged = Vec::with_capacity(
        classification
            .replaceable()
            .len()
            .saturating_add(additions.len()),
    );

    for property in classification.replaceable().iter().chain(additions) {
        if seen_keys.insert(property.key.clone()) {
            merged.push(CreateTypeProperty {
                name: property.name.clone(),
                key: property.key.clone(),
                format: property.format(),
            });
        }
    }

    merged
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
    use serde_json::json;

    use super::{TypePropertyMode, merge_replaceable_properties, type_property_mode};
    use crate::cli::{Cli, Commands, TypeCommands};

    fn property(id: &str, key: &str, name: &str) -> anytype::properties::Property {
        serde_json::from_value(json!({
            "id": id,
            "key": key,
            "name": name,
            "format": "text"
        }))
        .expect("valid property fixture")
    }

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
    fn merge_uses_source_classification_and_first_key_order() {
        let classification = anytype::types::TypePropertyClassification {
            featured_ids: vec!["featured".to_string()],
            featured: vec![property(
                "featured",
                "ordinary_looking",
                "Featured Property",
            )],
            recommended: vec![
                property("recommended-tag", "tag", "Recommended Tag"),
                property("current", "current", "Current Property"),
            ],
        };
        let additions = vec![
            property("duplicate-tag", "tag", "Duplicate Tag"),
            property("new", "new", "New Property"),
            property("duplicate-new", "new", "Duplicate New"),
        ];

        let merged = merge_replaceable_properties(&classification, &additions);
        let keys_and_names = merged
            .iter()
            .map(|property| (property.key.as_str(), property.name.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(
            keys_and_names,
            vec![
                ("tag", "Recommended Tag"),
                ("current", "Current Property"),
                ("new", "New Property"),
            ]
        );
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

    #[tokio::test]
    #[ignore = "requires a real Anytype server and disposable-space environment"]
    async fn live_add_property_preserves_exact_replaceable_set() {
        use anytype::{
            properties::PropertyFormat,
            test_util::{
                DisposableRun, TestError, retry_definitive_rate_limit, unique_suffix,
                with_disposable_space_context,
            },
        };

        let run = with_disposable_space_context("anyr-type-add-property", |ctx| {
            Box::pin(async move {
                let suffix = unique_suffix();
                let initial_key = format!("anyr_initial_{suffix}");
                let added_key = format!("anyr_added_{suffix}");
                let typ = retry_definitive_rate_limit("anyr type fixture", || async {
                    ctx.client
                        .new_type(&ctx.space_id, "Anyr Property Merge")
                        .key(format!("anyr_property_merge_{suffix}"))
                        .property("Initial Property", &initial_key, PropertyFormat::Text)
                        .create()
                        .await
                })
                .await?;
                ctx.register_type(&typ.id);

                let added = retry_definitive_rate_limit("anyr property fixture", || async {
                    ctx.client
                        .new_property(&ctx.space_id, "Added Property", PropertyFormat::Number)
                        .key(&added_key)
                        .create()
                        .await
                })
                .await?;
                ctx.register_property(&added.id);

                let before = ctx
                    .client
                    .get_type(&ctx.space_id, &typ.id)
                    .classify_properties()
                    .await?;
                let expected_featured_ids = before.featured_ids;

                let app = crate::cli::AppContext {
                    client: ctx.client.clone(),
                    output: crate::output::Output::new(crate::output::OutputFormat::Quiet, None),
                    date_format: "%Y-%m-%d".to_string(),
                };
                let args = crate::cli::TypeArgs {
                    command: crate::cli::TypeCommands::Update {
                        space: ctx.space_id.clone(),
                        type_id: typ.id.clone(),
                        key: None,
                        name: None,
                        plural: None,
                        icon_emoji: None,
                        layout: None,
                        add_properties: vec![added.id.clone(), added.id.clone()],
                        set_properties: Vec::new(),
                        clear_properties: false,
                    },
                };
                super::handle(&app, args)
                    .await
                    .map_err(|_| TestError::Assertion {
                        message: "anyr type add-property handler failed".to_string(),
                    })?;

                let after = ctx
                    .client
                    .get_type(&ctx.space_id, &typ.id)
                    .classify_properties()
                    .await?;
                assert_eq!(after.featured_ids, expected_featured_ids);
                assert_eq!(
                    after
                        .replaceable()
                        .iter()
                        .map(|property| property.key.as_str())
                        .collect::<Vec<_>>(),
                    vec![initial_key.as_str(), added_key.as_str()]
                );
                Ok::<(), TestError>(())
            })
        })
        .await
        .expect("disposable-space property merge run");

        match run {
            DisposableRun::Completed(()) => {}
            DisposableRun::Skipped(reason) => {
                panic!("disposable type property merge skipped: {reason:?}")
            }
        }
    }
}
