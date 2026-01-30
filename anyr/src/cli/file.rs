use anyhow::Result;
use anytype::prelude::*;
use serde_json::json;

use crate::{
    cli::{
        AppContext, FileArgs, FileCommands, FileFilterArgs, FileTypeArg,
        common::{resolve_space_id, resolve_type},
        pagination_limit, pagination_offset,
    },
    filter::{parse_filters, parse_property},
    output::OutputFormat,
};

#[allow(clippy::too_many_lines)]
pub async fn handle(ctx: &AppContext, args: FileArgs) -> Result<()> {
    match args.command {
        FileCommands::List {
            space,
            pagination,
            filters,
            filter,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let mut request = ctx
                .client
                .files()
                .list(&space_id)
                .limit(pagination_limit(&pagination))
                .offset(pagination_offset(&pagination));

            request = apply_file_filters_list(request, &filters);
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
        FileCommands::Search {
            space,
            text,
            pagination,
            filters,
            filter,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let mut request = ctx
                .client
                .files()
                .search(&space_id)
                .limit(pagination_limit(&pagination))
                .offset(pagination_offset(&pagination));

            if let Some(text) = text {
                request = request.text(text);
            }

            request = apply_file_filters_search(request, &filters);
            for filter in parse_filters(&filter.filters)? {
                request = request.filter(filter);
            }

            if pagination.all {
                let items = request.search().await?.collect_all().await?;
                if ctx.output.format() == OutputFormat::Table {
                    return ctx.output.emit_table(&items);
                }
                return ctx.output.emit_json(&items);
            }

            let result = request.search().await?;
            if ctx.output.format() == OutputFormat::Table {
                return ctx.output.emit_table(&result.items);
            }
            ctx.output.emit_json(&result)
        }
        FileCommands::Get { space, object_id } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let file = ctx.client.files().get(&space_id, &object_id).get().await?;
            ctx.output.emit_json(&file)
        }
        FileCommands::Update {
            space,
            object_id,
            name,
            properties,
            property_args,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let mut request = ctx.client.update_object(&space_id, &object_id);

            if let Some(name) = name {
                request = request.name(name);
            }

            let props = merge_properties(properties, property_args);
            if !props.is_empty() {
                let parsed = parse_properties(&props)?;
                let object = ctx.client.object(&space_id, &object_id).get().await?;
                let typ = object.get_type().ok_or_else(|| {
                    anyhow::anyhow!("file object has no type; cannot set properties")
                })?;
                let typ = resolve_type(ctx, &space_id, &typ.key).await?;
                request = ctx
                    .client
                    .set_properties(&space_id, request, &typ, &parsed)
                    .await?;
            }

            let object = request.update().await?;
            ctx.output.emit_json(&object)
        }
        FileCommands::Delete { space, object_id } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let object = ctx.client.object(space_id, object_id).delete().await?;
            ctx.output.emit_json(&object)
        }
        FileCommands::Download {
            object_id,
            dir,
            file,
        } => {
            let mut request = ctx.client.files().download(&object_id);
            match (&dir, &file) {
                (Some(path), None) => {
                    request = request.to_dir(path);
                }
                (None, Some(path)) => {
                    request = request.to_file(path);
                }
                (None, None) => {}
                (Some(_), Some(_)) => {
                    anyhow::bail!("--dir and --file are mutually exclusive");
                }
            }
            let download_path = request.download().await?;
            if ctx.output.format() == OutputFormat::Table {
                return ctx
                    .output
                    .emit_text(&format!("{}", download_path.display()));
            }
            ctx.output.emit_json(&json!({
                "path": download_path,
            }))
        }
        FileCommands::Upload {
            space,
            file,
            file_type,
        } => {
            let space_id = resolve_space_id(ctx, &space).await?;
            let mut request = ctx.client.files().upload(&space_id).from_path(&file);
            if let Some(file_type) = file_type {
                request = request.file_type(file_type.into());
            }
            let file = request.upload().await?;
            if ctx.output.format() == OutputFormat::Table {
                return ctx.output.emit_table(&[file]);
            }
            ctx.output.emit_json(&file)
        }
    }
}

fn apply_file_filters_list<'a>(
    mut request: anytype::files::FileListRequest<'a>,
    filters: &FileFilterArgs,
) -> anytype::files::FileListRequest<'a> {
    if let Some(value) = &filters.name_contains {
        request = request.name_contains(value.clone());
    }
    if let Some(value) = filters.file_type.clone() {
        request = request.file_type(&value.into());
    }
    if let Some(value) = &filters.ext {
        request = request.extension(value.clone());
    }
    if !filters.ext_in.is_empty() {
        request = request.extension_in(filters.ext_in.clone());
    }
    if !filters.ext_nin.is_empty() {
        request = request.extension_not_in(filters.ext_nin.clone());
    }
    if let Some(value) = filters.size_eq {
        request = request.size_eq(value);
    }
    if let Some(value) = filters.size_neq {
        request = request.size_neq(value);
    }
    if let Some(value) = filters.size_lt {
        request = request.size_lt(value);
    }
    if let Some(value) = filters.size_lte {
        request = request.size_lte(value);
    }
    if let Some(value) = filters.size_gt {
        request = request.size_gt(value);
    }
    if let Some(value) = filters.size_gte {
        request = request.size_gte(value);
    }
    request
}

fn apply_file_filters_search<'a>(
    mut request: anytype::files::FileSearchRequest<'a>,
    filters: &FileFilterArgs,
) -> anytype::files::FileSearchRequest<'a> {
    if let Some(value) = &filters.name_contains {
        request = request.name_contains(value.clone());
    }
    if let Some(value) = filters.file_type.clone() {
        request = request.file_type(&value.into());
    }
    if let Some(value) = &filters.ext {
        request = request.extension(value.clone());
    }
    if !filters.ext_in.is_empty() {
        request = request.extension_in(filters.ext_in.clone());
    }
    if !filters.ext_nin.is_empty() {
        request = request.extension_not_in(filters.ext_nin.clone());
    }
    if let Some(value) = filters.size_eq {
        request = request.size_eq(value);
    }
    if let Some(value) = filters.size_neq {
        request = request.size_neq(value);
    }
    if let Some(value) = filters.size_lt {
        request = request.size_lt(value);
    }
    if let Some(value) = filters.size_lte {
        request = request.size_lte(value);
    }
    if let Some(value) = filters.size_gt {
        request = request.size_gt(value);
    }
    if let Some(value) = filters.size_gte {
        request = request.size_gte(value);
    }
    request
}

fn merge_properties(mut properties: Vec<String>, property_args: Vec<String>) -> Vec<String> {
    properties.extend(property_args);
    properties
}

fn parse_properties(props: &[String]) -> Result<Vec<(String, String)>> {
    props.iter().map(|prop| parse_property(prop)).collect()
}

impl From<FileTypeArg> for FileType {
    fn from(value: FileTypeArg) -> Self {
        match value {
            FileTypeArg::File => Self::File,
            FileTypeArg::Image => Self::Image,
            FileTypeArg::Video => Self::Video,
            FileTypeArg::Audio => Self::Audio,
            FileTypeArg::Pdf => Self::Pdf,
        }
    }
}
