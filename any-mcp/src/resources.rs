// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Bounded MCP resource templates and document reads.
//!
//! [`AnytypeResources`] is transport-neutral so the production catalog can
//! delegate its `resources/list`, `resources/templates/list`, and
//! `resources/read` methods without duplicating URI or response validation.
//! Object discovery remains a paginated tool workflow: [`list_resources`](AnytypeResources::list_resources)
//! intentionally returns no object instances.

use std::fmt;

use anytype::{error::AnytypeError, objects::Object};
use chrono::{DateTime, Utc};
use rmcp::model::{
    Annotations, ErrorData, ListResourceTemplatesResult, ListResourcesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate, Role,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{
    domain::{LastModified, ObjectResourceUri, ObjectSummary},
    error::{AnytypeErrorMapping, ToolError, ToolErrorCode},
    object_output::{ObjectOutputError, object_summary},
    runtime::{
        ControlledOperationError, OperationContext, OperationFailureDiagnostic, RuntimeContext,
    },
};

/// Exact RFC 6570 resource template advertised for Anytype documents.
pub const OBJECT_RESOURCE_TEMPLATE: &str = "anytype://spaces/{space_id}/objects/{object_id}";
/// MIME type returned by every Anytype document resource read.
pub const MARKDOWN_MIME_TYPE: &str = "text/markdown";
/// Maximum number of Unicode scalar values returned by a resource read.
pub const MAX_RESOURCE_BODY_CHARS: usize = 100_000;

const RESOURCE_PRIORITY: f32 = 0.5;
const INVALID_URI_MESSAGE: &str = "Invalid Anytype document resource URI.";
const BOUNDED_MESSAGE: &str =
    "The document exceeds the resource limit; use object_get body chunking.";
const UPSTREAM_MESSAGE: &str = "Anytype could not complete the resource read.";

/// A validated document read together with its typed MCP resource descriptor.
///
/// The descriptor carries byte size and annotations supported by `rmcp` 2.2,
/// while [`ReadResourceResult`] carries only the complete markdown body. This
/// lets tool result adapters emit the same metadata-rich resource link without
/// duplicating document properties or content in `_meta`.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectResourceRead {
    descriptor: Resource,
    result: ReadResourceResult,
}

impl ObjectResourceRead {
    /// Borrows the metadata-rich resource descriptor suitable for a resource link.
    #[must_use]
    pub const fn descriptor(&self) -> &Resource {
        &self.descriptor
    }

    /// Borrows the protocol `resources/read` result.
    #[must_use]
    pub const fn result(&self) -> &ReadResourceResult {
        &self.result
    }

    /// Consumes the typed read and returns its protocol result.
    #[must_use]
    pub fn into_result(self) -> ReadResourceResult {
        self.result
    }
}

/// Transport-neutral document resource handler and catalog seam.
#[derive(Debug, Clone)]
pub struct AnytypeResources {
    runtime: RuntimeContext,
}

impl AnytypeResources {
    /// Creates resource handlers backed by the process-long Anytype runtime.
    #[must_use]
    pub const fn new(runtime: RuntimeContext) -> Self {
        Self { runtime }
    }

