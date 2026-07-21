use std::path::PathBuf;

use anyhow::Result;
use anytype::prelude::*;
use serde_json::json;

use crate::{
    cli::{
        AppContext, FileArgs, FileCommands, FileFilterArgs, FileStyleArg, FileTypeArg,
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
            sort,
            desc,
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

            if let Some(sort) = sort {
                request = if desc {
                    request.sort_desc(sort)
                } else {
                    request.sort_asc(sort)
                };
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
            space,
            object_id,
            dir,
            file,
            width,
            range,
            if_match,
            if_none_match,
            if_modified_since,
            if_unmodified_since,
            if_range,
        } => {
            let opts = RestDownloadOptions {
                width,
                range,
                if_match,
                if_none_match,
                if_modified_since,
                if_unmodified_since,
                if_range,
            };
            download_http(ctx, &object_id, &space, dir, file, opts).await
        }
        FileCommands::Metadata {
            space,
            object_id,
            width,
            if_match,
            if_none_match,
            if_modified_since,
            if_unmodified_since,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let mut request = ctx.client.files().download_request(&space_id, &object_id);
            if let Some(width) = width {
                request = request.width(width);
            }
            if let Some(value) = if_match {
                request = request.if_match(value);
            }
            if let Some(value) = if_none_match {
                request = request.if_none_match(value);
            }
            if let Some(value) = if_modified_since {
                request = request.if_modified_since(value);
            }
            if let Some(value) = if_unmodified_since {
                request = request.if_unmodified_since(value);
            }
            let response = request.head().await?;
            let status = response.status.as_u16();
            if ctx.output.format() == OutputFormat::Table {
                return ctx
                    .output
                    .emit_text(&metadata_table(status, &response.metadata));
            }
            ctx.output.emit_json(&json!({
                "status": status,
                "metadata": metadata_json(&response.metadata),
            }))
        }
        FileCommands::Upload {
            space,
            file,
            url,
            stdin,
            name,
            mime,
            file_type,
            style,
            details,
            created_in_context,
            created_in_context_ref,
            http,
        } => {
            // The unified builder auto-selects REST for a plain path/stdin upload
            // and gRPC once any richer option is present.
            let uses_grpc = url.is_some()
                || file_type.is_some()
                || style.is_some()
                || details.is_some()
                || created_in_context.is_some()
                || created_in_context_ref.is_some();
            validate_upload_transport(http, uses_grpc)?;
            validate_rest_only_upload_options(mime.is_some(), stdin, uses_grpc)?;
            validate_upload_name(name.is_some(), stdin)?;
            if http {
                eprintln!(
                    "warning: --http is deprecated and no longer selects a transport; a plain upload already uses REST"
                );
            }
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let mut request = ctx.client.files().upload(&space_id);
            if let Some(path) = file {
                request = request.from_path(&path);
            } else if let Some(url) = url {
                request = request.from_url(url);
            } else if stdin {
                let name = name.ok_or_else(|| {
                    anyhow::anyhow!("--name is required when uploading from --stdin")
                })?;
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
                request = request.bytes(name, buf);
            }
            if let Some(mime) = mime {
                request = request.mime(mime);
            }
            if let Some(file_type) = file_type {
                request = request.file_type(file_type.into());
            }
            if let Some(style) = style {
                request = request.style(style.into());
            }
            if let Some(details) = details {
                request = request.details(parse_details(&details)?);
            }
            if let Some(object_id) = created_in_context {
                request = request.created_in_context(object_id);
            }
            if let Some(block_id) = created_in_context_ref {
                request = request.created_in_context_ref(block_id);
            }
            let file = request.upload().await?;
            if ctx.output.format() == OutputFormat::Table {
                return ctx.output.emit_table(&[file]);
            }
            ctx.output.emit_json(&file)
        }
        FileCommands::Preload {
            space,
            file,
            url,
            file_type,
            created_in_context,
            created_in_context_ref,
        } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            let mut request = ctx.client.files().preload(&space_id);
            if let Some(path) = file {
                request = request.from_path(&path);
            } else if let Some(url) = url {
                request = request.from_url(url);
            }
            if let Some(file_type) = file_type {
                request = request.file_type(file_type.into());
            }
            if let Some(object_id) = created_in_context {
                request = request.created_in_context(object_id);
            }
            if let Some(block_id) = created_in_context_ref {
                request = request.created_in_context_ref(block_id);
            }
            let preload_file_id = request.preload().await?;
            if ctx.output.format() == OutputFormat::Table {
                return ctx.output.emit_text(&preload_file_id);
            }
            ctx.output
                .emit_json(&json!({ "preload_file_id": preload_file_id }))
        }
        FileCommands::DiscardPreload { space, file_id } => {
            let space_id = ctx.client.resolve_space_id(&space).await?;
            ctx.client
                .files()
                .discard_preload(&space_id, &file_id)
                .discard()
                .await?;
            if ctx.output.format() == OutputFormat::Table {
                return ctx.output.emit_text(&format!("discarded {file_id}"));
            }
            ctx.output
                .emit_json(&json!({ "file_id": file_id, "discarded": true }))
        }
    }
}

