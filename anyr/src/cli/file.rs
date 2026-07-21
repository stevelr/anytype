use std::path::PathBuf;

use anyhow::Result;
use anytype::prelude::*;
use serde_json::json;

use crate::{
    cli::{
        AppContext, FileArgs, FileCommands, FileFilterArgs, FileTypeArg, pagination_limit,
        pagination_offset,
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
            let space_id = ctx.client.resolve_space_id(&space).await?;
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
            let space_id = ctx.client.resolve_space_id(&space).await?;
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
            let space_id = ctx.client.resolve_space_id(&space).await?;
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
            let space_id = ctx.client.resolve_space_id(&space).await?;
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
                let typ = ctx.client.resolve_type(&space_id, &typ.key).await?;
                request = ctx
                    .client
                    .set_properties(&space_id, request, &typ, &parsed)
                    .await?;
            }

            let object = request.update().await?;
            ctx.output.emit_json(&object)
        }
        FileCommands::Delete {
            space,
            object_id,
            permanent,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            // Canonical REST delete (`DELETE /v1/spaces/{space}/files/{id}`);
            // `--permanent` maps to `skip_bin=true` (bypass the bin).
            let mut request = ctx.client.files().delete_request(&space_id, &object_id);
            if permanent {
                request = request.permanently();
            }
            request.delete().await?;
            if ctx.output.format() == OutputFormat::Table {
                return ctx.output.emit_text(&format!("deleted {object_id}"));
            }
            ctx.output
                .emit_json(&json!({ "id": object_id, "deleted": true, "permanent": permanent }))
        }
        FileCommands::Download {
            object_id,
            dir,
            file,
            space,
            http,
        } => {
            if http {
                return download_http(ctx, &object_id, space.as_deref(), dir, file).await;
            }
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
            http,
        } => {
            validate_upload_transport(http, file_type.is_some())?;
            if http {
                eprintln!(
                    "warning: --http is deprecated and no longer selects a transport; a plain upload already uses REST"
                );
            }
            let space_id = ctx.client.resolve_space_id(&space).await?;
            // The unified builder picks the least-capable backend that preserves
            // the request: REST for a plain path, gRPC once `--file-type` is set.
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

/// Download a file over the REST HTTP API, writing the bytes to `--file`, into
/// `--dir`, or to `<object_id>` in the current directory.
async fn download_http(
    ctx: &AppContext,
    object_id: &str,
    space: Option<&str>,
    dir: Option<PathBuf>,
    file: Option<PathBuf>,
) -> Result<()> {
    let space =
        space.ok_or_else(|| anyhow::anyhow!("--space is required when downloading with --http"))?;
    let space_id = ctx.client.resolve_space_id(space).await?;
    let bytes = ctx
        .client
        .files()
        .download_bytes(&space_id, object_id)
        .await?;
    let out_path = match (dir, file) {
        (_, Some(path)) => path,
        (Some(path), None) => path.join(object_id),
        (None, None) => PathBuf::from(object_id),
    };
    std::fs::write(&out_path, &bytes)?;
    if ctx.output.format() == OutputFormat::Table {
        return ctx.output.emit_text(&format!("{}", out_path.display()));
    }
    ctx.output
        .emit_json(&json!({ "path": out_path, "bytes": bytes.len() }))
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

/// Reject the deprecated `--http` upload flag when it is combined with a
/// gRPC-only option.
///
/// `--http` is still accepted as a deprecated no-op (a plain upload already
/// uses REST), but it must not silently discard a gRPC-only selector such as
/// `--file-type`; that combination is an error instead.
fn validate_upload_transport(http: bool, has_file_type: bool) -> Result<()> {
    if http && has_file_type {
        anyhow::bail!(
            "--http cannot be combined with --file-type (a gRPC-only option); drop --http to keep --file-type"
        );
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::validate_upload_transport;
    use crate::cli::{Cli, Commands, FileCommands};

    fn file_command(args: &[&str]) -> Result<FileCommands, clap::Error> {
        let cli = Cli::try_parse_from(args)?;
        match cli.command {
            Commands::File(file) => Ok(file.command),
            other => panic!("expected file command, got {other:?}"),
        }
    }

    #[test]
    fn upload_transport_rejects_http_with_file_type() {
        // --http (deprecated) must not silently discard a gRPC-only option.
        assert!(validate_upload_transport(true, true).is_err());
    }

    #[test]
    fn upload_transport_accepts_deprecated_http_alone() {
        assert!(validate_upload_transport(true, false).is_ok());
    }

    #[test]
    fn upload_transport_accepts_file_type_alone() {
        assert!(validate_upload_transport(false, true).is_ok());
    }

    #[test]
    fn upload_still_parses_deprecated_http_with_file_type() {
        // clap accepts the pair; rejection happens in the handler.
        let command = file_command(&[
            "anyr",
            "file",
            "upload",
            "space",
            "--file",
            "/tmp/x.png",
            "--http",
            "--file-type",
            "image",
        ])
        .expect("upload with --http and --file-type parses");
        match command {
            FileCommands::Upload {
                http, file_type, ..
            } => {
                assert!(http);
                assert!(file_type.is_some());
            }
            other => panic!("expected upload command, got {other:?}"),
        }
    }

    #[test]
    fn upload_defaults_have_no_http_or_file_type() {
        let command = file_command(&["anyr", "file", "upload", "space", "--file", "/tmp/x.png"])
            .expect("plain upload parses");
        match command {
            FileCommands::Upload {
                http, file_type, ..
            } => {
                assert!(!http);
                assert!(file_type.is_none());
            }
            other => panic!("expected upload command, got {other:?}"),
        }
    }

    #[test]
    fn delete_defaults_to_non_permanent() {
        let command = file_command(&["anyr", "file", "delete", "space", "file-1"])
            .expect("plain delete parses");
        match command {
            FileCommands::Delete { permanent, .. } => assert!(!permanent),
            other => panic!("expected delete command, got {other:?}"),
        }
    }

    #[test]
    fn delete_accepts_permanent() {
        let command = file_command(&["anyr", "file", "delete", "space", "file-1", "--permanent"])
            .expect("permanent delete parses");
        match command {
            FileCommands::Delete { permanent, .. } => assert!(permanent),
            other => panic!("expected delete command, got {other:?}"),
        }
    }

    #[test]
    fn delete_rejects_removed_http_flag() {
        // --http was removed from the delete surface entirely.
        assert!(file_command(&["anyr", "file", "delete", "space", "file-1", "--http"]).is_err());
    }
}