    /// Returns an empty resource instance page.
    ///
    /// Anytype objects are intentionally not enumerated here. Callers discover
    /// them through the bounded `object_search` tool and then read its URI.
    pub fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
    ) -> Result<ListResourcesResult, ErrorData> {
        reject_cursor(request)?;
        Ok(ListResourcesResult::with_all_items(Vec::new()))
    }

    /// Returns the single exact Anytype document resource template.
    pub fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        reject_cursor(request)?;
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            object_resource_template(),
        ]))
    }

    /// Reads one complete document through the shared runtime controls.
    ///
    /// URI validation happens before I/O. The Anytype object response is read
    /// through the client's configured document-byte ceiling. Conversion,
    /// identity checks, Unicode counting, and metadata validation remain
    /// inside the same cancellation and timeout boundary as the upstream call.
    pub async fn read_document(
        &self,
        request: ReadResourceRequestParams,
        cancellation: &CancellationToken,
    ) -> Result<ObjectResourceRead, ErrorData> {
        let (uri, space_id, object_id) = ObjectResourceUri::parse(&request.uri)
            .map_err(|_| resource_error(ToolError::validation(), INVALID_URI_MESSAGE))?;
        let client = self.runtime.client();
        let response_limit = client.get_config().response_limits.document_bytes;
        let expected_uri = uri.clone();
        let result = self
            .runtime
            .execute_classified(
                OperationContext::new("resource_read"),
                cancellation,
                async move {
                    let object = client
                        .object(space_id.as_str(), object_id.as_str())
                        .response_limit_bytes(response_limit)
                        .get()
                        .await
                        .map_err(ResourceOperationError::Upstream)?;
                    convert_object(object, &expected_uri)
                        .map_err(ResourceOperationError::Conversion)
                },
                ResourceOperationError::diagnostic,
            )
            .await;
        match result {
            Ok(read) => Ok(read),
            Err(error) => Err(controlled_error(error)),
        }
    }

    /// Protocol adapter used by the production `ServerHandler::read_resource` method.
    pub async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        cancellation: &CancellationToken,
    ) -> Result<ReadResourceResult, ErrorData> {
        self.read_document(request, cancellation)
            .await
            .map(ObjectResourceRead::into_result)
    }
}

/// Builds the one typed resource template advertised by `any-mcp`.
#[must_use]
pub fn object_resource_template() -> ResourceTemplate {
    ResourceTemplate::new(OBJECT_RESOURCE_TEMPLATE, "anytype_document")
        .with_title("Anytype document")
        .with_description("A complete bounded Anytype document body")
        .with_mime_type(MARKDOWN_MIME_TYPE)
        .with_annotations(resource_annotations(None))
}

/// Builds a metadata-rich MCP resource link from a validated object summary.
///
/// Search, get, and write catalog adapters can use this function so their
/// links share exactly the URI grammar accepted by [`AnytypeResources`]. Pass
/// the raw UTF-8 body size only when the complete current body is known.
#[must_use]
pub fn object_resource_link(summary: &ObjectSummary, size: Option<u64>) -> Resource {
    let mut resource = Resource::new(summary.resource_uri().as_str(), summary.id().as_str())
        .with_title(summary.name().as_str())
        .with_description("Anytype document body")
        .with_mime_type(MARKDOWN_MIME_TYPE)
        .with_annotations(resource_annotations(summary.last_modified()));
    if let Some(size) = size {
        resource = resource.with_size(size);
    }
    resource
}

fn reject_cursor(request: Option<PaginatedRequestParams>) -> Result<(), ErrorData> {
    if request.and_then(|request| request.cursor).is_some() {
        return Err(ErrorData::invalid_params(
            "Resource template and instance lists do not use cursors.",
            Some(json!({"code":"validation"})),
        ));
    }
    Ok(())
}

fn convert_object(
    object: Object,
    expected_uri: &ObjectResourceUri,
) -> Result<ObjectResourceRead, ResourceConversionError> {
    let summary = object_summary(&object).map_err(ResourceConversionError::Metadata)?;
    if summary.resource_uri() != expected_uri {
        return Err(ResourceConversionError::IdentityMismatch);
    }
    let markdown = object
        .markdown
        .ok_or(ResourceConversionError::MissingBody)?;
    if markdown.chars().count() > MAX_RESOURCE_BODY_CHARS {
        return Err(ResourceConversionError::BodyTooLarge);
    }
    let size = u64::try_from(markdown.len()).map_err(|_| ResourceConversionError::BodyTooLarge)?;
    let uri = expected_uri.as_str().to_owned();
    let descriptor = object_resource_link(&summary, Some(size));
    let contents = ResourceContents::text(markdown, uri).with_mime_type(MARKDOWN_MIME_TYPE);
    Ok(ObjectResourceRead {
        descriptor,
        result: ReadResourceResult::new(vec![contents]),
    })
}

