//! # Anytype Files (gRPC)
//!
//! gRPC-backed file operations: list/search files, upload/download, and preload flows.
//!

use std::path::{Path, PathBuf};

#[cfg(feature = "grpc")]
use anytype_rpc::{
    anytype::rpc::{
        file::{discard_preload, download, upload},
        object::search_with_meta,
    },
    model,
};
use chrono::{DateTime, FixedOffset};
use prost_types::{ListValue, Struct, Value};
use serde::{Deserialize, Serialize};
use serde_json::Number;
use tonic::Request;
use tracing::{debug, error, info};

use crate::{
    Result,
    client::AnytypeClient,
    error::AnytypeError,
    filters::{Filter, Sort, SortDirection},
    grpc_util::{ensure_error_ok, grpc_status, with_token_request},
    paged::{PagedResult, PaginatedResponse, PaginationMeta},
};

// ============================================================================
// Public types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileObject {
    pub id: String,
    pub space_id: String,
    pub name: Option<String>,
    pub size: Option<i64>,
    pub mime: Option<String>,
    pub added_at: Option<DateTime<FixedOffset>>,
    #[serde(default)]
    pub file_type: FileType,
    pub style: FileStyle,
    pub target_object_id: Option<String>,
    pub details: serde_json::Value,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, strum::EnumString, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum FileType {
    #[default]
    File,
    Image,
    Video,
    Audio,
    Pdf,
    /// catch-all in case other types added in the future
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, strum::EnumString, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum FileStyle {
    Auto,
    Link,
    Embed,
}

// ============================================================================
// Client entry point
// ============================================================================

#[derive(Debug)]
pub struct FilesClient<'a> {
    client: &'a AnytypeClient,
}

impl AnytypeClient {
    #[must_use]
    pub fn files(&self) -> FilesClient<'_> {
        FilesClient { client: self }
    }
}

impl<'a> FilesClient<'a> {
    pub fn list(&self, space_id: impl Into<String>) -> FileListRequest<'a> {
        FileListRequest {
            client: self.client,
            space_id: space_id.into(),
            filters: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    pub fn search(&self, space_id: impl Into<String>) -> FileSearchRequest<'a> {
        FileSearchRequest {
            client: self.client,
            space_id: space_id.into(),
            text: None,
            filters: Vec::new(),
            sorts: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    pub fn get(
        &self,
        space_id: impl Into<String>,
        object_id: impl Into<String>,
    ) -> FileGetRequest<'a> {
        FileGetRequest {
            client: self.client,
            space_id: space_id.into(),
            object_id: object_id.into(),
        }
    }

    pub fn download(&self, object_id: impl Into<String>) -> FileDownloadRequest<'a> {
        FileDownloadRequest {
            client: self.client,
            object_id: object_id.into(),
            destination: None,
        }
    }

    pub fn upload(&self, space_id: impl Into<String>) -> FileUploadRequest<'a> {
        FileUploadRequest {
            client: self.client,
            space_id: space_id.into(),
            source: None,
            file_type: None,
            style: None,
            details: None,
            created_in_context: None,
            created_in_context_ref: None,
        }
    }

    pub fn preload(&self, space_id: impl Into<String>) -> FilePreloadRequest<'a> {
        FilePreloadRequest {
            client: self.client,
            space_id: space_id.into(),
            source: None,
            file_type: None,
            created_in_context: None,
            created_in_context_ref: None,
        }
    }

    pub fn discard_preload(
        &self,
        space_id: impl Into<String>,
        file_id: impl Into<String>,
    ) -> FileDiscardPreloadRequest<'a> {
        FileDiscardPreloadRequest {
            client: self.client,
            space_id: space_id.into(),
            file_id: file_id.into(),
        }
    }
}

// ============================================================================
// Request builders
// ============================================================================

pub struct FileListRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    filters: Vec<Filter>,
    limit: Option<u32>,
    offset: Option<u32>,
}

