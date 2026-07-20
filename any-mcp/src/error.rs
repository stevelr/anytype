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
            AnytypeError::ResolutionLimitExceeded { .. } => ToolErrorCode::BoundedResult,
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
    use serde_json::json;

    use super::*;
    use crate::schema::output_schema;

    fn candidate(index: usize) -> ErrorCandidate {
        ErrorCandidate {
            id: EntityId::new(format!("id-{index}")).unwrap(),
            name: BoundedText::new(format!("Candidate {index}")).unwrap(),
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