/// REST download options that apply to the default `file download` path.
#[derive(Default)]
struct RestDownloadOptions {
    width: Option<u32>,
    range: Option<String>,
    if_match: Option<String>,
    if_none_match: Option<String>,
    if_modified_since: Option<String>,
    if_unmodified_since: Option<String>,
    if_range: Option<String>,
}

/// Download a file over the REST HTTP API, writing the bytes to `--file`, into
/// `--dir`, or to `<object_id>` in the current directory.
///
/// A `304 Not Modified` or a failed precondition (for example `412` or `416`)
/// leaves the destination untouched; only a successful response is written.
async fn download_http(
    ctx: &AppContext,
    object_id: &str,
    space: &str,
    dir: Option<PathBuf>,
    file: Option<PathBuf>,
    opts: RestDownloadOptions,
) -> Result<()> {
    let space_id = ctx.client.resolve_space_id(space).await?;
    let mut request = ctx.client.files().download_request(&space_id, object_id);
    if let Some(width) = opts.width {
        request = request.width(width);
    }
    if let Some(range) = opts.range {
        request = request.range(range);
    }
    if let Some(value) = opts.if_match {
        request = request.if_match(value);
    }
    if let Some(value) = opts.if_none_match {
        request = request.if_none_match(value);
    }
    if let Some(value) = opts.if_modified_since {
        request = request.if_modified_since(value);
    }
    if let Some(value) = opts.if_unmodified_since {
        request = request.if_unmodified_since(value);
    }
    if let Some(value) = opts.if_range {
        request = request.if_range(value);
    }
    let response = request.download().await?;
    let status = response.status.as_u16();
    let out_path = match (dir, file) {
        (_, Some(path)) => path,
        (Some(path), None) => path.join(object_id),
        (None, None) => PathBuf::from(object_id),
    };
    // Only overwrite the destination for a body-bearing success (200/206);
    // conditional and failed-precondition responses must not clobber the file.
    let written = if response.status.is_success() {
        std::fs::write(&out_path, &response.bytes)?;
        true
    } else {
        false
    };
    if ctx.output.format() == OutputFormat::Table {
        let path = if written {
            out_path.display().to_string()
        } else {
            "(not written)".to_string()
        };
        return ctx.output.emit_text(&format!("status {status} {path}"));
    }
    ctx.output.emit_json(&json!({
        "status": status,
        "written": written,
        "path": out_path,
        "bytes": response.bytes.len(),
        "metadata": metadata_json(&response.metadata),
    }))
}

/// Render the status line plus every present HTTP header field for the table
/// output of `file metadata` (its whole purpose is to surface the headers).
fn metadata_table(status: u16, metadata: &FileHttpMetadata) -> String {
    let mut lines = vec![format!("status {status}")];
    if let Some(value) = &metadata.content_type {
        lines.push(format!("content-type: {value}"));
    }
    if let Some(value) = metadata.content_length {
        lines.push(format!("content-length: {value}"));
    }
    if let Some(value) = &metadata.content_range {
        lines.push(format!("content-range: {value}"));
    }
    if let Some(value) = &metadata.accept_ranges {
        lines.push(format!("accept-ranges: {value}"));
    }
    if let Some(value) = &metadata.last_modified {
        lines.push(format!("last-modified: {value}"));
    }
    if let Some(value) = &metadata.etag {
        lines.push(format!("etag: {value}"));
    }
    if let Some(value) = &metadata.cache_control {
        lines.push(format!("cache-control: {value}"));
    }
    lines.join("\n")
}

/// Serialize the (non-`Serialize`) `FileHttpMetadata` fields into a JSON object.
fn metadata_json(metadata: &FileHttpMetadata) -> serde_json::Value {
    json!({
        "content_type": metadata.content_type,
        "content_length": metadata.content_length,
        "content_range": metadata.content_range,
        "accept_ranges": metadata.accept_ranges,
        "last_modified": metadata.last_modified,
        "etag": metadata.etag,
        "cache_control": metadata.cache_control,
    })
}

