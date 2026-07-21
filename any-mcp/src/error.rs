// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Stable, bounded, caller-visible MCP tool execution errors.

use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::domain::{BoundedText, DomainValueError, EntityId};

/// Maximum number of ambiguity candidates returned to a caller.
pub const MAX_ERROR_CANDIDATES: usize = 10;
/// Maximum number of characters in an ambiguity candidate's display name.
pub const MAX_CANDIDATE_NAME_CHARS: usize = 256;

/// Error returned when an ambiguity response has no candidates to present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmbiguityCandidatesError;

impl std::fmt::Display for AmbiguityCandidatesError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("at least one ambiguity candidate is required")
    }
}

impl std::error::Error for AmbiguityCandidatesError {}

/// Stable machine-readable codes for tool execution failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorCode {
    /// The configured Anytype credentials were rejected or unavailable.
    Authentication,
    /// A well-formed tool call failed domain-level input validation.
    Validation,
    /// A name or key resolved to more than one Anytype entity.
    Ambiguous,
    /// The requested entity does not exist or is not visible.
    NotFound,
    /// An optimistic-concurrency or idempotency precondition failed.
    Conflict,
    /// The requested result exceeds the documented bounded response size.
    BoundedResult,
    /// Anytype failed the request for a reason safe details cannot expose.
    Upstream,
}

impl ToolErrorCode {
    const fn corrective_message(self) -> &'static str {
        match self {
            Self::Authentication => {
                "Anytype authentication failed. Verify the configured credentials and retry."
            }
            Self::Validation => "Input validation failed. Correct the supplied fields and retry.",
            Self::Ambiguous => {
                "The reference is ambiguous. Retry with one of the candidate identifiers."
            }
            Self::NotFound => {
                "The requested Anytype entity was not found. Verify its identifier and space."
            }
            Self::Conflict => {
                "The object changed or a request precondition failed. Read it again before retrying."
            }
            Self::BoundedResult => {
                "The result exceeds this workflow's limit. Retry with a paginated or chunked read."
            }
            Self::Upstream => {
                "Anytype could not complete the request. Retry later or inspect redacted server diagnostics."
            }
        }
    }
}

/// One bounded candidate returned for an ambiguous Anytype reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorCandidate {
    /// Stable identifier that the caller can use to disambiguate a retry.
    pub id: EntityId,
    /// Bounded display name that helps the caller choose a candidate.
    pub name: BoundedText<MAX_CANDIDATE_NAME_CHARS>,
}

impl TryFrom<&anytype::resolve::ResolveCandidate> for ErrorCandidate {
    type Error = DomainValueError;

    fn try_from(candidate: &anytype::resolve::ResolveCandidate) -> Result<Self, Self::Error> {
        Ok(Self {
            id: EntityId::new(candidate.id())?,
            name: BoundedText::new(candidate.name())?,
        })
    }
}

/// Secret-safe error body returned in MCP `structuredContent`.
///
/// Messages are selected only from fixed corrective text. Upstream response
/// bodies, credential values, and arbitrary exception strings cannot enter
/// this wire model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolError {
    /// Stable machine-readable failure category.
    code: ToolErrorCode,
    /// Short, fixed corrective message safe to show to a caller.
    #[schemars(length(max = 160))]
    message: &'static str,
    /// Bounded alternatives supplied only for ambiguity errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = MAX_ERROR_CANDIDATES))]
    candidates: Option<Vec<ErrorCandidate>>,
}

/// Classification produced when mapping an `anytype-api` failure.
///
/// Ambiguity without valid candidates is deliberately not a completed
/// [`ToolError`]. Resolvers normally provide candidates directly; this state
/// prevents malformed or manually constructed API errors from reaching MCP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnytypeErrorMapping {
    /// A complete, secret-safe error that can be returned immediately.
    Ready(ToolError),
    /// The handler cannot expose this ambiguity until it has valid candidates.
    AmbiguityRequiresCandidates,
}