impl FileListRequest<'_> {
    /// list files with the text in the name
    #[must_use]
    pub fn name_contains(mut self, text: impl Into<String>) -> Self {
        self.filters.push(Filter::Text {
            condition: crate::filters::Condition::Contains,
            property_key: "name".to_string(),
            text: text.into(),
        });
        self
    }

    /// list files of a specific type
    #[must_use]
    pub fn file_type(mut self, file_type: &FileType) -> Self {
        if let Some(filter) = file_type_filter(file_type) {
            self.filters.push(filter);
        }
        self
    }

    /// List files with the extension
    #[must_use]
    pub fn extension(mut self, ext: impl Into<String>) -> Self {
        self.filters.push(Filter::Text {
            condition: crate::filters::Condition::Equal,
            property_key: "fileExt".to_string(),
            text: ext.into(),
        });
        self
    }

    /// List files with one of the extensions
    #[must_use]
    pub fn extension_in(mut self, extensions: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.filters.push(Filter::Select {
            condition: crate::filters::Condition::In,
            property_key: "fileExt".to_string(),
            select: extensions.into_iter().map(Into::into).collect(),
        });
        self
    }

    /// List files that don't have one of these extensions
    #[must_use]
    pub fn extension_not_in(
        mut self,
        extensions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.filters.push(Filter::Select {
            condition: crate::filters::Condition::NotIn,
            property_key: "fileExt".to_string(),
            select: extensions.into_iter().map(Into::into).collect(),
        });
        self
    }

    /// list files with size
    #[must_use]
    pub fn size_eq(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::Equal, size));
        self
    }

    #[must_use]
    pub fn size_neq(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::NotEqual, size));
        self
    }

    #[must_use]
    pub fn size_lt(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::Less, size));
        self
    }

    #[must_use]
    pub fn size_lte(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::LessOrEqual, size));
        self
    }

    #[must_use]
    pub fn size_gt(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::Greater, size));
        self
    }

    #[must_use]
    pub fn size_gte(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::GreaterOrEqual, size));
        self
    }

    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub async fn list(self) -> Result<PagedResult<FileObject>> {
        search_files(
            self.client,
            &self.space_id,
            None,
            self.filters,
            Vec::new(),
            self.limit,
            self.offset,
        )
        .await
    }
}

pub struct FileSearchRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    text: Option<String>,
    filters: Vec<Filter>,
    sorts: Vec<Sort>,
    limit: Option<u32>,
    offset: Option<u32>,
}

impl FileSearchRequest<'_> {
    #[must_use]
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    #[must_use]
    pub fn name_contains(mut self, text: impl Into<String>) -> Self {
        self.filters.push(Filter::Text {
            condition: crate::filters::Condition::Contains,
            property_key: "name".to_string(),
            text: text.into(),
        });
        self
    }

    #[must_use]
    pub fn file_type(mut self, file_type: &FileType) -> Self {
        if let Some(filter) = file_type_filter(file_type) {
            self.filters.push(filter);
        }
        self
    }

    #[must_use]
    pub fn extension(mut self, ext: impl Into<String>) -> Self {
        self.filters.push(Filter::Text {
            condition: crate::filters::Condition::Equal,
            property_key: "fileExt".to_string(),
            text: ext.into(),
        });
        self
    }

    #[must_use]
    pub fn extension_in(mut self, extensions: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.filters.push(Filter::Select {
            condition: crate::filters::Condition::In,
            property_key: "fileExt".to_string(),
            select: extensions.into_iter().map(Into::into).collect(),
        });
        self
    }

    #[must_use]
    pub fn extension_not_in(
        mut self,
        extensions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.filters.push(Filter::Select {
            condition: crate::filters::Condition::NotIn,
            property_key: "fileExt".to_string(),
            select: extensions.into_iter().map(Into::into).collect(),
        });
        self
    }

    #[must_use]
    pub fn size_eq(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::Equal, size));
        self
    }

    #[must_use]
    pub fn size_neq(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::NotEqual, size));
        self
    }

    #[must_use]
    pub fn size_lt(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::Less, size));
        self
    }

    #[must_use]
    pub fn size_lte(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::LessOrEqual, size));
        self
    }

    #[must_use]
    pub fn size_gt(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::Greater, size));
        self
    }

    #[must_use]
    pub fn size_gte(mut self, size: i64) -> Self {
        self.filters
            .push(size_filter(crate::filters::Condition::GreaterOrEqual, size));
        self
    }

    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    #[must_use]
    pub fn sort_asc(mut self, key: impl Into<String>) -> Self {
        self.sorts.push(Sort::asc(key));
        self
    }

    #[must_use]
    pub fn sort_desc(mut self, key: impl Into<String>) -> Self {
        self.sorts.push(Sort::desc(key));
        self
    }

    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub async fn search(self) -> Result<PagedResult<FileObject>> {
        search_files(
            self.client,
            &self.space_id,
            self.text,
            self.filters,
            self.sorts,
            self.limit,
            self.offset,
        )
        .await
    }
}