fn resource_annotations(last_modified: Option<&LastModified>) -> Annotations {
    let mut annotations = Annotations::default()
        .with_audience(vec![Role::User, Role::Assistant])
        .with_priority(RESOURCE_PRIORITY);
    if let Some(last_modified) = last_modified {
        // LastModified construction has already performed strict RFC 3339
        // validation. Reparse only to adapt into rmcp's chrono wire type.
        let timestamp = DateTime::parse_from_rfc3339(last_modified.as_str())
            .expect("validated RFC 3339 timestamp")
            .with_timezone(&Utc);
        annotations = annotations.with_timestamp(timestamp);
    }
    annotations
}

#[derive(Debug)]
enum ResourceOperationError {
    Upstream(AnytypeError),
    Conversion(ResourceConversionError),
}

impl ResourceOperationError {
    fn diagnostic(&self) -> OperationFailureDiagnostic {
        match self {
            Self::Upstream(error) => OperationFailureDiagnostic::from_anytype(error),
            Self::Conversion(_) => {
                OperationFailureDiagnostic::classified("conversion_error", "resource_conversion")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceConversionError {
    Metadata(ObjectOutputError),
    IdentityMismatch,
    MissingBody,
    BodyTooLarge,
}

impl fmt::Display for ResourceConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Anytype document resource conversion failed")
    }
}

fn controlled_error(error: ControlledOperationError<ResourceOperationError>) -> ErrorData {
    match error {
        ControlledOperationError::Operation(ResourceOperationError::Upstream(source)) => {
            let tool = match ToolError::from_anytype(&source) {
                AnytypeErrorMapping::Ready(error) => error,
                AnytypeErrorMapping::AmbiguityRequiresCandidates => ToolError::upstream(),
            };
            let message = if tool.code() == ToolErrorCode::BoundedResult {
                BOUNDED_MESSAGE
            } else {
                tool.message()
            };
            resource_error(tool, message)
        }
        ControlledOperationError::Operation(ResourceOperationError::Conversion(
            ResourceConversionError::BodyTooLarge,
        )) => resource_error(ToolError::bounded_result(), BOUNDED_MESSAGE),
        ControlledOperationError::Operation(ResourceOperationError::Conversion(
            ResourceConversionError::Metadata(error),
        )) => {
            let tool = error.tool_error();
            let message = tool.message();
            resource_error(tool, message)
        }
        ControlledOperationError::Operation(ResourceOperationError::Conversion(
            ResourceConversionError::IdentityMismatch | ResourceConversionError::MissingBody,
        ))
        | ControlledOperationError::Cancelled
        | ControlledOperationError::TimedOut
        | ControlledOperationError::ShuttingDown => {
            resource_error(ToolError::upstream(), UPSTREAM_MESSAGE)
        }
    }
}

fn resource_error(error: ToolError, message: &'static str) -> ErrorData {
    let code = error.code();
    let data = Some(json!({
        "code": code,
        "message": message,
    }));
    match code {
        ToolErrorCode::Validation => ErrorData::invalid_params(message, data),
        ToolErrorCode::NotFound => ErrorData::resource_not_found(message, data),
        ToolErrorCode::Authentication
        | ToolErrorCode::Ambiguous
        | ToolErrorCode::Conflict
        | ToolErrorCode::BoundedResult
        | ToolErrorCode::Upstream => ErrorData::internal_error(message, data),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anytype::{
        objects::{DataModel, ObjectLayout},
        prelude::{AnytypeClient, ClientConfig, HttpCredentials, ResponseLimits},
        properties::{PropertyValue, PropertyWithValue},
        types::Type,
    };
    use rmcp::model::ErrorCode;
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    use super::*;
    use crate::{
        domain::{DisplayName, ObjectId, ObjectSummary, SpaceId, TypeKey},
        runtime::StartupStatus,
    };

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const RESOURCE_URI: &str = "anytype://spaces/bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7/objects/bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const LAST_MODIFIED: &str = "2026-07-20T10:00:00Z";

    struct FixtureReply {
        status: &'static str,
        body: String,
        delay: Duration,
    }

    impl FixtureReply {
        fn json(value: Value) -> Self {
            Self {
                status: "200 OK",
                body: value.to_string(),
                delay: Duration::ZERO,
            }
        }

        fn error(status: &'static str, body: &str) -> Self {
            Self {
                status,
                body: body.to_owned(),
                delay: Duration::ZERO,
            }
        }

        fn delayed(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }
    }

    async fn fixture(replies: Vec<FixtureReply>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind resource fixture");
        let address = listener.local_addr().expect("resource fixture address");
        let server = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(replies.len());
            for reply in replies {
                let (mut socket, _) = listener.accept().await.expect("accept resource request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = socket.read(&mut buffer).await.expect("read request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8(request).expect("request headers are utf-8"));
                tokio::time::sleep(reply.delay).await;
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.status,
                    reply.body.len(),
                    reply.body,
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
            requests
        });
        (format!("http://{address}"), server)
    }

    async fn no_request_fixture() -> (String, JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind no-request fixture");
        let address = listener.local_addr().expect("no-request address");
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        });
        (format!("http://{address}"), server)
    }