/// Returns whether an Anytype failure definitively rejected a mutation.
///
/// `false` means that a write may have reached Anytype and the handler must
/// return [`ToolError::mutation_indeterminate`] rather than ordinary retry
/// guidance. The classifier is intentionally conservative and inspects only
/// the error variant, numeric HTTP status, or `anytype-api`'s typed
/// authentication seam. It never formats or copies URLs, credentials,
/// response bodies, or diagnostic messages.
#[must_use]
pub fn mutation_rejection_is_definitive(error: &anytype::error::AnytypeError) -> bool {
    use anytype::error::AnytypeError;

    match error {
        // These failures are produced before dispatch or prove that Anytype
        // rejected the operation without applying it.
        AnytypeError::Auth { .. }
        | AnytypeError::Unauthorized
        | AnytypeError::Forbidden
        | AnytypeError::Serialization { .. }
        | AnytypeError::NotFound { .. }
        | AnytypeError::Ambiguous { .. }
        | AnytypeError::ResolutionLimitExceeded { .. }
        | AnytypeError::RateLimitExceeded { .. }
        | AnytypeError::Validation { .. }
        | AnytypeError::NoKeyStore
        | AnytypeError::KeyStore { .. }
        | AnytypeError::GrpcUnavailable { .. }
        | AnytypeError::CacheDisabled => true,
        AnytypeError::ApiError { code, .. } => matches!(
            code,
            400 | 401
                | 403
                | 404
                | 405
                | 409
                | 410
                | 411
                | 412
                | 413
                | 414
                | 415
                | 416
                | 417
                | 422
                | 429
        ),

        // A transport failure, partial/oversized/malformed response, exhausted
        // retry sequence, verification timeout, or unclassified failure does
        // not prove whether a dispatched mutation took effect.
        AnytypeError::Grpc { .. } => error.is_authentication(),
        AnytypeError::Http { .. }
        | AnytypeError::ResponseTooLarge { .. }
        | AnytypeError::FileHeaderEvidenceTooLarge { .. }
        | AnytypeError::InvalidFileResponseHeader { .. }
        | AnytypeError::ChatSseEventTooLarge { .. }
        | AnytypeError::ChatSseTransport { .. }
        | AnytypeError::TooManyRetries { .. }
        | AnytypeError::Deserialization { .. }
        | AnytypeError::VerifyTimeout { .. }
        | AnytypeError::Other { .. } => false,
    }
}

impl ToolError {
    const fn from_code(code: ToolErrorCode) -> Self {
        Self {
            code,
            message: code.corrective_message(),
            candidates: None,
        }
    }

    /// Creates an authentication error with fixed corrective text.
    #[must_use]
    pub const fn authentication() -> Self {
        Self::from_code(ToolErrorCode::Authentication)
    }

    /// Creates a validation error with fixed corrective text.
    #[must_use]
    pub const fn validation() -> Self {
        Self::from_code(ToolErrorCode::Validation)
    }

    /// Creates the fixed error returned when a mutation reaches a read-only
    /// handler seam.
    #[must_use]
    pub const fn read_only() -> Self {
        Self::validation_message(
            "This Anytype server is read-only. Mutating workflows are disabled.",
        )
    }

    /// Creates a validation error from fixed, secret-free server text.
    pub(crate) const fn validation_message(message: &'static str) -> Self {
        Self {
            code: ToolErrorCode::Validation,
            message,
            candidates: None,
        }
    }

    /// Creates a not-found error with fixed corrective text.
    #[must_use]
    pub const fn not_found() -> Self {
        Self::from_code(ToolErrorCode::NotFound)
    }

    /// Creates a conflict error with fixed corrective text.
    #[must_use]
    pub const fn conflict() -> Self {
        Self::from_code(ToolErrorCode::Conflict)
    }