pub struct FileGetRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    object_id: String,
}

impl FileGetRequest<'_> {
    pub async fn get(self) -> Result<FileObject> {
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = search_with_meta::Request {
            space_id: self.space_id.clone(),
            filters: vec![filter_id_equal(&self.object_id)],
            sorts: Vec::new(),
            full_text: String::new(),
            offset: 0,
            limit: 1,
            object_type_filter: Vec::new(),
            keys: Vec::new(),
            return_meta: false,
            return_meta_relation_details: false,
            return_html_highlights_instead_of_ranges: false,
        };
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .object_search_with_meta(request)
            .await
            .map_err(grpc_status)?
            .into_inner();
        ensure_error_ok(response.error.as_ref(), "file get")?;
        let result = response
            .results
            .first()
            .ok_or_else(|| AnytypeError::Other {
                message: "file not found".to_string(),
            })?;
        let details = result.details.as_ref().ok_or_else(|| AnytypeError::Other {
            message: "file result missing details".to_string(),
        })?;
        Ok(file_from_details(
            &self.space_id,
            &result.object_id,
            details,
        ))
    }
}

pub struct FileDownloadRequest<'a> {
    client: &'a AnytypeClient,
    object_id: String,
    destination: Option<FileDownloadDestination>,
}

#[derive(Debug, Clone)]
enum FileDownloadDestination {
    Dir(PathBuf),
    File(PathBuf),
}

impl FileDownloadRequest<'_> {
    /// set the destination directory for the download
    #[must_use]
    pub fn to_path(mut self, path: impl AsRef<Path>) -> Self {
        self.destination = Some(FileDownloadDestination::Dir(path.as_ref().to_path_buf()));
        self
    }

    /// set the destination directory for the download
    #[must_use]
    pub fn to_dir(mut self, path: impl AsRef<Path>) -> Self {
        self.destination = Some(FileDownloadDestination::Dir(path.as_ref().to_path_buf()));
        self
    }

    /// set the destination file path for the download
    #[must_use]
    pub fn to_file(mut self, path: impl AsRef<Path>) -> Self {
        self.destination = Some(FileDownloadDestination::File(path.as_ref().to_path_buf()));
        self
    }

    /// Download the file. Returns the path to the file
    pub async fn download(self) -> Result<PathBuf> {
        debug!("enter download execute");
        let (request_path, target_file) = match self.destination {
            Some(FileDownloadDestination::Dir(path)) => (path, None),
            Some(FileDownloadDestination::File(path)) => {
                if path.is_dir() {
                    return Err(AnytypeError::Validation {
                        message: format!("download destination is a directory: {}", path.display()),
                    });
                }
                let parent = path
                    .parent()
                    .filter(|value| !value.as_os_str().is_empty())
                    .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
                if let Err(err) = std::fs::create_dir_all(&parent) {
                    return Err(AnytypeError::Other {
                        message: format!("create download directory {}: {err}", parent.display()),
                    });
                }
                (parent, Some(path))
            }
            None => (PathBuf::new(), None),
        };
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = download::Request {
            object_id: self.object_id.clone(),
            path: if request_path.as_os_str().is_empty() {
                String::new()
            } else {
                request_path.to_string_lossy().to_string()
            },
        };
        let request = with_token_request(Request::new(request), grpc.token()).map_err(|err| {
            error!("download rpc error: {err}");
            err
        })?;

        let response = commands
            .file_download(request)
            .await
            .map_err(|err| {
                error!("download error grpc_status {err:?}");
                grpc_status(err)
            })?
            .into_inner();

        // remove partial files if there was an error
        if let Err(err) = ensure_error_ok(response.error.as_ref(), "file download") {
            let local = PathBuf::from(response.local_path);
            if local.is_file() {
                info!("download error {err}. Removing incomplete download {local:?}");
                if let Err(delete_err) = std::fs::remove_file(&local) {
                    error!(
                        "failed to remove incomplete download {local:?} (err={delete_err}) after download error {err}"
                    );
                }
            } else {
                error!("download error {err}");
            }
            return Err(err);
        }
        let mut local_path = PathBuf::from(response.local_path);
        if let Some(target_path) = target_file {
            if target_path.is_dir() {
                return Err(AnytypeError::Validation {
                    message: format!(
                        "download file path points to a directory: {}",
                        target_path.display()
                    ),
                });
            }
            if local_path != target_path {
                if let Err(err) = std::fs::rename(&local_path, &target_path) {
                    if let Err(copy_err) = std::fs::copy(&local_path, &target_path) {
                        return Err(AnytypeError::Other {
                            message: format!(
                                "move download to {}: {err} (copy error: {copy_err})",
                                target_path.display()
                            ),
                        });
                    }
                    if let Err(remove_err) = std::fs::remove_file(&local_path) {
                        error!(
                            "failed to remove original download {local_path:?} after copy: {remove_err}"
                        );
                    }
                }
                local_path = target_path;
            }
        }
        debug!("download complete 536 {}", &local_path.display());
        Ok(local_path)
    }
}

