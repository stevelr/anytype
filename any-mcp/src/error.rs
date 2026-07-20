// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Stable, bounded, caller-visible MCP tool execution errors.

use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::domain::{BoundedText, EntityId};

/// Maximum number of ambiguity candidates returned to a caller.
pub const MAX_ERROR_CANDIDATES: usize = 10;
/// Maximum number of characters in an ambiguity candidate's display name.
pub const MAX_CANDIDATE_NAME_CHARS: usize = 256;

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

/// Secret-safe error body returned in MCP `structuredContent`.
///
/// Messages are selected only from fixed corrective text. Upstream response
/// bodies, credential values, and arbitrary exception strings cannot enter
/// this wire model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolError {
    /// Stable machine-readable failure category.
    pub code: ToolErrorCode,
    /// Short, fixed corrective message safe to show to a caller.
    #[schemars(length(max = 160))]
    message: &'static str,
    /// Bounded alternatives supplied only for ambiguity errors.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = MAX_ERROR_CANDIDATES))]
    candidates: Vec<ErrorCandidate>,
}

impl ToolError {
    /// Creates a stable error without accepting untrusted diagnostic text.
    #[must_use]
    pub const fn new(code: ToolErrorCode) -> Self {
        Self {
            code,
            message: code.corrective_message(),
            candidates: Vec::new(),
        }
    }

    /// Creates an ambiguity error and caps the returned candidate list.
    #[must_use]
    pub fn ambiguous(candidates: impl IntoIterator<Item = ErrorCandidate>) -> Self {
        Self {
            code: ToolErrorCode::Ambiguous,
            message: ToolErrorCode::Ambiguous.corrective_message(),
            candidates: candidates.into_iter().take(MAX_ERROR_CANDIDATES).collect(),
        }
    }

    /// Maps an `anytype-api` failure to a stable category without copying its
    /// URL, response body, credential text, or diagnostic message.
    #[must_use]
    pub fn from_anytype(error: &anytype::error::AnytypeError) -> Self {
        use anytype::error::AnytypeError;

        let code = match error {
            AnytypeError::Auth { .. }
            | AnytypeError::Unauthorized
            | AnytypeError::Forbidden
            | AnytypeError::NoKeyStore
            | AnytypeError::KeyStore { .. }
            | AnytypeError::GrpcUnavailable { .. } => ToolErrorCode::Authentication,
            AnytypeError::Validation { .. } => ToolErrorCode::Validation,
            AnytypeError::Ambiguous { .. } => ToolErrorCode::Ambiguous,
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
        Self::new(code)
    }

    /// Returns the caller-visible corrective message.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// Returns bounded ambiguity candidates, if present.
    #[must_use]
    pub fn candidates(&self) -> &[ErrorCandidate] {
        &self.candidates
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
        let error = ToolError::new(ToolErrorCode::Upstream);

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
        let error = ToolError::ambiguous((0..25).map(candidate));

        assert_eq!(error.candidates().len(), MAX_ERROR_CANDIDATES);
        assert_eq!(error.candidates()[0].id.as_str(), "id-0");
        assert_eq!(error.candidates()[9].id.as_str(), "id-9");
    }

    #[test]
    fn anytype_error_mapping_discards_upstream_response_text() {
        let source = anytype::error::AnytypeError::ApiError {
            code: 500,
            method: "POST".to_owned(),
            url: "http://localhost/private?token=secret".to_owned(),
            message: "Bearer super-secret response body".to_owned(),
        };

        let encoded = serde_json::to_string(&ToolError::from_anytype(&source)).unwrap();
        assert_eq!(
            ToolError::from_anytype(&source).code,
            ToolErrorCode::Upstream
        );
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("Bearer"));
        assert!(!encoded.contains("localhost"));
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
        assert_eq!(
            schema["$defs"]["ErrorCandidate"]["additionalProperties"],
            json!(false)
        );
    }
}