    /// Creates the fixed conflict returned when a controlled failure occurs
    /// after a write may have reached Anytype.
    ///
    /// This deliberately does not give generic retry advice: callers must
    /// reread state first so a successful but unobserved mutation is not
    /// applied twice.
    #[must_use]
    pub const fn mutation_indeterminate() -> Self {
        Self {
            code: ToolErrorCode::Conflict,
            message: "The mutation may have applied. Reread the object before retrying to avoid applying it twice.",
            candidates: None,
        }
    }

    /// Creates a bounded-result error with fixed corrective text.
    #[must_use]
    pub const fn bounded_result() -> Self {
        Self::from_code(ToolErrorCode::BoundedResult)
    }

    /// Creates a redacted upstream error with fixed corrective text.
    #[must_use]
    pub const fn upstream() -> Self {
        Self::from_code(ToolErrorCode::Upstream)
    }

    /// Creates an ambiguity error, requiring at least one candidate and
    /// capping the returned list at [`MAX_ERROR_CANDIDATES`].
    pub fn ambiguous(
        candidates: impl IntoIterator<Item = ErrorCandidate>,
    ) -> Result<Self, AmbiguityCandidatesError> {
        let candidates: Vec<_> = candidates.into_iter().take(MAX_ERROR_CANDIDATES).collect();
        if candidates.is_empty() {
            return Err(AmbiguityCandidatesError);
        }
        Ok(Self {
            code: ToolErrorCode::Ambiguous,
            message: ToolErrorCode::Ambiguous.corrective_message(),
            candidates: Some(candidates),
        })
    }

    /// Maps an `anytype-api` failure to a stable category without copying its
    /// URL, response body, credential text, or diagnostic message. Ambiguity
    /// becomes a ready error only when the resolver supplied candidates that
    /// satisfy the MCP identifier and name bounds.
    #[must_use]
    pub fn from_anytype(error: &anytype::error::AnytypeError) -> AnytypeErrorMapping {
        use anytype::error::AnytypeError;

        if error.is_authentication() {
            return AnytypeErrorMapping::Ready(Self::authentication());
        }

        let code = match error {
            AnytypeError::Auth { .. }
            | AnytypeError::Unauthorized
            | AnytypeError::Forbidden
            | AnytypeError::NoKeyStore
            | AnytypeError::KeyStore { .. }
            | AnytypeError::GrpcUnavailable { .. } => ToolErrorCode::Authentication,
            AnytypeError::Validation { .. } => ToolErrorCode::Validation,
            AnytypeError::Ambiguous { candidates, .. } => {
                let candidates = candidates
                    .iter()
                    .map(ErrorCandidate::try_from)
                    .filter_map(Result::ok)
                    .take(MAX_ERROR_CANDIDATES);
                return match Self::ambiguous(candidates) {
                    Ok(error) => AnytypeErrorMapping::Ready(error),
                    Err(_) => AnytypeErrorMapping::AmbiguityRequiresCandidates,
                };
            }
            AnytypeError::ResolutionLimitExceeded { .. }
            | AnytypeError::ResponseTooLarge { .. }
            | AnytypeError::FileHeaderEvidenceTooLarge { .. }
            | AnytypeError::ChatSseEventTooLarge { .. } => ToolErrorCode::BoundedResult,
            AnytypeError::NotFound { .. } => ToolErrorCode::NotFound,
            AnytypeError::ApiError {
                code: 400 | 422, ..
            } => ToolErrorCode::Validation,
            AnytypeError::ApiError {
                code: 401 | 403, ..
            } => ToolErrorCode::Authentication,
            AnytypeError::ApiError { code: 404, .. } => ToolErrorCode::NotFound,
            AnytypeError::ApiError {
                code: 409 | 412, ..
            } => ToolErrorCode::Conflict,
            AnytypeError::Http { .. }
            | AnytypeError::InvalidFileResponseHeader { .. }
            | AnytypeError::ChatSseTransport { .. }
            | AnytypeError::ApiError { .. }
            | AnytypeError::TooManyRetries { .. }
            | AnytypeError::Deserialization { .. }
            | AnytypeError::Serialization { .. }
            | AnytypeError::RateLimitExceeded { .. }
            | AnytypeError::Grpc { .. }
            | AnytypeError::CacheDisabled
            | AnytypeError::VerifyTimeout { .. }
            | AnytypeError::Other { .. } => ToolErrorCode::Upstream,
        };
        AnytypeErrorMapping::Ready(Self::from_code(code))
    }