pub struct FileUploadRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    source: Option<FileSource>,
    file_type: Option<FileType>,
    style: Option<FileStyle>,
    details: Option<serde_json::Value>,
    created_in_context: Option<String>,
    created_in_context_ref: Option<String>,
}

impl FileUploadRequest<'_> {
    #[must_use]
    pub fn from_path(mut self, path: impl AsRef<Path>) -> Self {
        self.source = Some(FileSource::Path(path.as_ref().to_path_buf()));
        self
    }

    #[must_use]
    pub fn from_url(mut self, url: impl Into<String>) -> Self {
        self.source = Some(FileSource::Url(url.into()));
        self
    }

    #[must_use]
    pub fn file_type(mut self, file_type: FileType) -> Self {
        self.file_type = Some(file_type);
        self
    }

    #[must_use]
    pub fn style(mut self, style: FileStyle) -> Self {
        self.style = Some(style);
        self
    }

    #[must_use]
    pub fn details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    #[must_use]
    pub fn created_in_context(mut self, object_id: impl Into<String>) -> Self {
        self.created_in_context = Some(object_id.into());
        self
    }

    #[must_use]
    pub fn created_in_context_ref(mut self, block_id: impl Into<String>) -> Self {
        self.created_in_context_ref = Some(block_id.into());
        self
    }

    pub async fn upload(self) -> Result<FileObject> {
        let result = upload_file(
            self.client,
            &self.space_id,
            self.source,
            self.file_type,
            self.style,
            self.details,
            self.created_in_context,
            self.created_in_context_ref,
            false,
            None,
        )
        .await?;
        Ok(file_from_details(
            &self.space_id,
            &result.object_id,
            &result.details,
        ))
    }
}

pub struct FilePreloadRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    source: Option<FileSource>,
    file_type: Option<FileType>,
    created_in_context: Option<String>,
    created_in_context_ref: Option<String>,
}

impl FilePreloadRequest<'_> {
    #[must_use]
    pub fn from_path(mut self, path: impl AsRef<Path>) -> Self {
        self.source = Some(FileSource::Path(path.as_ref().to_path_buf()));
        self
    }

    #[must_use]
    pub fn file_type(mut self, file_type: FileType) -> Self {
        self.file_type = Some(file_type);
        self
    }

    #[must_use]
    pub fn created_in_context(mut self, object_id: impl Into<String>) -> Self {
        self.created_in_context = Some(object_id.into());
        self
    }

    #[must_use]
    pub fn created_in_context_ref(mut self, block_id: impl Into<String>) -> Self {
        self.created_in_context_ref = Some(block_id.into());
        self
    }

    pub async fn preload(self) -> Result<String> {
        let result = upload_file(
            self.client,
            &self.space_id,
            self.source,
            self.file_type,
            None,
            None,
            self.created_in_context,
            self.created_in_context_ref,
            true,
            None,
        )
        .await?;
        Ok(result.preload_file_id)
    }
}

pub struct FileDiscardPreloadRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    file_id: String,
}

