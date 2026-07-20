// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Reusable MCP tool contracts and annotation profiles.

use rmcp::{
    model::{Tool, ToolAnnotations},
    schemars::JsonSchema,
};

use crate::schema::{SchemaContractError, input_schema, output_schema};

/// Returns the annotation profile for a read-only, closed-world workflow.
#[must_use]
pub fn read_annotations() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(true)
        .destructive(false)
        .open_world(false)
}

/// Returns the annotation profile for a non-idempotent create workflow.
#[must_use]
pub fn create_annotations() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(false)
        .destructive(false)
        .idempotent(false)
        .open_world(false)
}

/// Returns the annotation profile for an update, edit, or archive workflow.
#[must_use]
pub fn update_annotations() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(false)
        .destructive(true)
        .idempotent(false)
        .open_world(false)
}

/// Creates an `rmcp` tool contract with strict input and output schemas.
///
/// `name` and `description` are static server metadata. Request and response
/// values remain bounded by the supplied typed schemas and runtime
/// deserialization.
pub fn workflow_tool<I, O>(
    name: &'static str,
    description: &'static str,
    annotations: ToolAnnotations,
) -> Result<Tool, SchemaContractError>
where
    I: JsonSchema + 'static,
    O: JsonSchema + 'static,
{
    Ok(Tool::new(name, description, input_schema::<I>()?)
        .with_raw_output_schema(output_schema::<O>()?)
        .with_annotations(annotations))
}

#[cfg(test)]
mod tests {
    use rmcp::schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;
    use crate::domain::ObjectId;

    #[derive(Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "schema-only representative input")]
    struct Input {
        /// Object to retrieve.
        object_id: ObjectId,
    }

    #[derive(Serialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct Output {
        /// Object that was retrieved.
        object_id: ObjectId,
    }

    #[test]
    fn annotation_profiles_serialize_exactly() {
        assert_eq!(
            serde_json::to_value(read_annotations()).unwrap(),
            json!({
                "readOnlyHint": true,
                "destructiveHint": false,
                "openWorldHint": false
            })
        );
        assert_eq!(
            serde_json::to_value(create_annotations()).unwrap(),
            json!({
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": false
            })
        );
        assert_eq!(
            serde_json::to_value(update_annotations()).unwrap(),
            json!({
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": false
            })
        );
    }

    #[test]
    fn workflow_tool_attaches_strict_input_output_and_annotations() {
        let tool = workflow_tool::<Input, Output>(
            "object_get",
            "Retrieve bounded object metadata.",
            read_annotations(),
        )
        .unwrap();

        assert_eq!(tool.name, "object_get");
        assert_eq!(tool.input_schema["additionalProperties"], json!(false));
        assert_eq!(
            tool.output_schema.as_ref().unwrap()["additionalProperties"],
            json!(false)
        );
        assert_eq!(tool.annotations, Some(read_annotations()));
    }
}
