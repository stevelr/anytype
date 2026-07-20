// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! MCP result helpers with structured values and compact JSON text fallback.

use std::fmt;

use crate::error::ToolError;
use rmcp::model::CallToolResult;

/// Opaque failure to serialize a typed tool result.
///
/// The underlying serializer message is intentionally discarded so callers
/// cannot accidentally expose private data through an execution error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultEncodingError;

impl fmt::Display for ResultEncodingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("typed tool result could not be encoded")
    }
}

impl std::error::Error for ResultEncodingError {}

/// Converts a stable tool execution error into an MCP `isError=true` result.
///
/// Serialization of the fixed error model cannot normally fail. The fallback
/// remains a fixed upstream error so even an unexpected encoder failure does
/// not place arbitrary diagnostics in the protocol response.
#[must_use]
pub fn tool_error(error: &ToolError) -> CallToolResult {
    let value = serde_json::to_value(error).unwrap_or_else(|_| {
        serde_json::json!({
            "code": "upstream",
            "message": ToolError::upstream().message(),
        })
    });
    CallToolResult::structured_error(value)
}

#[cfg(test)]
mod tests {
    use rmcp::schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;
    use crate::domain::{DisplayName, ObjectId, ObjectSummary, SpaceId, TypeKey};
    use crate::protocol::{ToolProfile, workflow_tool};

    #[derive(Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only representative input")]
    struct SummaryInput {
        /// Whether to return a summary.
        enabled: bool,
    }

    #[derive(Serialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct SummaryResult {
        /// Compact object metadata.
        object: ObjectSummary,
    }

    #[test]
    fn success_contains_matching_structured_and_compact_json_content() {
        let value = SummaryResult {
            object: ObjectSummary::new(
                ObjectId::new("obj-1").unwrap(),
                DisplayName::new("Roadmap").unwrap(),
                TypeKey::new("page").unwrap(),
                SpaceId::new("space-1").unwrap(),
                None,
            ),
        };

        let contract = workflow_tool::<SummaryInput, SummaryResult>(
            "object_get",
            "Retrieve bounded object metadata.",
            ToolProfile::Read,
        )
        .unwrap();
        let result = contract.success(&value).unwrap();
        let expected = json!({
            "object": {
                "id": "obj-1",
                "name": "Roadmap",
                "type_key": "page",
                "space_id": "space-1",
                "resource_uri": "anytype://spaces/space-1/objects/obj-1"
            }
        });

        assert_eq!(result.structured_content, Some(expected.clone()));
        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.content.len(), 1);
        assert_eq!(
            result.content[0].as_text().unwrap().text,
            expected.to_string()
        );
    }

    #[test]
    fn error_contains_stable_structured_and_compact_json_content() {
        let result = tool_error(&ToolError::authentication());
        let expected = json!({
            "code": "authentication",
            "message": "Anytype authentication failed. Verify the configured credentials and retry."
        });

        assert_eq!(result.structured_content, Some(expected.clone()));
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.content.len(), 1);
        assert_eq!(
            result.content[0].as_text().unwrap().text,
            expected.to_string()
        );
    }
}