impl FileDiscardPreloadRequest<'_> {
    pub async fn discard(self) -> Result<()> {
        let grpc = self.client.grpc_client().await?;
        let mut commands = grpc.client_commands();
        let request = discard_preload::Request {
            file_id: self.file_id,
            space_id: self.space_id,
        };
        let request = with_token_request(Request::new(request), grpc.token())?;
        let response = commands
            .file_discard_preload(request)
            .await
            .map_err(grpc_status)?
            .into_inner();
        ensure_error_ok(response.error.as_ref(), "file discard preload")?;
        Ok(())
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

#[derive(Debug)]
enum FileSource {
    Url(String),
    Path(PathBuf),
}

async fn search_files(
    client: &AnytypeClient,
    space_id: &str,
    text: Option<String>,
    filters: Vec<Filter>,
    sorts: Vec<Sort>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<PagedResult<FileObject>> {
    let grpc = client.grpc_client().await?;
    let mut commands = grpc.client_commands();

    let mut grpc_filters = Vec::with_capacity(filters.len() + 1);
    grpc_filters.push(filter_not_empty("fileId"));
    for filter in filters {
        grpc_filters.push(filter_to_dataview(filter)?);
    }

    let mut grpc_sorts = Vec::with_capacity(sorts.len());
    for sort in sorts {
        grpc_sorts.push(sort_to_dataview(sort));
    }

    #[allow(clippy::cast_possible_wrap)] // u32 to i32 for offset and limit
    let request = search_with_meta::Request {
        space_id: space_id.to_string(),
        filters: grpc_filters,
        sorts: grpc_sorts,
        full_text: text.unwrap_or_default(),
        offset: offset.unwrap_or_default() as i32,
        limit: limit.unwrap_or(100) as i32,
        object_type_filter: Vec::new(),
        keys: Vec::new(),
        return_meta: false,
        return_meta_relation_details: false,
        return_html_highlights_instead_of_ranges: false,
    };

    let request = with_token_request(Request::new(request), grpc.token())?;
    let response = commands
        .object_search_with_meta(request)
        .await
        .map_err(grpc_status)?
        .into_inner();
    ensure_error_ok(response.error.as_ref(), "file search")?;

    let items: Vec<FileObject> = response
        .results
        .into_iter()
        .filter_map(|result| {
            let details = result.details.as_ref()?;
            Some(file_from_details(space_id, &result.object_id, details))
        })
        .collect();

    let limit_value = limit.unwrap_or(100);
    let has_more = items.len() == limit_value as usize;
    let total = offset.unwrap_or_default() as usize + items.len();
    let response = PaginatedResponse {
        items,
        pagination: PaginationMeta {
            has_more,
            limit: limit_value,
            offset: offset.unwrap_or_default(),
            total,
        },
    };
    Ok(PagedResult::from_response(response))
}

struct UploadResult {
    object_id: String,
    preload_file_id: String,
    details: Struct,
}

#[allow(clippy::too_many_arguments)]
async fn upload_file(
    client: &AnytypeClient,
    space_id: &str,
    source: Option<FileSource>,
    file_type: Option<FileType>,
    style: Option<FileStyle>,
    details: Option<serde_json::Value>,
    created_in_context: Option<String>,
    created_in_context_ref: Option<String>,
    preload_only: bool,
    preload_file_id: Option<String>,
) -> Result<UploadResult> {
    let source = source.ok_or_else(|| AnytypeError::Validation {
        message: "file upload requires a source (path or url)".to_string(),
    })?;

    let grpc = client.grpc_client().await?;
    let mut commands = grpc.client_commands();
    let (url, local_path) = match source {
        FileSource::Url(url) => (url, String::new()),
        FileSource::Path(path) => (String::new(), path.to_string_lossy().to_string()),
    };

    let request = upload::Request {
        space_id: space_id.to_string(),
        url,
        local_path,
        r#type: grpc_file_type(&file_type.unwrap_or(FileType::File)),
        disable_encryption: false,
        style: grpc_file_style(&style.unwrap_or(FileStyle::Auto)),
        details: details.map(json_to_struct).transpose()?,
        origin: 0,
        image_kind: 0,
        preload_only,
        preload_file_id: preload_file_id.unwrap_or_default(),
        created_in_context: created_in_context.unwrap_or_default(),
        created_in_context_ref: created_in_context_ref.unwrap_or_default(),
    };
    let request = with_token_request(Request::new(request), grpc.token())?;
    let response = commands
        .file_upload(request)
        .await
        .map_err(grpc_status)?
        .into_inner();
    ensure_error_ok(response.error.as_ref(), "file upload")?;
    let details = response.details.unwrap_or_default();
    Ok(UploadResult {
        object_id: response.object_id,
        preload_file_id: response.preload_file_id,
        details,
    })
}

fn grpc_file_type(file_type: &FileType) -> i32 {
    match file_type {
        &FileType::File | &FileType::Other(_) => model::block::content::file::Type::File as i32,
        &FileType::Image => model::block::content::file::Type::Image as i32,
        &FileType::Video => model::block::content::file::Type::Video as i32,
        &FileType::Audio => model::block::content::file::Type::Audio as i32,
        &FileType::Pdf => model::block::content::file::Type::Pdf as i32,
    }
}

fn grpc_file_style(style: &FileStyle) -> i32 {
    match style {
        FileStyle::Auto => model::block::content::file::Style::Auto as i32,
        FileStyle::Link => model::block::content::file::Style::Link as i32,
        FileStyle::Embed => model::block::content::file::Style::Embed as i32,
    }
}

fn filter_not_empty(key: &str) -> model::block::content::dataview::Filter {
    model::block::content::dataview::Filter {
        id: String::new(),
        operator: model::block::content::dataview::filter::Operator::No as i32,
        relation_key: key.to_string(),
        relation_property: String::new(),
        condition: model::block::content::dataview::filter::Condition::NotEmpty as i32,
        value: None,
        quick_option: model::block::content::dataview::filter::QuickOption::ExactDate as i32,
        format: 0,
        include_time: false,
        nested_filters: Vec::new(),
    }
}

fn filter_id_equal(id: &str) -> model::block::content::dataview::Filter {
    model::block::content::dataview::Filter {
        id: String::new(),
        operator: model::block::content::dataview::filter::Operator::No as i32,
        relation_key: "id".to_string(),
        relation_property: String::new(),
        condition: model::block::content::dataview::filter::Condition::Equal as i32,
        value: Some(value_string(id.to_string())),
        quick_option: model::block::content::dataview::filter::QuickOption::ExactDate as i32,
        format: 0,
        include_time: false,
        nested_filters: Vec::new(),
    }
}

#[allow(clippy::too_many_lines)]
fn filter_to_dataview(filter: Filter) -> Result<model::block::content::dataview::Filter> {
    let (relation_key, condition, value) = match filter {
        Filter::Text {
            condition,
            property_key,
            text: str,
        }
        | Filter::Date {
            condition,
            property_key,
            date: str,
        }
        | Filter::Url {
            condition,
            property_key,
            url: str,
        }
        | Filter::Email {
            condition,
            property_key,
            email: str,
        }
        | Filter::Phone {
            condition,
            property_key,
            phone: str,
        } => (property_key, condition, Some(value_string(str))),
        Filter::Number {
            condition,
            property_key,
            number,
        } => {
            let number = number.as_f64().ok_or_else(|| AnytypeError::Validation {
                message: "number filter must be numeric".to_string(),
            })?;
            (property_key, condition, Some(value_number(number)))
        }
        Filter::Select {
            condition,
            property_key,
            select,
        } => (
            property_key,
            condition,
            Some(value_list(select.into_iter().map(value_string).collect())),
        ),
        Filter::MultiSelect {
            condition,
            property_key,
            multi_select,
        } => (
            property_key,
            condition,
            Some(value_list(
                multi_select.into_iter().map(value_string).collect(),
            )),
        ),
        Filter::Checkbox {
            condition,
            property_key,
            checkbox,
        } => (property_key, condition, Some(value_bool(checkbox))),
        Filter::Files {
            condition,
            property_key,
            files,
        } => (
            property_key,
            condition,
            Some(value_list(files.into_iter().map(value_string).collect())),
        ),
        Filter::Objects {
            condition,
            property_key,
            objects,
        } => (
            property_key,
            condition,
            Some(value_list(objects.into_iter().map(value_string).collect())),
        ),
        Filter::Empty {
            condition,
            property_key,
        }
        | Filter::NotEmpty {
            condition,
            property_key,
        } => (property_key, condition, None),
        Filter::Value {
            condition,
            property_key,
            value,
        } => (
            property_key,
            condition,
            value.map(json_value_to_prost).transpose()?,
        ),
    };

    Ok(model::block::content::dataview::Filter {
        id: String::new(),
        operator: model::block::content::dataview::filter::Operator::No as i32,
        relation_key,
        relation_property: String::new(),
        condition: grpc_filter_condition(condition),
        value,
        quick_option: model::block::content::dataview::filter::QuickOption::ExactDate as i32,
        format: 0,
        include_time: false,
        nested_filters: Vec::new(),
    })
}

fn grpc_filter_condition(condition: crate::filters::Condition) -> i32 {
    use model::block::content::dataview::filter::Condition as GrpcCondition;

    use crate::filters::Condition;

    match condition {
        Condition::None => GrpcCondition::None as i32,
        Condition::Equal => GrpcCondition::Equal as i32,
        Condition::NotEqual => GrpcCondition::NotEqual as i32,
        Condition::Greater => GrpcCondition::Greater as i32,
        Condition::Less => GrpcCondition::Less as i32,
        Condition::GreaterOrEqual => GrpcCondition::GreaterOrEqual as i32,
        Condition::LessOrEqual => GrpcCondition::LessOrEqual as i32,
        Condition::Contains => GrpcCondition::Like as i32,
        Condition::NotContains => GrpcCondition::NotLike as i32,
        Condition::In => GrpcCondition::In as i32,
        Condition::NotIn => GrpcCondition::NotIn as i32,
        Condition::Empty => GrpcCondition::Empty as i32,
        Condition::NotEmpty => GrpcCondition::NotEmpty as i32,
        Condition::All | Condition::AllIn => GrpcCondition::AllIn as i32,
        Condition::NotAllIn => GrpcCondition::NotAllIn as i32,
        Condition::ExactIn => GrpcCondition::ExactIn as i32,
        Condition::NotExactIn => GrpcCondition::NotExactIn as i32,
        Condition::Exists => GrpcCondition::Exists as i32,
    }
}

fn sort_to_dataview(sort: Sort) -> model::block::content::dataview::Sort {
    let sort_type = match sort.direction {
        SortDirection::Asc => model::block::content::dataview::sort::Type::Asc,
        SortDirection::Desc => model::block::content::dataview::sort::Type::Desc,
    };

    model::block::content::dataview::Sort {
        relation_key: sort.property_key,
        r#type: sort_type as i32,
        custom_order: Vec::new(),
        format: 0,
        include_time: false,
        id: String::new(),
        empty_placement: 0,
        no_collate: false,
    }
}

fn file_from_details(space_id: &str, object_id: &str, details: &Struct) -> FileObject {
    let name = string_field(details, "name");
    #[allow(clippy::cast_possible_truncation)]
    let size = number_field(details, "sizeInBytes").map(|val| val as i64);
    let mime = string_field(details, "fileMimeType");
    let added_at = string_field(details, "addedDate")
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok());
    let target_object_id = string_field(details, "targetObjectId");
    let file_type = mime.as_deref().map(file_type_from_mime).unwrap_or_default();

    FileObject {
        id: object_id.to_string(),
        space_id: space_id.to_string(),
        name,
        size,
        mime,
        added_at,
        file_type,
        style: FileStyle::Auto,
        target_object_id,
        details: struct_to_json(details),
    }
}

fn file_type_from_mime(mime: &str) -> FileType {
    if mime.starts_with("image/") {
        return FileType::Image;
    }
    if mime.starts_with("video/") {
        return FileType::Video;
    }
    if mime.starts_with("audio/") {
        return FileType::Audio;
    }
    if mime == "application/pdf" {
        return FileType::Pdf;
    }
    FileType::File
}

fn size_filter(condition: crate::filters::Condition, size: i64) -> Filter {
    Filter::Number {
        condition,
        property_key: "sizeInBytes".to_string(),
        number: serde_json::Number::from(size),
    }
}

fn file_type_filter(file_type: &FileType) -> Option<Filter> {
    let (condition, value) = match file_type {
        FileType::Image => (crate::filters::Condition::Contains, "image/".to_string()),
        FileType::Video => (crate::filters::Condition::Contains, "video/".to_string()),
        FileType::Audio => (crate::filters::Condition::Contains, "audio/".to_string()),
        FileType::Pdf => (
            crate::filters::Condition::Equal,
            "application/pdf".to_string(),
        ),
        FileType::File | FileType::Other(_) => return None,
    };

    Some(Filter::Text {
        condition,
        property_key: "fileMimeType".to_string(),
        text: value,
    })
}

fn string_field(details: &Struct, key: &str) -> Option<String> {
    details.fields.get(key).and_then(|value| match &value.kind {
        Some(prost_types::value::Kind::StringValue(value)) => Some(value.clone()),
        _ => None,
    })
}

fn number_field(details: &Struct, key: &str) -> Option<f64> {
    details.fields.get(key).and_then(|value| match &value.kind {
        Some(prost_types::value::Kind::NumberValue(value)) => Some(*value),
        _ => None,
    })
}

fn struct_to_json(details: &Struct) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in &details.fields {
        map.insert(key.clone(), prost_value_to_json(value));
    }
    serde_json::Value::Object(map)
}