    fn runtime(base_url: String, timeout: Duration) -> RuntimeContext {
        runtime_with_limits(base_url, timeout, ResponseLimits::default())
    }

    fn runtime_with_limits(
        base_url: String,
        timeout: Duration,
        response_limits: ResponseLimits,
    ) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some(base_url),
            keystore: Some("env".to_owned()),
            keystore_service: Some("resource-test".to_owned()),
            app_name: "resource-test".to_owned(),
            response_limits,
            ..ClientConfig::default()
        })
        .expect("resource fixture client");
        client.set_api_key(HttpCredentials::new("fixture-secret-token"));
        RuntimeContext::from_parts(
            client,
            1,
            timeout,
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        )
    }

    fn object(body: Option<String>, space_id: &str, object_id: &str) -> Object {
        Object {
            archived: false,
            icon: None,
            id: object_id.to_owned(),
            layout: ObjectLayout::Basic,
            markdown: body,
            name: Some("Roadmap".to_owned()),
            object: DataModel::Object,
            properties: vec![PropertyWithValue {
                id: "last-modified-property".to_owned(),
                key: "last_modified_date".to_owned(),
                name: "Last modified".to_owned(),
                value: PropertyValue::Date {
                    date: LAST_MODIFIED.to_owned(),
                },
            }],
            snippet: Some("must not enter resource metadata".to_owned()),
            space_id: space_id.to_owned(),
            r#type: Some(Type {
                archived: false,
                icon: None,
                id: "type-1".to_owned(),
                key: "page".to_owned(),
                layout: ObjectLayout::Basic,
                name: Some("Page".to_owned()),
                plural_name: None,
                properties: Vec::new(),
            }),
        }
    }

    fn object_response(body: Option<String>, space_id: &str, object_id: &str) -> Value {
        json!({"object": object(body, space_id, object_id)})
    }

    fn request(uri: &str) -> ReadResourceRequestParams {
        ReadResourceRequestParams::new(uri)
    }

    fn error_code(error: &ErrorData) -> &str {
        error
            .data
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
            .expect("resource error code")
    }

    fn text(result: &ReadResourceResult) -> &str {
        match result.contents.first().expect("one resource content") {
            ResourceContents::TextResourceContents { text, .. } => text,
            ResourceContents::BlobResourceContents { .. } => panic!("unexpected blob resource"),
            _ => panic!("unexpected future resource content variant"),
        }
    }

    #[test]
    fn exact_template_is_typed_and_resources_list_never_enumerates_objects() {
        let handlers = AnytypeResources::new(runtime(
            "http://127.0.0.1:1".to_owned(),
            Duration::from_secs(1),
        ));
        let templates = handlers
            .list_resource_templates(None)
            .expect("template list");
        assert_eq!(templates.resource_templates.len(), 1);
        let template = &templates.resource_templates[0];
        assert_eq!(template.uri_template, OBJECT_RESOURCE_TEMPLATE);
        assert_eq!(template.name, "anytype_document");
        assert_eq!(template.mime_type.as_deref(), Some(MARKDOWN_MIME_TYPE));
        let annotations = template.annotations.as_ref().expect("template annotations");
        assert_eq!(annotations.priority, Some(RESOURCE_PRIORITY));
        assert_eq!(
            annotations.audience.as_deref(),
            Some([Role::User, Role::Assistant].as_slice())
        );
        assert!(annotations.last_modified.is_none());

        let resources = handlers.list_resources(None).expect("resource list");
        assert!(resources.resources.is_empty());
        assert!(resources.next_cursor.is_none());
        assert!(resources.meta.is_none());

        let mut paginated = PaginatedRequestParams::default();
        paginated.cursor = Some("not-used".to_owned());
        let cursor = Some(paginated);
        assert!(handlers.list_resources(cursor.clone()).is_err());
        assert!(handlers.list_resource_templates(cursor).is_err());
    }

    #[test]
    fn canonical_search_get_and_write_uri_type_round_trips_strictly() {
        let summary = ObjectSummary::new(
            ObjectId::new(OBJECT_ID).unwrap(),
            DisplayName::new("Roadmap").unwrap(),
            TypeKey::new("page").unwrap(),
            SpaceId::new(SPACE_ID).unwrap(),
            None,
        );
        let wire = serde_json::to_value(&summary).expect("summary wire value");
        assert_eq!(wire["resource_uri"], RESOURCE_URI);
        let search_link = object_resource_link(&summary, None);
        assert_eq!(search_link.uri, RESOURCE_URI);
        assert_eq!(search_link.size, None);
        let (uri, space, object) =
            ObjectResourceUri::parse(search_link.uri.as_str()).expect("canonical URI parses");
        assert_eq!(uri.as_str(), RESOURCE_URI);
        assert_eq!(space.as_str(), SPACE_ID);
        assert_eq!(object.as_str(), OBJECT_ID);
        assert_eq!(
            serde_json::from_value::<ObjectSummary>(wire)
                .unwrap()
                .resource_uri()
                .as_str(),
            RESOURCE_URI
        );
    }

    #[tokio::test]
    async fn exact_get_returns_complete_100k_unicode_body_and_bounded_metadata() {
        let body = "🦀".repeat(MAX_RESOURCE_BODY_CHARS);
        let (base_url, server) = fixture(vec![FixtureReply::json(object_response(
            Some(body.clone()),
            SPACE_ID,
            OBJECT_ID,
        ))])
        .await;
        let handlers = AnytypeResources::new(runtime(base_url, Duration::from_secs(3)));
        let read = handlers
            .read_document(request(RESOURCE_URI), &CancellationToken::new())
            .await
            .expect("resource read at character boundary");

        assert_eq!(text(read.result()), body);
        assert_eq!(read.result().contents.len(), 1);
        assert!(read.result().meta.is_none());
        let descriptor = read.descriptor();
        assert_eq!(descriptor.uri, RESOURCE_URI);
        assert_eq!(descriptor.name, OBJECT_ID);
        assert_eq!(descriptor.title.as_deref(), Some("Roadmap"));
        assert_eq!(descriptor.mime_type.as_deref(), Some(MARKDOWN_MIME_TYPE));
        assert_eq!(descriptor.size, Some((MAX_RESOURCE_BODY_CHARS * 4) as u64));
        assert!(descriptor.meta.is_none());
        let annotations = descriptor
            .annotations
            .as_ref()
            .expect("resource annotations");
        assert_eq!(annotations.priority, Some(RESOURCE_PRIORITY));
        assert_eq!(
            annotations
                .last_modified
                .expect("last modified")
                .to_rfc3339(),
            "2026-07-20T10:00:00+00:00"
        );
        let descriptor_wire = serde_json::to_value(descriptor).expect("descriptor wire");
        assert!(descriptor_wire.get("properties").is_none());
        assert!(descriptor_wire.get("markdown").is_none());
        assert!(descriptor_wire.get("snippet").is_none());

        let requests = server.await.expect("resource fixture");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/objects/{OBJECT_ID} HTTP/1.1\r\n"
        )));
    }

    #[tokio::test]
    async fn body_above_100k_chars_fails_without_silent_truncation() {
        let body = "é".repeat(MAX_RESOURCE_BODY_CHARS + 1);
        let (base_url, server) = fixture(vec![FixtureReply::json(object_response(
            Some(body),
            SPACE_ID,
            OBJECT_ID,
        ))])
        .await;
        let handlers = AnytypeResources::new(runtime(base_url, Duration::from_secs(3)));
        let error = handlers
            .read_resource(request(RESOURCE_URI), &CancellationToken::new())
            .await
            .expect_err("oversized resource body");
        assert_eq!(error_code(&error), "bounded_result");
        assert_eq!(error.message, BOUNDED_MESSAGE);
        assert!(error.message.contains("object_get"));
        assert!(!serde_json::to_string(&error).unwrap().contains('é'));
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn document_response_byte_ceiling_is_exact_before_conversion() {
        let body = "é🦀".repeat(40);
        let response = object_response(Some(body.clone()), SPACE_ID, OBJECT_ID);
        let response_bytes = response.to_string().len() as u64;

        let (base_url, server) = fixture(vec![FixtureReply::json(response.clone())]).await;
        let handlers = AnytypeResources::new(runtime_with_limits(
            base_url,
            Duration::from_secs(3),
            ResponseLimits {
                document_bytes: response_bytes,
                ..ResponseLimits::default()
            },
        ));
        let exact = handlers
            .read_resource(request(RESOURCE_URI), &CancellationToken::new())
            .await
            .expect("response exactly at the byte ceiling");
        assert_eq!(text(&exact), body);
        assert_eq!(server.await.unwrap().len(), 1);

        let (base_url, server) = fixture(vec![FixtureReply::json(response)]).await;
        let handlers = AnytypeResources::new(runtime_with_limits(
            base_url,
            Duration::from_secs(3),
            ResponseLimits {
                document_bytes: response_bytes - 1,
                ..ResponseLimits::default()
            },
        ));
        let error = handlers
            .read_resource(request(RESOURCE_URI), &CancellationToken::new())
            .await
            .expect_err("response one byte above the ceiling");
        assert_eq!(error_code(&error), "bounded_result");
        assert_eq!(error.message, BOUNDED_MESSAGE);
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn malformed_uris_are_rejected_before_any_io() {
        let invalid = [
            "http://spaces/space-1/objects/object-1",
            "anytype://other/space-1/objects/object-1",
            "anytype://user@spaces/space-1/objects/object-1",
            "anytype://spaces/space-1/objects/object-1?query=1",
            "anytype://spaces/space-1/objects/object-1#fragment",
            "anytype://spaces/space-1/objects/object-1/extra",
            "anytype://spaces/./objects/object-1",
            "anytype://spaces/../objects/object-1",
            "anytype://spaces/space-1/objects/..",
            "anytype://spaces/space%2D1/objects/object-1",
            "anytype://spaces/space-1/objects/object%2D1",
            "anytype://spaces//objects/object-1",
            "anytype://spaces/space-1/objects/",
        ];
        for uri in invalid {
            let (base_url, no_request) = no_request_fixture().await;
            let handlers = AnytypeResources::new(runtime(base_url, Duration::from_secs(1)));
            let error = handlers
                .read_resource(request(uri), &CancellationToken::new())
                .await
                .expect_err("invalid URI");
            assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
            assert_eq!(error_code(&error), "validation");
            assert!(no_request.await.expect("no-request fixture"));
        }
    }

    #[tokio::test]
    async fn returned_object_and_space_identity_must_match_the_uri() {
        for (space_id, object_id) in [("other-space", OBJECT_ID), (SPACE_ID, "other-object")] {
            let (base_url, server) = fixture(vec![FixtureReply::json(object_response(
                Some("body".to_owned()),
                space_id,
                object_id,
            ))])
            .await;
            let handlers = AnytypeResources::new(runtime(base_url, Duration::from_secs(1)));
            let error = handlers
                .read_resource(request(RESOURCE_URI), &CancellationToken::new())
                .await
                .expect_err("identity mismatch");
            assert_eq!(error_code(&error), "upstream");
            assert_eq!(error.message, UPSTREAM_MESSAGE);
            assert_eq!(server.await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn upstream_error_classes_are_stable_and_secret_safe() {
        let replies = vec![
            FixtureReply::error("404 Not Found", "not-found-secret-body"),
            FixtureReply::error("401 Unauthorized", "auth-secret-body"),
            FixtureReply::error("500 Internal Server Error", "upstream-secret-body"),
        ];
        let (base_url, server) = fixture(replies).await;
        let endpoint = base_url.clone();
        let handlers = AnytypeResources::new(runtime(base_url, Duration::from_secs(1)));
        for expected in ["not_found", "authentication", "upstream"] {
            let error = handlers
                .read_resource(request(RESOURCE_URI), &CancellationToken::new())
                .await
                .expect_err("upstream error");
            assert_eq!(error_code(&error), expected);
            let wire = serde_json::to_string(&error).unwrap();
            assert!(!wire.contains("secret-body"));
            assert!(!wire.contains("fixture-secret-token"));
            assert!(!wire.contains(&endpoint));
        }
        assert_eq!(server.await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn cancellation_aborts_a_delayed_resource_read() {
        let (base_url, server) = fixture(vec![
            FixtureReply::json(object_response(
                Some("body".to_owned()),
                SPACE_ID,
                OBJECT_ID,
            ))
            .delayed(Duration::from_millis(250)),
        ])
        .await;
        let handlers = AnytypeResources::new(runtime(base_url, Duration::from_secs(2)));
        let cancellation = CancellationToken::new();
        let trigger = cancellation.clone();
        let cancel_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            trigger.cancel();
        });
        let error = handlers
            .read_resource(request(RESOURCE_URI), &cancellation)
            .await
            .expect_err("cancelled read");
        cancel_task.await.unwrap();
        assert_eq!(error_code(&error), "upstream");
        assert_eq!(error.message, UPSTREAM_MESSAGE);
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn missing_body_and_invalid_last_modified_fail_closed() {
        let mut invalid_timestamp = object(Some("body".to_owned()), SPACE_ID, OBJECT_ID);
        invalid_timestamp.properties[0].value = PropertyValue::Date {
            date: "not-a-timestamp".to_owned(),
        };
        let replies = vec![
            FixtureReply::json(json!({"object": object(None, SPACE_ID, OBJECT_ID)})),
            FixtureReply::json(json!({"object": invalid_timestamp})),
        ];
        let (base_url, server) = fixture(replies).await;
        let handlers = AnytypeResources::new(runtime(base_url, Duration::from_secs(1)));
        for expected_message in [UPSTREAM_MESSAGE, ToolError::upstream().message()] {
            let error = handlers
                .read_resource(request(RESOURCE_URI), &CancellationToken::new())
                .await
                .expect_err("invalid resource document");
            assert_eq!(error_code(&error), "upstream");
            assert_eq!(error.message, expected_message);
        }
        assert_eq!(server.await.unwrap().len(), 2);
    }
}
