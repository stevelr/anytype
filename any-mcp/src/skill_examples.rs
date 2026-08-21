// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use anytype::chats::MessageBlock;
use rmcp::model::Tool;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    discovery::tag_list_tool,
    object_create::object_create_tool,
    object_update::object_update_tool,
    optional_toolsets::{
        OptionalToolsetSelection, compose_optional_catalog, production_optional_metadata,
        production_optional_registries,
    },
};

const EXAMPLES: &str =
    include_str!("../../skills/skills/any-mcp/references/tool-call-examples.json");
const ALL_OPTIONAL_TOOLSETS: &str = "body-blocks,chats,files,members,schema,views-write";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExampleFile {
    examples: Vec<ToolCallExample>,
    rich_chat_blocks: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCallExample {
    workflow: String,
    step: String,
    tool: String,
    arguments: Value,
}

fn tool_schemas() -> BTreeMap<String, Value> {
    let selection = OptionalToolsetSelection::parse(
        Some(ALL_OPTIONAL_TOOLSETS.to_owned()),
        &production_optional_metadata(),
    )
    .expect("select all production optional toolsets");
    let optional = compose_optional_catalog(
        &selection,
        production_optional_registries(),
        false,
        &[],
        &[],
        &[],
    )
    .expect("compose production optional catalog");
    let base = [
        object_create_tool()
            .expect("object_create contract")
            .into_tool(),
        object_update_tool()
            .expect("object_update contract")
            .into_tool(),
        tag_list_tool().expect("tag_list contract").into_tool(),
    ];

    base.into_iter()
        .chain(optional.tools)
        .map(|tool: Tool| {
            (
                tool.name.to_string(),
                Value::Object(tool.input_schema.as_ref().clone()),
            )
        })
        .collect()
}

#[test]
fn documented_skill_tool_calls_match_production_schemas() {
    let examples: ExampleFile = serde_json::from_str(EXAMPLES).expect("parse skill examples");
    let schemas = tool_schemas();

    assert!(
        !examples.examples.is_empty(),
        "skill examples must not be empty"
    );
    for example in examples.examples {
        let schema = schemas.get(&example.tool).unwrap_or_else(|| {
            panic!(
                "unknown tool in {} / {}: {}",
                example.workflow, example.step, example.tool
            )
        });
        let validator = jsonschema::draft202012::options()
            .build(schema)
            .unwrap_or_else(|error| panic!("compile {} schema: {error}", example.tool));
        if let Err(error) = validator.validate(&example.arguments) {
            panic!(
                "invalid arguments in {} / {} for {}: {error}",
                example.workflow, example.step, example.tool
            );
        }
    }

    serde_json::from_value::<Vec<MessageBlock>>(examples.rich_chat_blocks)
        .expect("rich-chat fallback blocks must match anytype-api");
}
