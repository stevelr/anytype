use crate::cli::common::{resolve_space_id, resolve_type_id};
use crate::cli::{
    AppContext, ensure_authenticated, pagination_limit, pagination_offset, resolve_icon,
};
use crate::filter::{parse_filters, parse_type_property};
use crate::output::OutputFormat;
use anyhow::{Result, bail};
use anytype::validation::looks_like_object_id;
use std::collections::HashSet;

const EXCLUDED_TYPE_RELATION_KEYS: [&str; 6] = [
    "type",
    "tag",
    "backlinks",
    "last_modified_date",
    "last_modified_by",
    "last_opened_date",
];

pub async fn handle(ctx: &AppContext, args: super::TypeArgs) -> Result<()> {
    ensure_authenticated(&ctx.client)?;
    match args.command {
        super::TypeCommands::List {
            space,
            pagination,
            filter,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
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
            let space_id = resolve_space_id(ctx, &space).await?;
            let type_id = resolve_type_id(ctx, &space_id, &type_id).await?;
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
            let space_id = resolve_space_id(ctx, &space).await?;
            let mut request = ctx.client.new_type(space_id, name).key(key);
            if let Some(plural) = plural {
                request = request.plural_name(plural);
            }
            if let Some(icon) = resolve_icon(&icon_emoji, &None)? {
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
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let type_id = resolve_type_id(ctx, &space_id, &type_id).await?;
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
            if let Some(icon) = resolve_icon(&icon_emoji, &None)? {
                request = request.icon(icon);
            }
            if let Some(layout) = layout {
                request = request.layout(layout.to_layout());
            }

            if !add_properties.is_empty() {
                let current_type = ctx.client.get_type(&space_id, &type_id).get().await?;
                let mut seen_keys = HashSet::new();
                let mut all_properties = Vec::new();

                for prop in current_type.properties.iter() {
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
                            bail!("property is ambiguous: {}", prop_ref);
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

                for prop in all_properties {
                    request = request.property(prop.name, prop.key, prop.format);
                }
            }

            let item = request.update().await?;
            ctx.output.emit_json(&item)
        }
        super::TypeCommands::Delete { space, type_id } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let type_id = resolve_type_id(ctx, &space_id, &type_id).await?;
            let item = ctx.client.get_type(space_id, type_id).delete().await?;
            ctx.output.emit_json(&item)
        }
    }
}