/// Parse the `--details` argument, which is either a JSON literal or `@FILE`
/// referencing a file whose contents are JSON.
fn parse_details(value: &str) -> Result<serde_json::Value> {
    let raw = if let Some(path) = value.strip_prefix('@') {
        if path.is_empty() {
            anyhow::bail!("--details @FILE path is empty");
        }
        std::fs::read_to_string(path).map_err(|err| anyhow::anyhow!("read {path}: {err}"))?
    } else {
        value.to_string()
    };
    serde_json::from_str(&raw).map_err(|err| anyhow::anyhow!("--details is not valid JSON: {err}"))
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
/// `--url`, `--file-type`, `--style`, `--details`, or a creation-context
/// option; that combination is an error instead.
fn validate_upload_transport(http: bool, uses_grpc: bool) -> Result<()> {
    if http && uses_grpc {
        anyhow::bail!(
            "--http cannot be combined with a gRPC-only option (--url/--file-type/--style/--details/--created-in-context*); drop --http to keep it"
        );
    }
    Ok(())
}

/// Reject REST-only upload options that a gRPC-only selector would silently
/// discard.
///
/// `--mime` and `--stdin` (an in-memory byte upload) are honored only by the
/// REST backend. Once a gRPC-only option (`--url`, `--file-type`, `--style`,
/// `--details`, or a creation-context option) promotes the upload to gRPC, the
/// backend drops the MIME type and rejects in-memory bytes. Rather than lose the
/// value silently (or fail late, after stdin has been consumed), reject the
/// combination up front.
fn validate_rest_only_upload_options(mime: bool, stdin: bool, uses_grpc: bool) -> Result<()> {
    if uses_grpc && mime {
        anyhow::bail!(
            "--mime is honored only by the REST upload backend and cannot be combined with a gRPC-only option (--url/--file-type/--style/--details/--created-in-context*)"
        );
    }
    if uses_grpc && stdin {
        anyhow::bail!(
            "--stdin uploads bytes in memory, which only the REST backend supports; it cannot be combined with a gRPC-only option (--file-type/--style/--details/--created-in-context*)"
        );
    }
    Ok(())
}

/// Reject `--name` for a non-`--stdin` upload.
///
/// The file name is recorded only for a `--stdin` byte upload; a `--file` or
/// `--url` upload derives its name elsewhere and would silently ignore `--name`.
/// (clap enforces the reverse — `--stdin` requires `--name` — but cannot require
/// a bare boolean flag, so this direction is validated here.)
fn validate_upload_name(name: bool, stdin: bool) -> Result<()> {
    if name && !stdin {
        anyhow::bail!("--name is only used with a --stdin upload; drop --name or add --stdin");
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

impl From<FileStyleArg> for FileStyle {
    fn from(value: FileStyleArg) -> Self {
        match value {
            FileStyleArg::Auto => Self::Auto,
            FileStyleArg::Link => Self::Link,
            FileStyleArg::Embed => Self::Embed,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{
        parse_details, validate_rest_only_upload_options, validate_upload_name,
        validate_upload_transport,
    };
    use crate::cli::{Cli, Commands, FileCommands, FileStyleArg};

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

    #[test]
    fn search_defaults_to_ascending_sort() {
        let command = file_command(&["anyr", "file", "search", "space", "--sort", "name"])
            .expect("search with --sort parses");
        match command {
            FileCommands::Search { sort, desc, .. } => {
                assert_eq!(sort.as_deref(), Some("name"));
                assert!(!desc);
            }
            other => panic!("expected search command, got {other:?}"),
        }
    }

    #[test]
    fn search_parses_descending_sort() {
        let command = file_command(&[
            "anyr", "file", "search", "space", "--sort", "size", "--desc",
        ])
        .expect("search with --sort --desc parses");
        match command {
            FileCommands::Search { sort, desc, .. } => {
                assert_eq!(sort.as_deref(), Some("size"));
                assert!(desc);
            }
            other => panic!("expected search command, got {other:?}"),
        }
    }

    #[test]
    fn search_desc_requires_sort() {
        // --desc is meaningless without --sort and must be rejected.
        assert!(file_command(&["anyr", "file", "search", "space", "--desc"]).is_err());
    }

    #[test]
    fn upload_requires_a_source() {
        assert!(file_command(&["anyr", "file", "upload", "space"]).is_err());
    }

    #[test]
    fn upload_sources_are_mutually_exclusive() {
        assert!(
            file_command(&[
                "anyr",
                "file",
                "upload",
                "space",
                "--file",
                "/tmp/x",
                "--url",
                "https://e/x",
            ])
            .is_err()
        );
    }

    #[test]
    fn upload_stdin_requires_name() {
        assert!(file_command(&["anyr", "file", "upload", "space", "--stdin"]).is_err());
    }

    #[test]
    fn upload_parses_stdin_with_name() {
        let command = file_command(&[
            "anyr", "file", "upload", "space", "--stdin", "--name", "note.txt",
        ])
        .expect("stdin upload with --name parses");
        match command {
            FileCommands::Upload { stdin, name, .. } => {
                assert!(stdin);
                assert_eq!(name.as_deref(), Some("note.txt"));
            }
            other => panic!("expected upload command, got {other:?}"),
        }
    }

    #[test]
    fn upload_parses_url_and_rich_options() {
        let command = file_command(&[
            "anyr",
            "file",
            "upload",
            "space",
            "--url",
            "https://e/x.png",
            "--style",
            "embed",
            "--file-type",
            "image",
            "--details",
            "{\"k\":1}",
            "--created-in-context",
            "obj1",
            "--created-in-context-ref",
            "blk1",
        ])
        .expect("rich URL upload parses");
        match command {
            FileCommands::Upload {
                url,
                style,
                file_type,
                details,
                created_in_context,
                created_in_context_ref,
                ..
            } => {
                assert_eq!(url.as_deref(), Some("https://e/x.png"));
                assert!(matches!(style, Some(FileStyleArg::Embed)));
                assert!(file_type.is_some());
                assert_eq!(details.as_deref(), Some("{\"k\":1}"));
                assert_eq!(created_in_context.as_deref(), Some("obj1"));
                assert_eq!(created_in_context_ref.as_deref(), Some("blk1"));
            }
            other => panic!("expected upload command, got {other:?}"),
        }
    }

    #[test]
    fn upload_transport_rejects_http_with_url() {
        // --url is a gRPC-only option; combining it with the deprecated --http errors.
        assert!(validate_upload_transport(true, true).is_err());
    }

    #[test]
    fn rest_only_options_reject_mime_with_grpc() {
        // --mime is REST-only; a gRPC-only selector would silently drop it.
        assert!(validate_rest_only_upload_options(true, false, true).is_err());
    }

    #[test]
    fn rest_only_options_reject_stdin_with_grpc() {
        // --stdin (in-memory bytes) is REST-only and must fail up front, not late.
        assert!(validate_rest_only_upload_options(false, true, true).is_err());
    }

    #[test]
    fn rest_only_options_accept_rest_upload() {
        // --mime and --stdin are fine when no gRPC-only selector is present.
        assert!(validate_rest_only_upload_options(true, true, false).is_ok());
    }

    #[test]
    fn upload_rejects_mime_with_grpc_selector() {
        // clap parses --mime with a gRPC-only option; the handler rejects it.
        let command = file_command(&[
            "anyr",
            "file",
            "upload",
            "space",
            "--url",
            "https://e/x",
            "--mime",
            "image/png",
        ])
        .expect("clap accepts --mime with --url");
        match command {
            FileCommands::Upload { mime, url, .. } => {
                assert_eq!(mime.as_deref(), Some("image/png"));
                assert!(url.is_some());
            }
            other => panic!("expected upload command, got {other:?}"),
        }
    }

    #[test]
    fn upload_name_requires_stdin() {
        // --name is honored only for --stdin uploads; a --file/--url upload must
        // reject it rather than silently ignore it.
        assert!(validate_upload_name(true, false).is_err());
        assert!(validate_upload_name(true, true).is_ok());
        assert!(validate_upload_name(false, false).is_ok());
    }

    #[test]
    fn parse_details_reads_json_literal() {
        let value = parse_details("{\"a\":1}").expect("valid JSON literal");
        assert_eq!(value["a"], 1);
    }

    #[test]
    fn parse_details_rejects_invalid_json() {
        assert!(parse_details("not json").is_err());
    }

    #[test]
    fn download_requires_space_and_object() {
        // REST is the default; SPACE is now a required positional.
        assert!(file_command(&["anyr", "file", "download", "file-1"]).is_err());
    }

    #[test]
    fn download_parses_space_object_and_rest_options() {
        let command = file_command(&[
            "anyr",
            "file",
            "download",
            "space",
            "file-1",
            "--width",
            "128",
            "--range",
            "bytes=0-9",
            "--if-none-match",
            "\"etag\"",
        ])
        .expect("default REST download parses with options");
        match command {
            FileCommands::Download {
                space,
                object_id,
                width,
                range,
                if_none_match,
                ..
            } => {
                assert_eq!(space, "space");
                assert_eq!(object_id, "file-1");
                assert_eq!(width, Some(128));
                assert_eq!(range.as_deref(), Some("bytes=0-9"));
                assert_eq!(if_none_match.as_deref(), Some("\"etag\""));
            }
            other => panic!("expected download command, got {other:?}"),
        }
    }

    #[test]
    fn download_rejects_removed_http_and_space_flags() {
        // --http/--space were removed; REST is unconditional now.
        assert!(file_command(&["anyr", "file", "download", "space", "file-1", "--http"]).is_err());
        assert!(
            file_command(&[
                "anyr", "file", "download", "space", "file-1", "--space", "other",
            ])
            .is_err()
        );
    }

    #[test]
    fn download_destination_is_mutually_exclusive() {
        assert!(
            file_command(&[
                "anyr", "file", "download", "space", "file-1", "--dir", "/tmp", "--file", "/tmp/x",
            ])
            .is_err()
        );
    }

    #[test]
    fn download_rejects_removed_via_heart_subcommand() {
        // The legacy gRPC Heart download path was removed; REST is the only
        // download command, so `download-via-heart` must no longer parse.
        assert!(file_command(&["anyr", "file", "download-via-heart", "file-1"]).is_err());
    }

    #[test]
    fn metadata_parses_space_object_and_conditionals() {
        let command = file_command(&[
            "anyr",
            "file",
            "metadata",
            "space",
            "file-1",
            "--width",
            "64",
            "--if-match",
            "\"e\"",
        ])
        .expect("metadata command parses");
        match command {
            FileCommands::Metadata {
                space,
                object_id,
                width,
                if_match,
                ..
            } => {
                assert_eq!(space, "space");
                assert_eq!(object_id, "file-1");
                assert_eq!(width, Some(64));
                assert_eq!(if_match.as_deref(), Some("\"e\""));
            }
            other => panic!("expected metadata command, got {other:?}"),
        }
    }

    #[test]
    fn preload_parses_file_and_context() {
        let command = file_command(&[
            "anyr",
            "file",
            "preload",
            "space",
            "--file",
            "/tmp/x",
            "--file-type",
            "image",
            "--created-in-context",
            "obj1",
        ])
        .expect("preload command parses");
        match command {
            FileCommands::Preload {
                space,
                file,
                url,
                file_type,
                created_in_context,
                ..
            } => {
                assert_eq!(space, "space");
                assert_eq!(file.as_deref(), Some(std::path::Path::new("/tmp/x")));
                assert!(url.is_none());
                assert!(file_type.is_some());
                assert_eq!(created_in_context.as_deref(), Some("obj1"));
            }
            other => panic!("expected preload command, got {other:?}"),
        }
    }

    #[test]
    fn preload_parses_url_source() {
        let command = file_command(&[
            "anyr",
            "file",
            "preload",
            "space",
            "--url",
            "https://e/x.png",
        ])
        .expect("preload from --url parses");
        match command {
            FileCommands::Preload { file, url, .. } => {
                assert!(file.is_none());
                assert_eq!(url.as_deref(), Some("https://e/x.png"));
            }
            other => panic!("expected preload command, got {other:?}"),
        }
    }

    #[test]
    fn preload_requires_a_source() {
        assert!(file_command(&["anyr", "file", "preload", "space"]).is_err());
    }

    #[test]
    fn preload_sources_are_mutually_exclusive() {
        assert!(
            file_command(&[
                "anyr",
                "file",
                "preload",
                "space",
                "--file",
                "/tmp/x",
                "--url",
                "https://e/x",
            ])
            .is_err()
        );
    }

    #[test]
    fn discard_preload_parses_space_and_file_id() {
        let command = file_command(&["anyr", "file", "discard-preload", "space", "preload-1"])
            .expect("discard-preload command parses");
        match command {
            FileCommands::DiscardPreload { space, file_id } => {
                assert_eq!(space, "space");
                assert_eq!(file_id, "preload-1");
            }
            other => panic!("expected discard-preload command, got {other:?}"),
        }
    }
}