    /// Returns the stable machine-readable failure code.
    #[must_use]
    pub const fn code(&self) -> ToolErrorCode {
        self.code
    }

    /// Returns the caller-visible corrective message.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// Returns bounded ambiguity candidates, if present.
    #[must_use]
    pub fn candidates(&self) -> &[ErrorCandidate] {
        self.candidates.as_deref().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::schema::output_schema;

    fn candidate(index: usize) -> ErrorCandidate {
        ErrorCandidate {
            id: EntityId::new(format!("id-{index}")).unwrap(),
            name: BoundedText::new(format!("Candidate {index}")).unwrap(),
        }
    }

    fn api_error(code: u16) -> anytype::error::AnytypeError {
        anytype::error::AnytypeError::ApiError {
            code,
            method: "SECRET_METHOD_TOKEN".to_owned(),
            url: "http://localhost/private?token=SECRET_URL_TOKEN".to_owned(),
            message: "SECRET_RESPONSE_BODY".to_owned(),
        }
    }

    fn assert_anytype_mapping(
        error: &anytype::error::AnytypeError,
        expected: Option<ToolErrorCode>,
    ) {
        match (ToolError::from_anytype(error), expected) {
            (AnytypeErrorMapping::Ready(error), Some(expected)) => {
                assert_eq!(error.code(), expected);
                let encoded = serde_json::to_string(&error).expect("serialize mapped error");
                assert!(!encoded.contains("SECRET"));
                assert!(!encoded.contains("localhost"));
            }
            (AnytypeErrorMapping::AmbiguityRequiresCandidates, None) => {}
            (actual, expected) => panic!("unexpected mapping {actual:?}, expected {expected:?}"),
        }
    }

    #[test]
    fn mutation_http_rejection_allowlist_is_conservative_at_boundaries() {
        let cases = [
            (399, false),
            (400, true),
            (401, true),
            (402, false),
            (403, true),
            (404, true),
            (405, true),
            (406, false),
            (407, false),
            (408, false),
            (409, true),
            (410, true),
            (411, true),
            (412, true),
            (413, true),
            (414, true),
            (415, true),
            (416, true),
            (417, true),
            (418, false),
            (421, false),
            (422, true),
            (423, false),
            (425, false),
            (428, false),
            (429, true),
            (430, false),
            (499, false),
            (500, false),
            (599, false),
            (600, false),
        ];

        for (status, expected) in cases {
            let source = api_error(status);
            assert_eq!(
                mutation_rejection_is_definitive(&source),
                expected,
                "unexpected mutation classification for HTTP {status}"
            );
            let mapped = match status {
                400 | 422 => ToolErrorCode::Validation,
                401 | 403 => ToolErrorCode::Authentication,
                404 => ToolErrorCode::NotFound,
                409 | 412 => ToolErrorCode::Conflict,
                _ => ToolErrorCode::Upstream,
            };
            assert_anytype_mapping(&source, Some(mapped));
        }
    }

    #[test]
    fn anytype_classifiers_cover_every_directly_constructible_error_variant() {
        use anytype::error::{AnytypeError, KeyStoreError};

        let definitive = vec![
            (
                AnytypeError::Auth {
                    message: "SECRET_AUTH_TOKEN".to_owned(),
                },
                Some(ToolErrorCode::Authentication),
            ),
            (
                AnytypeError::Unauthorized,
                Some(ToolErrorCode::Authentication),
            ),
            (AnytypeError::Forbidden, Some(ToolErrorCode::Authentication)),
            (
                AnytypeError::Serialization {
                    source: serde_json::from_str::<u8>("not-json").unwrap_err(),
                },
                Some(ToolErrorCode::Upstream),
            ),
            (
                AnytypeError::NotFound {
                    obj_type: "SECRET_TYPE".to_owned(),
                    key: "SECRET_KEY".to_owned(),
                },
                Some(ToolErrorCode::NotFound),
            ),
            (
                AnytypeError::Ambiguous {
                    obj_type: "SECRET_TYPE".to_owned(),
                    key: "SECRET_KEY".to_owned(),
                    candidates: Vec::new(),
                },
                None,
            ),
            (
                AnytypeError::ResolutionLimitExceeded {
                    obj_type: "SECRET_TYPE".to_owned(),
                    key: "SECRET_KEY".to_owned(),
                    limit: 1,
                },
                Some(ToolErrorCode::BoundedResult),
            ),
            (
                AnytypeError::RateLimitExceeded {
                    header: "SECRET_RATE_HEADER".to_owned(),
                    duration: Duration::from_secs(1),
                },
                Some(ToolErrorCode::Upstream),
            ),
            (
                AnytypeError::Validation {
                    message: "SECRET_VALIDATION".to_owned(),
                },
                Some(ToolErrorCode::Validation),
            ),
            (
                AnytypeError::NoKeyStore,
                Some(ToolErrorCode::Authentication),
            ),
            (
                AnytypeError::KeyStore {
                    source: KeyStoreError::Config {
                        message: "SECRET_KEYSTORE".to_owned(),
                    },
                },
                Some(ToolErrorCode::Authentication),
            ),
            (
                AnytypeError::GrpcUnavailable {
                    message: "SECRET_GRPC_CONFIG".to_owned(),
                },
                Some(ToolErrorCode::Authentication),
            ),
            (AnytypeError::CacheDisabled, Some(ToolErrorCode::Upstream)),
        ];
        for (error, mapped) in &definitive {
            assert!(
                mutation_rejection_is_definitive(error),
                "definitive variant was classified as indeterminate"
            );
            assert_anytype_mapping(error, *mapped);
        }

        let indeterminate = vec![
            (
                AnytypeError::ResponseTooLarge {
                    limit: 1,
                    declared: Some(2),
                },
                ToolErrorCode::BoundedResult,
            ),
            (
                AnytypeError::FileHeaderEvidenceTooLarge {
                    limit: 4_096,
                    status: 429,
                },
                ToolErrorCode::BoundedResult,
            ),
            (
                AnytypeError::InvalidFileResponseHeader {
                    status: 206,
                    header: "content-range",
                    issue: "malformed",
                },
                ToolErrorCode::Upstream,
            ),
            (
                AnytypeError::TooManyRetries { n: 3 },
                ToolErrorCode::Upstream,
            ),
            (
                AnytypeError::Deserialization {
                    source: serde_json::from_str::<u8>("not-json").unwrap_err(),
                },
                ToolErrorCode::Upstream,
            ),
            (
                AnytypeError::VerifyTimeout {
                    obj_type: "SECRET_TYPE".to_owned(),
                    key: "SECRET_KEY".to_owned(),
                    attempts: 3,
                    timeout: Duration::from_secs(1),
                    last_error: Some("SECRET_LAST_ERROR".to_owned()),
                },
                ToolErrorCode::Upstream,
            ),
            (
                AnytypeError::Other {
                    message: "SECRET_OTHER".to_owned(),
                },
                ToolErrorCode::Upstream,
            ),
        ];
        for (error, mapped) in &indeterminate {
            assert!(
                !mutation_rejection_is_definitive(error),
                "ambiguous variant was classified as definitive"
            );
            assert_anytype_mapping(error, Some(*mapped));
        }
    }

    #[test]
    fn typed_view_authentication_uses_secret_safe_mcp_guidance() {
        let source = anytype::test_util::view_authentication_error_fixture();

        assert!(source.is_authentication());
        assert!(mutation_rejection_is_definitive(&source));
        assert_anytype_mapping(&source, Some(ToolErrorCode::Authentication));

        let wire_error = match ToolError::from_anytype(&source) {
            AnytypeErrorMapping::Ready(error) => error,
            AnytypeErrorMapping::AmbiguityRequiresCandidates => {
                panic!("authentication never requires candidates")
            }
        };
        let encoded = serde_json::to_string(&wire_error).expect("serialize wire error");
        assert!(!encoded.contains("SECRET_VIEW_TOKEN"));
    }

    #[tokio::test]
    async fn opaque_http_and_grpc_transport_variants_are_indeterminate() {
        use anytype::prelude::{
            AnytypeClient, AnytypeError, ClientConfig, GrpcCredentials, HttpCredentials,
        };
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind disconnect fixture");
        let address = listener.local_addr().expect("disconnect address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept HTTP request");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.expect("read HTTP request");
            // Drop without returning an HTTP status.
        });
        let mut http_config = ClientConfig::default().app_name("mutation-http-classifier");
        http_config.base_url = Some(format!("http://{address}"));
        http_config.keystore = Some("env".to_owned());
        http_config.disable_cache = true;
        let http_client = AnytypeClient::with_config(http_config).expect("HTTP fixture client");
        http_client.set_api_key(HttpCredentials::new("SECRET_HTTP_TOKEN"));
        let http_error = http_client
            .spaces()
            .limit(1)
            .list()
            .await
            .expect_err("disconnect must produce an HTTP error");
        server.await.expect("disconnect fixture task");
        assert!(matches!(http_error, AnytypeError::Http { .. }));
        assert!(!mutation_rejection_is_definitive(&http_error));
        assert_anytype_mapping(&http_error, Some(ToolErrorCode::Upstream));

