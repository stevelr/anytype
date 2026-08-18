// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Reusable MCP tool contracts and annotation profiles.

use std::marker::PhantomData;

use rmcp::{
    model::{CallToolResult, Tool, ToolAnnotations},
    schemars::JsonSchema,
};
use serde::Serialize;

use crate::{
    result::ResultEncodingError,
    schema::{SchemaContractError, input_schema, output_schema},
};

/// Fixed annotation profiles permitted for bounded Anytype workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolProfile {
    /// Read-only workflow within the closed Anytype world.
    Read,
    /// Non-idempotent additive creation workflow.
    Create,
    /// Destructive update, edit, or archive workflow.
    Update,
}

impl ToolProfile {
    fn annotations(self) -> ToolAnnotations {
        match self {
            Self::Read => ToolAnnotations::new()
                .read_only(true)
                .destructive(false)
                .open_world(false),
            Self::Create => ToolAnnotations::new()
                .read_only(false)
                .destructive(false)
                .idempotent(false)
                .open_world(false),
            Self::Update => ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(false)
                .open_world(false),
        }
    }
}

/// Typed MCP tool contract linking one strict output schema to success values.
///
/// The inner `rmcp` metadata is created only by [`workflow_tool`], so every
/// public success response has first passed the same strict output-schema
/// checks advertised to the client.
#[derive(Debug, Clone)]
pub struct WorkflowTool<O> {
    tool: Tool,
    output: PhantomData<fn() -> O>,
}

impl<O> WorkflowTool<O> {
    /// Borrows the `rmcp` tool metadata for registration or inspection.
    #[must_use]
    pub const fn as_tool(&self) -> &Tool {
        &self.tool
    }

    /// Consumes the typed contract and returns its `rmcp` metadata.
    #[must_use]
    pub fn into_tool(self) -> Tool {
        self.tool
    }
}

impl<O> WorkflowTool<O>
where
    O: Serialize,
{
    /// Encodes a success value whose Rust type exactly matches the contract's
    /// validated output schema.
    ///
    /// ```compile_fail
    /// use any_mcp::protocol::{ToolProfile, workflow_tool};
    /// use schemars::JsonSchema;
    /// use serde::{Deserialize, Serialize};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// #[derive(Deserialize, JsonSchema)]
    /// #[serde(deny_unknown_fields)]
    /// struct Input {
    ///     /// Whether to retrieve the object.
    ///     enabled: bool,
    /// }
    /// #[derive(Serialize, JsonSchema)]
    /// #[serde(deny_unknown_fields)]
    /// struct Output {
    ///     /// Whether retrieval completed.
    ///     complete: bool,
    /// }
    /// #[derive(Serialize, JsonSchema)]
    /// #[serde(deny_unknown_fields)]
    /// struct DifferentOutput {
    ///     /// Whether a different operation completed.
    ///     different: bool,
    /// }
    ///
    /// let contract = workflow_tool::<Input, Output>(
    ///     "object_get",
    ///     "Retrieve bounded object metadata.",
    ///     ToolProfile::Read,
    /// )?;
    /// contract.success(&DifferentOutput { different: true })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn success(&self, value: &O) -> Result<CallToolResult, ResultEncodingError> {
        serde_json::to_value(value)
            .map(CallToolResult::structured)
            .map(|mut result| {
                result.result_type = None;
                result
            })
            .map_err(|_| ResultEncodingError)
    }
}

/// Creates an `rmcp` tool contract with strict input and output schemas.
///
/// `name` and `description` are static server metadata. Request and response
/// values remain bounded by the supplied typed schemas and runtime
/// deserialization.
pub fn workflow_tool<I, O>(
    name: &'static str,
    description: &'static str,
    profile: ToolProfile,
) -> Result<WorkflowTool<O>, SchemaContractError>
where
    I: JsonSchema + 'static,
    O: JsonSchema + Serialize + 'static,
{
    let tool = Tool::new(name, description, input_schema::<I>()?)
        .with_raw_output_schema(output_schema::<O>()?)
        .with_annotations(profile.annotations());
    Ok(WorkflowTool {
        tool,
        output: PhantomData,
    })
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

    #[derive(Serialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct UnboundedOutput {
        /// Intentionally unbounded output text.
        value: String,
    }

    #[test]
    fn annotation_profiles_serialize_exactly() {
        assert_eq!(
            serde_json::to_value(ToolProfile::Read.annotations()).unwrap(),
            json!({
                "readOnlyHint": true,
                "destructiveHint": false,
                "openWorldHint": false
            })
        );
        assert_eq!(
            serde_json::to_value(ToolProfile::Create.annotations()).unwrap(),
            json!({
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": false
            })
        );
        assert_eq!(
            serde_json::to_value(ToolProfile::Update.annotations()).unwrap(),
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
        let contract = workflow_tool::<Input, Output>(
            "object_get",
            "Retrieve bounded object metadata.",
            ToolProfile::Read,
        )
        .unwrap();
        let tool = contract.as_tool();

        assert_eq!(tool.name, "object_get");
        assert_eq!(tool.input_schema["additionalProperties"], json!(false));
        assert_eq!(
            tool.output_schema.as_ref().unwrap()["additionalProperties"],
            json!(false)
        );
        assert_eq!(tool.annotations, Some(ToolProfile::Read.annotations()));

        let result = contract
            .success(&Output {
                object_id: ObjectId::new("obj-1").unwrap(),
            })
            .unwrap();
        assert_eq!(result.structured_content.unwrap()["object_id"], "obj-1");
    }

    #[test]
    fn unbounded_output_cannot_form_a_typed_contract() {
        assert_eq!(
            workflow_tool::<Input, UnboundedOutput>(
                "object_get",
                "Retrieve bounded object metadata.",
                ToolProfile::Read,
            )
            .map(|_| ()),
            Err(SchemaContractError)
        );
    }
}