fn prost_value_to_json(value: &Value) -> serde_json::Value {
    match &value.kind {
        Some(prost_types::value::Kind::NullValue(_)) | None => serde_json::Value::Null,
        Some(prost_types::value::Kind::NumberValue(value)) => {
            Number::from_f64(*value).map_or(serde_json::Value::Null, serde_json::Value::Number)
        }
        Some(prost_types::value::Kind::StringValue(value)) => {
            serde_json::Value::String(value.clone())
        }
        Some(prost_types::value::Kind::BoolValue(value)) => serde_json::Value::Bool(*value),
        Some(prost_types::value::Kind::StructValue(value)) => struct_to_json(value),
        Some(prost_types::value::Kind::ListValue(value)) => {
            serde_json::Value::Array(value.values.iter().map(prost_value_to_json).collect())
        }
    }
}

fn json_to_struct(value: serde_json::Value) -> Result<Struct> {
    match json_value_to_prost(value)? {
        Value {
            kind: Some(prost_types::value::Kind::StructValue(value)),
        } => Ok(value),
        _ => Err(AnytypeError::Validation {
            message: "details must be an object".to_string(),
        }),
    }
}

fn json_value_to_prost(value: serde_json::Value) -> Result<Value> {
    Ok(match value {
        serde_json::Value::Null => Value {
            kind: Some(prost_types::value::Kind::NullValue(0)),
        },
        serde_json::Value::Bool(value) => Value {
            kind: Some(prost_types::value::Kind::BoolValue(value)),
        },
        serde_json::Value::Number(value) => Value {
            kind: Some(prost_types::value::Kind::NumberValue(
                value.as_f64().unwrap_or_default(),
            )),
        },
        serde_json::Value::String(value) => Value {
            kind: Some(prost_types::value::Kind::StringValue(value)),
        },
        serde_json::Value::Array(values) => Value {
            kind: Some(prost_types::value::Kind::ListValue(ListValue {
                values: values
                    .into_iter()
                    .map(json_value_to_prost)
                    .collect::<Result<Vec<_>>>()?,
            })),
        },
        serde_json::Value::Object(map) => Value {
            kind: Some(prost_types::value::Kind::StructValue(Struct {
                fields: map
                    .into_iter()
                    .map(|(key, value)| Ok((key, json_value_to_prost(value)?)))
                    .collect::<Result<_>>()?,
            })),
        },
    })
}

fn value_string(value: impl Into<String>) -> Value {
    Value {
        kind: Some(prost_types::value::Kind::StringValue(value.into())),
    }
}

fn value_number(value: f64) -> Value {
    Value {
        kind: Some(prost_types::value::Kind::NumberValue(value)),
    }
}

fn value_bool(value: bool) -> Value {
    Value {
        kind: Some(prost_types::value::Kind::BoolValue(value)),
    }
}

fn value_list(values: Vec<Value>) -> Value {
    Value {
        kind: Some(prost_types::value::Kind::ListValue(ListValue { values })),
    }
}