        let mut grpc_config = ClientConfig::default()
            .app_name("mutation-grpc-classifier")
            .grpc_endpoint("not a valid SECRET_GRPC_ENDPOINT".to_owned());
        grpc_config.keystore = Some("env".to_owned());
        let grpc_client = AnytypeClient::with_config(grpc_config).expect("gRPC fixture client");
        grpc_client
            .get_key_store()
            .update_grpc_credentials(&GrpcCredentials::from_token("SECRET_GRPC_TOKEN"))
            .expect("set fixture gRPC credentials");
        let grpc_error = grpc_client
            .grpc_client()
            .await
            .expect_err("invalid endpoint must produce a gRPC error");
        assert!(matches!(grpc_error, AnytypeError::Grpc { .. }));
        assert!(!grpc_error.is_authentication());
        assert!(!mutation_rejection_is_definitive(&grpc_error));
        assert_anytype_mapping(&grpc_error, Some(ToolErrorCode::Upstream));

        let encoded = format!(
            "{:?}{:?}",
            mutation_rejection_is_definitive(&http_error),
            mutation_rejection_is_definitive(&grpc_error)
        );
        assert!(!encoded.contains("SECRET"));
    }

    #[test]
    fn mutation_rejection_classification_and_wire_errors_never_copy_source_text() {
        for code in [400, 408, 429, 499, 500] {
            let source = api_error(code);
            let classification = mutation_rejection_is_definitive(&source);
            let wire_error = if classification {
                match ToolError::from_anytype(&source) {
                    AnytypeErrorMapping::Ready(error) => error,
                    AnytypeErrorMapping::AmbiguityRequiresCandidates => {
                        panic!("HTTP errors never require candidates")
                    }
                }
            } else {
                ToolError::mutation_indeterminate()
            };
            let encoded = format!(
                "{classification:?} {}",
                serde_json::to_string(&wire_error).unwrap()
            );
            for secret in [
                "SECRET_METHOD_TOKEN",
                "SECRET_URL_TOKEN",
                "SECRET_RESPONSE_BODY",
                "localhost",
            ] {
                assert!(!encoded.contains(secret));
            }
        }
    }

    #[test]
    fn upstream_error_is_stable_and_contains_no_diagnostic_input() {
        let error = ToolError::upstream();

        assert_eq!(
            serde_json::to_value(error).unwrap(),
            json!({
                "code": "upstream",
                "message": "Anytype could not complete the request. Retry later or inspect redacted server diagnostics."
            })
        );
    }

    #[test]
    fn mutation_indeterminate_is_exact_conflict_guidance() {
        let error = ToolError::mutation_indeterminate();

        assert_eq!(error.code(), ToolErrorCode::Conflict);
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            json!({
                "code": "conflict",
                "message": "The mutation may have applied. Reread the object before retrying to avoid applying it twice."
            })
        );
    }

    #[test]
    fn ambiguity_candidates_are_bounded() {
        let error = ToolError::ambiguous((0..25).map(candidate)).unwrap();

        assert_eq!(error.candidates().len(), MAX_ERROR_CANDIDATES);
        assert_eq!(error.candidates()[0].id.as_str(), "id-0");
        assert_eq!(error.candidates()[9].id.as_str(), "id-9");
        assert_eq!(error.code(), ToolErrorCode::Ambiguous);
        assert_eq!(
            serde_json::to_value(&error).unwrap()["candidates"]
                .as_array()
                .unwrap()
                .len(),
            MAX_ERROR_CANDIDATES
        );
        assert_eq!(
            ToolError::ambiguous(std::iter::empty()),
            Err(AmbiguityCandidatesError)
        );
    }

    #[test]
    fn anytype_error_mapping_discards_upstream_response_text() {
        let source = anytype::error::AnytypeError::ApiError {
            code: 500,
            method: "POST".to_owned(),
            url: "http://localhost/private?token=secret".to_owned(),
            message: "Bearer super-secret response body".to_owned(),
        };

        let AnytypeErrorMapping::Ready(error) = ToolError::from_anytype(&source) else {
            panic!("HTTP 500 must map to a ready upstream error");
        };
        let encoded = serde_json::to_string(&error).unwrap();
        assert_eq!(error.code(), ToolErrorCode::Upstream);
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("Bearer"));
        assert!(!encoded.contains("localhost"));
    }

    #[test]
    fn anytype_ambiguity_requires_explicit_candidate_enrichment() {
        let source = anytype::error::AnytypeError::Ambiguous {
            obj_type: "space".to_owned(),
            key: "Roadmap".to_owned(),
            candidates: Vec::new(),
        };

        assert_eq!(
            ToolError::from_anytype(&source),
            AnytypeErrorMapping::AmbiguityRequiresCandidates
        );
    }

    #[test]
    fn candidate_rich_anytype_ambiguity_maps_to_exact_tool_error() {
        let source = anytype::error::AnytypeError::Ambiguous {
            obj_type: "space".to_owned(),
            key: "Roadmap".to_owned(),
            candidates: vec![
                anytype::resolve::ResolveCandidate::new("space-a", "Roadmap"),
                anytype::resolve::ResolveCandidate::new("space-b", "Roadmap"),
            ],
        };

        let AnytypeErrorMapping::Ready(error) = ToolError::from_anytype(&source) else {
            panic!("bounded resolver candidates must complete the tool error");
        };
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            json!({
                "code": "ambiguous",
                "message": "The reference is ambiguous. Retry with one of the candidate identifiers.",
                "candidates": [
                    { "id": "space-a", "name": "Roadmap" },
                    { "id": "space-b", "name": "Roadmap" }
                ]
            })
        );
    }

    #[test]
    fn invalid_anytype_candidate_cannot_reach_tool_output() {
        let source = anytype::error::AnytypeError::Ambiguous {
            obj_type: "space".to_owned(),
            key: "Roadmap".to_owned(),
            candidates: vec![anytype::resolve::ResolveCandidate::new(
                "unsafe/id",
                "Roadmap",
            )],
        };

        assert_eq!(
            ToolError::from_anytype(&source),
            AnytypeErrorMapping::AmbiguityRequiresCandidates
        );

        let source = anytype::error::AnytypeError::Ambiguous {
            obj_type: "space".to_owned(),
            key: "Roadmap".to_owned(),
            candidates: vec![anytype::resolve::ResolveCandidate::new(
                "space-a",
                "x".repeat(MAX_CANDIDATE_NAME_CHARS + 1),
            )],
        };
        assert_eq!(
            ToolError::from_anytype(&source),
            AnytypeErrorMapping::AmbiguityRequiresCandidates
        );
    }

    #[test]
    fn mixed_anytype_candidates_retain_valid_alternatives() {
        let source = anytype::error::AnytypeError::Ambiguous {
            obj_type: "space".to_owned(),
            key: "Roadmap".to_owned(),
            candidates: vec![
                anytype::resolve::ResolveCandidate::new("unsafe/id", "First"),
                anytype::resolve::ResolveCandidate::new("space-a", "Roadmap A"),
                anytype::resolve::ResolveCandidate::new(
                    "space-b",
                    "x".repeat(MAX_CANDIDATE_NAME_CHARS + 1),
                ),
                anytype::resolve::ResolveCandidate::new("space-c", "Roadmap C"),
            ],
        };

        let AnytypeErrorMapping::Ready(error) = ToolError::from_anytype(&source) else {
            panic!("valid alternatives must survive malformed neighbors");
        };
        assert_eq!(error.candidates().len(), 2);
        assert_eq!(error.candidates()[0].id.as_str(), "space-a");
        assert_eq!(error.candidates()[1].id.as_str(), "space-c");
    }

    #[test]
    fn resolution_scan_limit_maps_to_bounded_result() {
        let source = anytype::error::AnytypeError::ResolutionLimitExceeded {
            obj_type: "space".to_owned(),
            key: "Roadmap".to_owned(),
            limit: anytype::resolve::MAX_RESOLVE_SCAN_ITEMS,
        };

        let AnytypeErrorMapping::Ready(error) = ToolError::from_anytype(&source) else {
            panic!("scan limits must map directly to a bounded-result error");
        };
        assert_eq!(error.code(), ToolErrorCode::BoundedResult);
        assert!(error.candidates().is_empty());
    }

    #[test]
    fn oversized_response_maps_without_exposing_limit_metadata() {
        let source = anytype::error::AnytypeError::ResponseTooLarge {
            limit: 8 * 1024 * 1024,
            declared: Some(123_456_789),
        };

        let AnytypeErrorMapping::Ready(error) = ToolError::from_anytype(&source) else {
            panic!("response ceilings must map directly to bounded_result");
        };
        assert_eq!(error.code(), ToolErrorCode::BoundedResult);
        let encoded = serde_json::to_string(&error).unwrap();
        assert!(!encoded.contains("123456789"));
        assert!(!encoded.contains("8388608"));
    }

    #[test]
    fn error_schema_is_closed_and_bounds_messages_and_candidates() {
        let schema = output_schema::<ToolError>().unwrap();

        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["properties"]["message"]["maxLength"], json!(160));
        assert_eq!(
            schema["properties"]["candidates"]["maxItems"],
            json!(MAX_ERROR_CANDIDATES)
        );
        assert_eq!(schema["properties"]["candidates"]["minItems"], json!(1));
        assert_eq!(
            schema["$defs"]["ErrorCandidate"]["additionalProperties"],
            json!(false)
        );
    }
}
