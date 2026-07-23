// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Transport-neutral scenarios and live-coverage ownership declarations.
#![cfg_attr(not(feature = "acceptance-harness"), allow(dead_code))]

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    future::Future,
    hash::Hash,
    pin::Pin,
    time::Duration,
};

use anytype::{
    body::{
        BlockContent, BlockRestrictions, BodyBlock, BodySnapshot, CalloutIcon, DividerStyle,
        EmbedProcessor, HorizontalAlign, LayoutStyle, LinkCardStyle, LinkDescriptionMode,
        LinkIconSize, MarkKind, TextStyle, VerticalAlign,
    },
    prelude::{BodyOp, Color, InsertPosition, NewBlock, ObjectLayout, PropertyFormat},
    test_util::{DisposableFailureCategory, TestContext, unique_suffix},
};

/// Seeded value that must never appear in stderr or protocol errors.
pub const BODY_DIAGNOSTIC_SECRET: &str = "SECRET_BODY_DIAGNOSTIC_SENTINEL";
/// Exact root-inclusive DFS item count in the live pagination fixture.
pub const BODY_PAGINATION_ITEM_COUNT: usize = 20;

fn body_block_state_except_children_matches(actual: &BodyBlock, expected: &BodyBlock) -> bool {
    actual.id == expected.id
        && actual.content == expected.content
        && actual.align == expected.align
        && actual.vertical_align == expected.vertical_align
        && actual.background_color == expected.background_color
        && actual.restrictions == expected.restrictions
}

fn body_initial_prefix_ids_preserved(blocks: &[&BodyBlock], initial_blocks: &[BodyBlock]) -> bool {
    blocks.len() >= initial_blocks.len()
        && blocks
            .iter()
            .zip(initial_blocks)
            .all(|(actual, expected)| actual.id == expected.id)
}

fn body_initial_root_nonchild_state_preserved(
    blocks: &[&BodyBlock],
    initial_blocks: &[BodyBlock],
) -> bool {
    blocks
        .first()
        .zip(initial_blocks.first())
        .is_some_and(|(actual, expected)| {
            body_block_state_except_children_matches(actual, expected)
        })
}

fn body_initial_nonroot_full_state_preserved(
    blocks: &[&BodyBlock],
    initial_blocks: &[BodyBlock],
) -> bool {
    blocks.len() >= initial_blocks.len()
        && blocks
            .iter()
            .skip(1)
            .zip(initial_blocks.iter().skip(1))
            .all(|(actual, expected)| *actual == expected)
}

fn body_initial_root_children_preserved(
    snapshot: &BodySnapshot,
    initial_blocks: &[BodyBlock],
) -> bool {
    initial_blocks.first().is_some_and(|initial_root| {
        snapshot.root().children.get(..initial_root.children.len())
            == Some(initial_root.children.as_slice())
    })
}

fn body_initial_prefix_gate(
    prefix_ids_ok: bool,
    _root_nonchild_state_ok: bool,
    nonroot_full_state_ok: bool,
    root_children_prefix_ok: bool,
) -> bool {
    prefix_ids_ok && nonroot_full_state_ok && root_children_prefix_ok
}

fn body_pagination_suffix_spec(append_count: usize) -> Vec<(TextStyle, String)> {
    (0..append_count)
        .map(|index| {
            if index == 0 {
                (TextStyle::Header1, "Existing heading".to_owned())
            } else {
                (TextStyle::Paragraph, format!("Paragraph {}", index - 1))
            }
        })
        .collect()
}

fn body_pagination_append_operations(append_count: usize) -> Result<Vec<BodyOp>, String> {
    if append_count > BODY_PAGINATION_ITEM_COUNT {
        return Err("body fixture append count exceeds the exact page size".to_owned());
    }
    let mut operations = Vec::with_capacity(append_count);
    for (style, text) in body_pagination_suffix_spec(append_count) {
        let block = match style {
            TextStyle::Header1 => NewBlock::heading(1, text)
                .map_err(|_| "body fixture heading constructor failed".to_owned())?,
            TextStyle::Paragraph => NewBlock::paragraph(text)
                .map_err(|_| "body fixture paragraph constructor failed".to_owned())?,
            _ => return Err("body fixture plan contained an unsupported style".to_owned()),
        };
        operations.push(BodyOp::Append { block });
    }
    Ok(operations)
}

fn is_exact_body_pagination_fixture(
    snapshot: &BodySnapshot,
    initial_blocks: &[BodyBlock],
    created_ids: &[String],
    expected_suffix: &[(TextStyle, String)],
) -> bool {
    let blocks = snapshot.iter().collect::<Vec<_>>();
    let Some(suffix) = blocks.get(initial_blocks.len()..) else {
        return false;
    };
    let prefix_ids_unchanged = body_initial_prefix_ids_preserved(&blocks, initial_blocks);
    let root_nonchild_state_unchanged =
        body_initial_root_nonchild_state_preserved(&blocks, initial_blocks);
    let nonroot_full_state_unchanged =
        body_initial_nonroot_full_state_preserved(&blocks, initial_blocks);
    let root_children_prefix_unchanged =
        body_initial_root_children_preserved(snapshot, initial_blocks);
    let initial_prefix_valid = body_initial_prefix_gate(
        prefix_ids_unchanged,
        root_nonchild_state_unchanged,
        nonroot_full_state_unchanged,
        root_children_prefix_unchanged,
    );
    let suffix_ids_match = suffix
        .iter()
        .map(|block| block.id.as_str())
        .eq(created_ids.iter().map(String::as_str));
    let suffix_content_matches =
        suffix
            .iter()
            .zip(expected_suffix)
            .all(|(block, (expected_style, expected_text))| {
                matches!(
                    &block.content,
                    BlockContent::Text(content)
                        if content.style == *expected_style && content.text == *expected_text
                )
            });
    let direct_root_suffix = snapshot.root().children.len() >= created_ids.len()
        && snapshot
            .root()
            .children
            .iter()
            .rev()
            .take(created_ids.len())
            .map(|id| id.as_str())
            .eq(created_ids.iter().rev().map(String::as_str));
    blocks.len() == BODY_PAGINATION_ITEM_COUNT
        && initial_prefix_valid
        && suffix.len() == created_ids.len()
        && suffix.len() == expected_suffix.len()
        && suffix_ids_match
        && suffix_content_matches
        && direct_root_suffix
}

#[test]
fn body_pagination_fixture_plan_fills_twenty_including_preserved_root_prefix() {
    let expected = body_pagination_suffix_spec(16);
    assert_eq!(expected.len(), 16);
    assert_eq!(
        expected.first(),
        Some(&(TextStyle::Header1, "Existing heading".to_owned()))
    );
    assert_eq!(
        expected.last(),
        Some(&(TextStyle::Paragraph, "Paragraph 14".to_owned()))
    );
    for initial_count in [1, 4, 19] {
        let append_count = BODY_PAGINATION_ITEM_COUNT - initial_count;
        let operations =
            body_pagination_append_operations(append_count).expect("valid fixture constructors");
        assert_eq!(initial_count + operations.len(), BODY_PAGINATION_ITEM_COUNT);
        assert!(
            operations
                .iter()
                .all(|operation| matches!(operation, BodyOp::Append { .. }))
        );
        assert_eq!(
            body_pagination_suffix_spec(append_count)
                .first()
                .map(|(style, _)| *style),
            Some(TextStyle::Header1)
        );
    }
}

#[test]
fn body_pagination_prefix_gate_allows_only_root_metadata_drift() {
    assert!(body_initial_prefix_gate(true, true, true, true));
    assert!(body_initial_prefix_gate(true, false, true, true));
    assert!(!body_initial_prefix_gate(false, true, true, true));
    assert!(!body_initial_prefix_gate(true, true, false, true));
    assert!(!body_initial_prefix_gate(true, true, true, false));
}

/// Content-free evidence from one transport-neutral rich-body workflow.
#[derive(Debug, PartialEq, Eq)]
pub struct BodyScenarioEvidence {
    pub normalized_results: Vec<Value>,
    pub listed_block_count: usize,
}

/// Closed, payload-free stage for a body acceptance failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyScenarioStage {
    /// Deterministic fixture construction.
    Fixture,
    /// Catalog and exact pagination checks.
    Pagination,
    /// Stale continuation rejection.
    StaleCursor,
    /// Primitive mutation workflows.
    Primitive,
    /// Primary rich-page create.
    RichPrimaryCreate,
    /// Primary rich-page independent readback.
    RichPrimaryReadback,
    /// Primary rich-page replay.
    RichPrimaryReplay,
    /// Rich-page update matrix.
    RichUpdates,
    /// Supplemental rich-page variants.
    RichSupplemental,
}

/// Payload-free failure returned by the shared body acceptance scenario.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyScenarioFailure {
    stage: BodyScenarioStage,
}

impl BodyScenarioFailure {
    /// Returns the closed disposable diagnostic category for this failure.
    #[must_use]
    pub const fn category(self) -> DisposableFailureCategory {
        match self.stage {
            BodyScenarioStage::Fixture => DisposableFailureCategory::BodyFixture,
            BodyScenarioStage::Pagination => DisposableFailureCategory::BodyPagination,
            BodyScenarioStage::StaleCursor => DisposableFailureCategory::BodyStaleCursor,
            BodyScenarioStage::Primitive => DisposableFailureCategory::BodyPrimitive,
            BodyScenarioStage::RichPrimaryCreate => {
                DisposableFailureCategory::BodyRichPrimaryCreate
            }
            BodyScenarioStage::RichPrimaryReadback => {
                DisposableFailureCategory::BodyRichPrimaryReadback
            }
            BodyScenarioStage::RichPrimaryReplay => {
                DisposableFailureCategory::BodyRichPrimaryReplay
            }
            BodyScenarioStage::RichUpdates => DisposableFailureCategory::BodyRichUpdates,
            BodyScenarioStage::RichSupplemental => DisposableFailureCategory::BodyRichSupplemental,
        }
    }
}

/// Heap-owned future for the fixture-heavy rich-body acceptance workflow.
pub type BodyScenarioFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BodyScenarioEvidence, BodyScenarioFailure>> + 'a>>;

#[test]
fn body_scenario_futures_keep_only_a_heap_handle_inline() {
    assert!(std::mem::size_of::<BodyScenarioFuture<'static>>() <= 2 * std::mem::size_of::<usize>());
    assert!(
        std::mem::size_of::<BodyReadOnlyScenarioFuture<'static>>()
            <= 2 * std::mem::size_of::<usize>()
    );
}

/// Content-free evidence from one read-only body catalog check.
// This shared module is also compiled into direct-router unit tests, whose
// body slice intentionally exercises only the read-write scenario.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub struct BodyReadOnlyEvidence {
    pub body_tools: Vec<String>,
    pub list_result: Value,
    pub mutation_error_categories: Vec<String>,
}

/// Fully validated, transport-neutral evidence for one domain tool error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolErrorEvidence {
    result: Value,
    code: String,
}

impl ToolErrorEvidence {
    /// Validates the complete MCP tool-error result, including its canonical
    /// text duplicate. Preview may add only `resultType: complete`.
    pub fn from_result(result: &Value, preview: bool) -> Result<Self, String> {
        let object = result
            .as_object()
            .ok_or_else(|| "tool error result was not an object".to_owned())?;
        let expected_keys = if preview { 4 } else { 3 };
        if object.len() != expected_keys
            || object.get("isError") != Some(&Value::Bool(true))
            || preview
                != object
                    .get("resultType")
                    .is_some_and(|value| value == "complete")
        {
            return Err("tool error result envelope was not exact".to_owned());
        }
        let structured = object
            .get("structuredContent")
            .and_then(Value::as_object)
            .ok_or_else(|| "tool error omitted structured content".to_owned())?;
        if !(2..=3).contains(&structured.len())
            || !structured
                .keys()
                .all(|key| matches!(key.as_str(), "code" | "message" | "candidates"))
        {
            return Err("tool error structured content was not exact".to_owned());
        }
        let code = structured
            .get("code")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "tool error omitted code".to_owned())?
            .to_owned();
        structured
            .get("message")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "tool error omitted message".to_owned())?;
        if let Some(candidates) = structured.get("candidates") {
            let candidates = candidates
                .as_array()
                .filter(|values| !values.is_empty() && values.len() <= 8)
                .ok_or_else(|| "tool error candidates were not bounded".to_owned())?;
            if !candidates.iter().all(|candidate| {
                candidate.as_object().is_some_and(|candidate| {
                    candidate.len() == 2
                        && candidate
                            .get("id")
                            .and_then(Value::as_str)
                            .is_some_and(|value| !value.is_empty() && value.len() <= 256)
                        && candidate
                            .get("name")
                            .and_then(Value::as_str)
                            .is_some_and(|value| !value.is_empty() && value.len() <= 256)
                })
            }) {
                return Err("tool error candidates were not exact".to_owned());
            }
        }
        let content = object
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| "tool error omitted content".to_owned())?;
        let [item] = content.as_slice() else {
            return Err("tool error content count was not exact".to_owned());
        };
        let item = item
            .as_object()
            .ok_or_else(|| "tool error content item was not an object".to_owned())?;
        if item.len() != 2 || item.get("type") != Some(&json!("text")) {
            return Err("tool error content item was not canonical text".to_owned());
        }
        let text = item
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| "tool error text duplicate was absent".to_owned())?;
        let structured_value = Value::Object(structured.clone());
        let canonical = serde_json::to_string(&structured_value)
            .map_err(|_| "tool error structured content was not serializable".to_owned())?;
        if text != canonical
            || serde_json::from_str::<Value>(text).ok().as_ref() != Some(&structured_value)
        {
            return Err("tool error text duplicate was not canonical".to_owned());
        }
        let mut normalized = object.clone();
        normalized.remove("resultType");
        Ok(Self {
            result: Value::Object(normalized),
            code,
        })
    }

    /// Returns the stable domain error code after full result validation.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the complete transport-neutral tool result.
    #[must_use]
    pub fn normalized_result(&self) -> &Value {
        &self.result
    }
}

#[test]
fn tool_error_evidence_requires_complete_canonical_result() {
    let structured = json!({"code":"conflict","message":"Conflict."});
    let stable = json!({
        "content":[{"type":"text","text":serde_json::to_string(&structured).expect("canonical")}],
        "structuredContent":structured,
        "isError":true
    });
    let mut preview = stable.clone();
    preview["resultType"] = json!("complete");
    assert_eq!(
        ToolErrorEvidence::from_result(&stable, false)
            .expect("stable error")
            .code(),
        "conflict"
    );
    assert_eq!(
        ToolErrorEvidence::from_result(&preview, true)
            .expect("preview error")
            .normalized_result(),
        &stable
    );
    let ambiguous_structured = json!({
        "code":"ambiguous",
        "message":"Choose one.",
        "candidates":[{"id":"candidate-1","name":"Candidate"}]
    });
    let ambiguous = json!({
        "content":[{
            "type":"text",
            "text":serde_json::to_string(&ambiguous_structured).expect("canonical")
        }],
        "structuredContent":ambiguous_structured,
        "isError":true
    });
    assert_eq!(
        ToolErrorEvidence::from_result(&ambiguous, false)
            .expect("ambiguous error")
            .code(),
        "ambiguous"
    );

    let mutate = |pointer: &str, value: Value| {
        let mut candidate = stable.clone();
        *candidate.pointer_mut(pointer).expect("test pointer") = value;
        candidate
    };
    let mut extra_structured = stable.clone();
    extra_structured["structuredContent"]["extra"] = json!(true);
    let mut extra_result = stable.clone();
    extra_result["extra"] = json!(true);
    let mut extra_content = stable.clone();
    extra_content["content"]
        .as_array_mut()
        .expect("content array")
        .push(json!({"type":"text","text":"duplicate"}));
    for invalid in [
        mutate("/isError", json!(false)),
        mutate("/content/0/type", json!("image")),
        mutate(
            "/content/0/text",
            json!("{\"message\":\"Conflict.\",\"code\":\"conflict\"}"),
        ),
        mutate("/structuredContent/code", json!("")),
        mutate("/structuredContent/message", json!("")),
        extra_structured,
        extra_result,
        extra_content,
    ] {
        assert!(ToolErrorEvidence::from_result(&invalid, false).is_err());
    }
    assert!(ToolErrorEvidence::from_result(&stable, true).is_err());
    assert!(ToolErrorEvidence::from_result(&preview, false).is_err());
    preview["resultType"] = json!("partial");
    assert!(ToolErrorEvidence::from_result(&preview, true).is_err());
}

/// Heap-owned future for the read-only rich-body acceptance workflow.
pub type BodyReadOnlyScenarioFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BodyReadOnlyEvidence, String>> + 'a>>;

/// Payload-free production lifecycle counters optionally exposed by a direct
/// acceptance driver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BodyDriverMetrics {
    pub page_create_polls: usize,
    pub show_attempts: usize,
    pub foreground_close_attempts: usize,
    pub foreground_close_confirmed: usize,
    pub fallback_close_attempts: usize,
    pub fallback_close_confirmed: usize,
    pub write_polls: usize,
    pub show_limit_rejections: usize,
    pub non_show_limit_rejections: usize,
    pub close_limit_rejections: usize,
    pub mutation_limit_rejections: usize,
}

fn body_metrics_delta(
    before: BodyDriverMetrics,
    after: BodyDriverMetrics,
) -> Result<BodyDriverMetrics, String> {
    macro_rules! delta {
        ($field:ident) => {
            after.$field.checked_sub(before.$field).ok_or_else(|| {
                format!("body acceptance counter decreased: {}", stringify!($field))
            })?
        };
    }
    Ok(BodyDriverMetrics {
        page_create_polls: delta!(page_create_polls),
        show_attempts: delta!(show_attempts),
        foreground_close_attempts: delta!(foreground_close_attempts),
        foreground_close_confirmed: delta!(foreground_close_confirmed),
        fallback_close_attempts: delta!(fallback_close_attempts),
        fallback_close_confirmed: delta!(fallback_close_confirmed),
        write_polls: delta!(write_polls),
        show_limit_rejections: delta!(show_limit_rejections),
        non_show_limit_rejections: delta!(non_show_limit_rejections),
        close_limit_rejections: delta!(close_limit_rejections),
        mutation_limit_rejections: delta!(mutation_limit_rejections),
    })
}

fn expected_rich_metrics(page_create_polls: usize, blocks: usize) -> BodyDriverMetrics {
    let shows = blocks.saturating_add(1);
    BodyDriverMetrics {
        page_create_polls,
        show_attempts: shows,
        foreground_close_attempts: shows,
        foreground_close_confirmed: shows,
        write_polls: blocks,
        ..BodyDriverMetrics::default()
    }
}

#[derive(Clone, Copy)]
enum BodyMetricExpectation {
    Exact(BodyDriverMetrics),
    PrimitiveMutation,
}

impl BodyMetricExpectation {
    fn matches(self, observed: BodyDriverMetrics) -> bool {
        match self {
            Self::Exact(expected) => observed == expected,
            Self::PrimitiveMutation => primitive_metrics_within_verification_budget(observed),
        }
    }
}

fn primitive_metrics_within_verification_budget(observed: BodyDriverMetrics) -> bool {
    (2..=4).contains(&observed.show_attempts)
        && observed.page_create_polls == 0
        && observed.foreground_close_attempts == observed.show_attempts
        && observed.foreground_close_confirmed == observed.show_attempts
        && observed.fallback_close_attempts == 0
        && observed.fallback_close_confirmed == 0
        && observed.write_polls == 1
        && observed.show_limit_rejections == 0
        && observed.non_show_limit_rejections == 0
        && observed.close_limit_rejections == 0
        && observed.mutation_limit_rejections == 0
}

#[test]
fn primitive_metric_budget_accepts_one_to_three_verification_rounds() {
    for verification_attempts in 1..=3 {
        let shows = 1 + verification_attempts;
        assert!(primitive_metrics_within_verification_budget(
            BodyDriverMetrics {
                show_attempts: shows,
                foreground_close_attempts: shows,
                foreground_close_confirmed: shows,
                write_polls: 1,
                ..BodyDriverMetrics::default()
            }
        ));
    }
    for shows in [1, 5] {
        assert!(!primitive_metrics_within_verification_budget(
            BodyDriverMetrics {
                show_attempts: shows,
                foreground_close_attempts: shows,
                foreground_close_confirmed: shows,
                write_polls: 1,
                ..BodyDriverMetrics::default()
            }
        ));
    }
    for invalid in [
        BodyDriverMetrics {
            show_attempts: 3,
            foreground_close_attempts: 2,
            foreground_close_confirmed: 3,
            write_polls: 1,
            ..BodyDriverMetrics::default()
        },
        BodyDriverMetrics {
            show_attempts: 3,
            foreground_close_attempts: 3,
            foreground_close_confirmed: 2,
            write_polls: 1,
            ..BodyDriverMetrics::default()
        },
        BodyDriverMetrics {
            show_attempts: 3,
            foreground_close_attempts: 3,
            foreground_close_confirmed: 3,
            fallback_close_attempts: 1,
            write_polls: 1,
            ..BodyDriverMetrics::default()
        },
        BodyDriverMetrics {
            show_attempts: 3,
            foreground_close_attempts: 3,
            foreground_close_confirmed: 3,
            write_polls: 2,
            ..BodyDriverMetrics::default()
        },
        BodyDriverMetrics {
            show_attempts: 3,
            foreground_close_attempts: 3,
            foreground_close_confirmed: 3,
            write_polls: 1,
            show_limit_rejections: 1,
            ..BodyDriverMetrics::default()
        },
    ] {
        assert!(!primitive_metrics_within_verification_budget(invalid));
    }
}

fn expected_create_replay_metrics() -> BodyDriverMetrics {
    BodyDriverMetrics {
        show_attempts: 1,
        foreground_close_attempts: 1,
        foreground_close_confirmed: 1,
        ..BodyDriverMetrics::default()
    }
}

async fn call_body_tool_with_metrics(
    driver: &mut impl McpDriver,
    name: &'static str,
    arguments: Value,
    expected: BodyMetricExpectation,
    label: &'static str,
) -> Result<Value, String> {
    let before = driver.body_acceptance_metrics();
    let result = driver.call_tool(name, arguments).await?;
    let after = driver.body_acceptance_metrics();
    if let (Some(before), Some(after)) = (before, after) {
        let observed = body_metrics_delta(before, after)?;
        if !expected.matches(observed) {
            return Err(format!("{label} production metrics diverged: {observed:?}"));
        }
    }
    Ok(result)
}

#[derive(Clone, Copy)]
enum BodyUpdateExpectation {
    Text,
    TextStyle,
    Checked(bool),
    TextColor(Option<&'static str>),
    CalloutIcon(bool),
    DividerStyle,
    BackgroundColor(Option<&'static str>),
    HorizontalAlign,
    VerticalAlign,
    EmbedSource,
    LinkAppearance,
}

fn update_target_changed_exactly(
    before: &BodyBlock,
    after: &BodyBlock,
    expectation: BodyUpdateExpectation,
) -> bool {
    let mut restored = after.clone();
    let exact_value = match expectation {
        BodyUpdateExpectation::Text => {
            let (BlockContent::Text(before), BlockContent::Text(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = after.text == "matrix text"
                && after.marks.len() == 1
                && after.marks[0].range.start == 0
                && after.marks[0].range.end == 6
                && matches!(after.marks[0].kind, MarkKind::Bold);
            after.text.clone_from(&before.text);
            after.marks.clone_from(&before.marks);
            exact
        }
        BodyUpdateExpectation::TextStyle => {
            let (BlockContent::Text(before), BlockContent::Text(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = after.style == TextStyle::Header2;
            after.style = before.style;
            exact
        }
        BodyUpdateExpectation::Checked(expected) => {
            let (BlockContent::Text(before), BlockContent::Text(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = after.checked == expected;
            after.checked = before.checked;
            exact
        }
        BodyUpdateExpectation::TextColor(expected) => {
            let (BlockContent::Text(before), BlockContent::Text(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = after.color.as_ref().map(|color| color.as_str()) == expected;
            after.color.clone_from(&before.color);
            exact
        }
        BodyUpdateExpectation::CalloutIcon(present) => {
            let (BlockContent::Text(before), BlockContent::Text(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = if present {
                matches!(after.icon, Some(CalloutIcon::Emoji(ref emoji)) if emoji == "📌")
            } else {
                after.icon.is_none()
            };
            after.icon.clone_from(&before.icon);
            exact
        }
        BodyUpdateExpectation::DividerStyle => {
            let (BlockContent::Divider(before), BlockContent::Divider(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = *after == DividerStyle::Line;
            *after = *before;
            exact
        }
        BodyUpdateExpectation::BackgroundColor(expected) => {
            let exact = restored
                .background_color
                .as_ref()
                .map(|color| color.as_str())
                == expected;
            restored
                .background_color
                .clone_from(&before.background_color);
            exact
        }
        BodyUpdateExpectation::HorizontalAlign => {
            let exact = restored.align == HorizontalAlign::Left;
            restored.align = before.align;
            exact
        }
        BodyUpdateExpectation::VerticalAlign => {
            let exact = restored.vertical_align == VerticalAlign::Top;
            restored.vertical_align = before.vertical_align;
            exact
        }
        BodyUpdateExpectation::EmbedSource => {
            let (BlockContent::Embed(before), BlockContent::Embed(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = after.processor == EmbedProcessor::Mermaid
                && after.text == "graph LR; Updated-->Verified";
            after.text.clone_from(&before.text);
            exact
        }
        BodyUpdateExpectation::LinkAppearance => {
            let (BlockContent::Link(before), BlockContent::Link(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = after.card_style == LinkCardStyle::Inline
                && after.icon_size == LinkIconSize::Medium
                && after.description == LinkDescriptionMode::Added
                && after.relations.is_empty();
            after.card_style = before.card_style;
            after.icon_size = before.icon_size;
            after.description = before.description;
            after.relations.clone_from(&before.relations);
            exact
        }
    };
    exact_value && restored == *before
}

fn exact_update_snapshot_transition(
    before: &BodySnapshot,
    after: &BodySnapshot,
    block_id: &str,
    expectation: BodyUpdateExpectation,
) -> bool {
    let scope_exact = before.space_id == after.space_id
        && before.object_id == after.object_id
        && before.root_id == after.root_id
        && before.len() == after.len();
    if !scope_exact {
        return false;
    }
    let before_ids = before.iter().map(|block| &block.id).collect::<Vec<_>>();
    let after_ids = after.iter().map(|block| &block.id).collect::<Vec<_>>();
    let dfs_order_exact = before_ids == after_ids;
    if !dfs_order_exact {
        return false;
    }
    let mut target_exact = false;
    let mut root_opaque_semantics_exact = false;
    let mut nonroot_exact = true;
    for prior in before.iter() {
        let Some(fresh) = after.get(&prior.id) else {
            return false;
        };
        if prior.id.as_str() == block_id {
            target_exact = update_target_changed_exactly(prior, fresh, expectation);
        } else if prior.id == before.root_id {
            let mut restored = fresh.clone();
            root_opaque_semantics_exact = match (&prior.content, &mut restored.content) {
                (
                    BlockContent::Unsupported(prior_opaque),
                    BlockContent::Unsupported(fresh_opaque),
                ) => {
                    fresh_opaque.summary.approx_bytes = prior_opaque.summary.approx_bytes;
                    restored == *prior
                }
                _ => fresh == prior,
            };
        } else if fresh != prior {
            nonroot_exact = false;
        }
    }
    target_exact && root_opaque_semantics_exact && nonroot_exact
}

#[allow(clippy::too_many_arguments)]
async fn run_body_update_arm(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    object_id: &str,
    snapshot_hash: &str,
    block_id: &str,
    change: Value,
    expectation: BodyUpdateExpectation,
    label: &'static str,
) -> Result<(Value, String), String> {
    let before = ctx
        .client
        .blocks()
        .body(&ctx.space_id, object_id)
        .fetch()
        .await
        .map_err(|_| format!("{label} independent before read failed"))?;
    let before_metrics = driver.body_acceptance_metrics();
    let result = driver
        .call_tool(
            "body_block_update",
            json!({
                "space":ctx.space_id,
                "object_id":object_id,
                "expected_snapshot_hash":snapshot_hash,
                "block_id":block_id,
                "change":change
            }),
        )
        .await?;
    if let (Some(before), Some(after)) = (before_metrics, driver.body_acceptance_metrics()) {
        let observed = body_metrics_delta(before, after)?;
        if !primitive_metrics_within_verification_budget(observed) {
            return Err(format!("{label} production metrics diverged: {observed:?}"));
        }
    }
    let after = ctx
        .client
        .blocks()
        .body(&ctx.space_id, object_id)
        .fetch()
        .await
        .map_err(|_| format!("{label} independent after read failed"))?;
    if !exact_update_snapshot_transition(&before, &after, block_id, expectation) {
        return Err(format!(
            "{label} changed more or less than its one exact typed field"
        ));
    }
    let next_hash = body_string(&result, "/snapshot_hash", "update snapshot hash")?.to_owned();
    Ok((normalize_body_result(&result), next_hash))
}

/// Proves that read-only mode advertises only body reads and rejects every
/// direct mutation name before decoding caller arguments.
#[allow(dead_code)]
pub fn run_body_read_only_scenario<'a>(
    driver: &'a mut impl McpDriver,
    space_id: &'a str,
    object_id: &'a str,
) -> BodyReadOnlyScenarioFuture<'a> {
    Box::pin(run_body_read_only_scenario_inner(
        driver, space_id, object_id,
    ))
}

async fn run_body_read_only_scenario_inner(
    driver: &mut impl McpDriver,
    space_id: &str,
    object_id: &str,
) -> Result<BodyReadOnlyEvidence, String> {
    const BODY_TOOLS: [&str; 6] = [
        "body_block_create",
        "body_block_delete",
        "body_block_list",
        "body_block_move",
        "body_block_update",
        "rich_page_create",
    ];
    const MUTATIONS: [&str; 5] = [
        "body_block_create",
        "body_block_update",
        "body_block_delete",
        "body_block_move",
        "rich_page_create",
    ];
    let body_tools = driver
        .list_tools()
        .await?
        .into_iter()
        .filter(|name| BODY_TOOLS.contains(&name.as_str()))
        .collect::<Vec<_>>();
    if body_tools != ["body_block_list"] {
        return Err("read-only body catalog was not the exact one-tool inventory".to_owned());
    }
    let list_result = driver
        .call_tool(
            "body_block_list",
            json!({"space":space_id,"object_id":object_id,"limit":8}),
        )
        .await?;
    if list_result["space_id"] != space_id
        || list_result["object_id"] != object_id
        || list_result["items"]
            .as_array()
            .is_none_or(|items| items.is_empty())
    {
        return Err("read-only body list did not return the shared fixture".to_owned());
    }
    let list_result = normalize_body_result(&list_result);
    let mut mutation_error_categories = Vec::new();
    for name in MUTATIONS {
        let error = driver
            .call_tool_error(name, json!({"SECRET_UNPARSED_BODY_VALUE":true}))
            .await?;
        if error
            .normalized_result()
            .to_string()
            .contains("SECRET_UNPARSED_BODY_VALUE")
        {
            return Err("read-only mutation error exposed caller input".to_owned());
        }
        if error.code() != "validation" {
            return Err("read-only mutation did not fail before argument decoding".to_owned());
        }
        mutation_error_categories.push("validation".to_owned());
    }
    Ok(BodyReadOnlyEvidence {
        body_tools,
        list_result,
        mutation_error_categories,
    })
}

fn body_string<'a>(value: &'a Value, pointer: &str, field: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("body scenario omitted {field}"))
}

fn body_scenario_fixture_name() -> String {
    format!("Body parity fixture {BODY_DIAGNOSTIC_SECRET}")
}

fn normalize_body_result(value: &Value) -> Value {
    fn normalized(value: &Value, field: Option<&str>) -> Value {
        match value {
            Value::Object(object) => {
                let unsupported_content = field == Some("content")
                    && object.get("kind").and_then(Value::as_str) == Some("unsupported");
                Value::Object(
                    object
                        .iter()
                        .map(|(key, value)| {
                            let normalized =
                                if unsupported_content && key == "approx_bytes" && value.is_u64() {
                                    json!(0)
                                } else {
                                    normalized(value, Some(key))
                                };
                            (key.clone(), normalized)
                        })
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(
                values
                    .iter()
                    .map(|value| normalized(value, field))
                    .collect(),
            ),
            Value::String(_) => match field {
                Some(
                    "id" | "space_id" | "object_id" | "root_id" | "parent_id" | "block_id"
                    | "target_object_id",
                ) => {
                    json!("<id>")
                }
                Some("snapshot_hash" | "final_snapshot_hash") => json!("<snapshot-hash>"),
                Some("next_cursor") => json!("<cursor>"),
                _ => value.clone(),
            },
            _ => value.clone(),
        }
    }
    normalized(value, None)
}

#[test]
fn body_result_normalization_limits_opaque_byte_volatility_to_unsupported_content() {
    assert_eq!(
        body_scenario_fixture_name(),
        format!("Body parity fixture {BODY_DIAGNOSTIC_SECRET}")
    );
    let result = |approx_bytes, kind, opaque_kind, child_count| {
        json!({
            "space_id":"space-generated",
            "object_id":"object-generated",
            "root_id":"root-generated",
            "snapshot_hash":"snapshot-generated",
            "next_cursor":"cursor-generated",
            "items":[{
                "id":"root-generated",
                "parent_id":"parent-generated",
                "sibling_index":0,
                "depth":0,
                "child_count":child_count,
                "restrictions":{
                    "read":false,
                    "edit":false,
                    "remove":false,
                    "drag":false,
                    "drop_on":false
                },
                "align":"left",
                "vertical_align":"top",
                "background_color":null,
                "content":{
                    "kind":kind,
                    "opaque_kind":opaque_kind,
                    "child_count":child_count,
                    "approx_bytes":approx_bytes
                }
            }]
        })
    };
    let baseline = normalize_body_result(&result(917, "unsupported", "page", 1));
    assert_eq!(
        baseline,
        normalize_body_result(&result(991, "unsupported", "page", 1))
    );
    assert_ne!(
        baseline,
        normalize_body_result(&result(917, "unsupported", "layout", 1))
    );
    assert_ne!(
        baseline,
        normalize_body_result(&result(917, "unsupported", "page", 2))
    );
    assert_ne!(
        baseline,
        normalize_body_result(&result(917, "file", "page", 1))
    );

    let typed = |approx_bytes| {
        json!({
            "content":{
                "kind":"file",
                "approx_bytes":approx_bytes,
                "target_object_id":"target-generated",
                "file_kind":"file",
                "mime":"application/octet-stream",
                "size":4,
                "state":"done",
                "style":"link"
            }
        })
    };
    assert_ne!(
        normalize_body_result(&typed(917)),
        normalize_body_result(&typed(991))
    );
    assert_eq!(baseline["space_id"], "<id>");
    assert_eq!(baseline["object_id"], "<id>");
    assert_eq!(baseline["root_id"], "<id>");
    assert_eq!(baseline["snapshot_hash"], "<snapshot-hash>");
    assert_eq!(baseline["next_cursor"], "<cursor>");

    let mut inner_child_drift = result(917, "unsupported", "page", 1);
    inner_child_drift["items"][0]["content"]["child_count"] = json!(2);
    assert_ne!(baseline, normalize_body_result(&inner_child_drift));

    for approx_bytes in [Value::String("917".to_owned()), json!(-1)] {
        let mut malformed = result(917, "unsupported", "page", 1);
        malformed["items"][0]["content"]["approx_bytes"] = approx_bytes;
        assert_ne!(baseline, normalize_body_result(&malformed));
    }
    let mut missing = result(917, "unsupported", "page", 1);
    missing["items"][0]["content"]
        .as_object_mut()
        .expect("unsupported content")
        .remove("approx_bytes");
    assert_ne!(baseline, normalize_body_result(&missing));

    let unrelated = |approx_bytes| {
        json!({
            "wrapper":{
                "kind":"unsupported",
                "opaque_kind":"page",
                "child_count":1,
                "approx_bytes":approx_bytes
            }
        })
    };
    assert_ne!(
        normalize_body_result(&unrelated(917)),
        normalize_body_result(&unrelated(991))
    );
    assert_ne!(
        normalize_body_result(&json!({"content":{"kind":"text","text":"stable"}})),
        normalize_body_result(&json!({"content":{"kind":"text","text":"preview"}}))
    );
}

fn rich_applied_ids(result: &Value, local_keys: &[&str]) -> Result<Vec<String>, String> {
    let applied = result["applied"]
        .as_array()
        .ok_or_else(|| "rich result omitted applied receipts".to_owned())?;
    if applied.len() != local_keys.len() {
        return Err("rich result returned the wrong applied-receipt count".to_owned());
    }
    applied
        .iter()
        .zip(local_keys)
        .enumerate()
        .map(|(index, (receipt, expected_key))| {
            if receipt["index"].as_u64() != Some(index as u64)
                || receipt["local_key"].as_str() != Some(expected_key)
            {
                return Err("rich result reordered applied receipts".to_owned());
            }
            body_string(receipt, "/block_id", "rich block ID").map(str::to_owned)
        })
        .collect()
}

fn rich_root_contains_exact_suffix(snapshot: &BodySnapshot, ids: &[String]) -> bool {
    let children = snapshot
        .children(&snapshot.root_id)
        .iter()
        .map(|id| id.as_str())
        .collect::<Vec<_>>();
    let expected = ids.iter().map(String::as_str).collect::<Vec<_>>();
    children.ends_with(&expected)
}

fn verify_table_shape(
    snapshot: &BodySnapshot,
    table_id: &str,
    rows: usize,
    columns: usize,
    header_row: bool,
) -> bool {
    let Some(table) = snapshot.iter().find(|block| block.id.as_str() == table_id) else {
        return false;
    };
    if !matches!(table.content, BlockContent::Table) || table.children.len() != 2 {
        return false;
    }
    let Some(column_region) = snapshot.get(&table.children[0]) else {
        return false;
    };
    let Some(row_region) = snapshot.get(&table.children[1]) else {
        return false;
    };
    let materialized_cells = if header_row { columns } else { 0 };
    let Some(expected_subtree_count) = materialized_cells
        .checked_add(rows)
        .and_then(|value| value.checked_add(columns))
        .and_then(|value| value.checked_add(3))
    else {
        return false;
    };
    let mut subtree_count = 0usize;
    let mut stack = vec![table];
    while let Some(block) = stack.pop() {
        subtree_count = subtree_count.saturating_add(1);
        if subtree_count > expected_subtree_count {
            return false;
        }
        for child in block.children.iter().rev() {
            let Some(child) = snapshot.get(child) else {
                return false;
            };
            stack.push(child);
        }
    }
    matches!(
        column_region.content,
        BlockContent::Layout(LayoutStyle::TableColumns)
    ) && column_region.children.len() == columns
        && column_region.children.iter().all(|id| {
            snapshot.get(id).is_some_and(|block| {
                matches!(block.content, BlockContent::TableColumn) && block.children.is_empty()
            })
        })
        && matches!(
            row_region.content,
            BlockContent::Layout(LayoutStyle::TableRows)
        )
        && row_region.children.len() == rows
        && row_region.children.iter().enumerate().all(|(index, id)| {
            snapshot.get(id).is_some_and(|block| {
                matches!(
                    block.content,
                    BlockContent::TableRow { is_header }
                        if is_header == (header_row && index == 0)
                ) && block.children.len() == if header_row && index == 0 { columns } else { 0 }
                    && block.children.iter().all(|cell_id| {
                        snapshot
                            .get(cell_id)
                            .is_some_and(canonical_empty_table_cell)
                    })
            })
        })
        && subtree_count == expected_subtree_count
}

fn canonical_empty_table_cell(block: &BodyBlock) -> bool {
    matches!(
        &block.content,
        BlockContent::Text(text)
            if text.text.is_empty()
                && text.style == TextStyle::Paragraph
                && !text.checked
                && text.color.is_none()
                && text.icon.is_none()
                && text.marks.is_empty()
    ) && block.children.is_empty()
        && block.align == HorizontalAlign::Left
        && block.vertical_align == VerticalAlign::Top
        && block
            .background_color
            .as_ref()
            .is_some_and(|color| color.as_str() == "grey")
        && block.restrictions == BlockRestrictions::default()
}

#[test]
fn independent_table_shape_rejects_every_noncanonical_cell_fixture() {
    use anytype::body::test_fixtures::{
        TableFixtureDefect, table_snapshot, table_snapshot_with_header,
    };

    let verify = |defect| {
        let snapshot = table_snapshot(defect).expect("valid table fixture graph");
        let table_id = snapshot
            .iter()
            .find(|block| matches!(block.content, BlockContent::Table))
            .expect("table fixture root")
            .id
            .to_string();
        verify_table_shape(&snapshot, &table_id, 2, 2, true)
    };
    assert!(verify(TableFixtureDefect::None));
    for defect in [
        TableFixtureDefect::MissingCell,
        TableFixtureDefect::ExtraCell,
        TableFixtureDefect::WrongCellType,
        TableFixtureDefect::NonemptyCell,
        TableFixtureDefect::WrongCellPresentation,
        TableFixtureDefect::WrongCellBackground,
        TableFixtureDefect::CellWithChild,
        TableFixtureDefect::ReversedRegions,
    ] {
        assert!(!verify(defect), "accepted malformed fixture: {defect:?}");
    }
    let no_header =
        table_snapshot_with_header(false, TableFixtureDefect::None).expect("no-header fixture");
    assert!(verify_table_shape(&no_header, "table", 2, 2, false));
}

fn verify_primary_rich_snapshot(snapshot: &BodySnapshot, ids: &[String], target: &str) -> bool {
    let expected_text = [
        (TextStyle::Paragraph, "Paragraph", false),
        (TextStyle::Header1, "Heading 1", false),
        (TextStyle::Header2, "Heading 2", false),
        (TextStyle::Header3, "Heading 3", false),
        (TextStyle::Quote, "Quote", false),
        (TextStyle::Code, "let answer = 42;", false),
        (TextStyle::Bulleted, "Bulleted", false),
        (TextStyle::Numbered, "Numbered", false),
        (TextStyle::Checkbox, "Checked", true),
        (TextStyle::Toggle, "Toggle", false),
        (TextStyle::Callout, "Callout", false),
    ];
    if ids.len() != 16 || !rich_root_contains_exact_suffix(snapshot, ids) {
        return false;
    }
    for (index, (style, text, checked)) in expected_text.into_iter().enumerate() {
        let Some(block) = snapshot
            .iter()
            .find(|block| block.id.as_str() == ids[index])
        else {
            return false;
        };
        let BlockContent::Text(content) = &block.content else {
            return false;
        };
        if content.style != style || content.text != text || content.checked != checked {
            return false;
        }
        if index == 0
            && (content.color.as_ref().map(|color| color.as_str()) != Some("blue")
                || block.align != HorizontalAlign::Center
                || block.vertical_align != VerticalAlign::Middle
                || block.background_color.as_ref().map(|color| color.as_str()) != Some("grey"))
        {
            return false;
        }
        if index == 10
            && !matches!(content.icon, Some(CalloutIcon::Emoji(ref emoji)) if emoji == "💡")
        {
            return false;
        }
    }
    let by_id = |index: usize| {
        snapshot
            .iter()
            .find(|block| block.id.as_str() == ids[index])
            .map(|block| &block.content)
    };
    matches!(by_id(11), Some(BlockContent::Divider(DividerStyle::Dots)))
        && matches!(by_id(12), Some(BlockContent::Relation(relation)) if relation.key == "tag")
        && snapshot
            .iter()
            .find(|block| block.id.as_str() == ids[12])
            .is_some_and(|block| block.align == HorizontalAlign::Justify)
        && matches!(
            by_id(13),
            Some(BlockContent::Link(link))
                if link.target_object_id == target
                    && link.card_style == LinkCardStyle::Card
                    && link.icon_size == LinkIconSize::Small
                    && link.description == LinkDescriptionMode::Content
                    && link.relations == ["tag"]
        )
        && matches!(
            by_id(14),
            Some(BlockContent::Embed(embed))
                if embed.processor == EmbedProcessor::Mermaid
                    && embed.text == "graph TD; A-->B"
        )
        && snapshot
            .iter()
            .find(|block| block.id.as_str() == ids[13])
            .is_some_and(|block| {
                block.align == HorizontalAlign::Right
                    && block.vertical_align == VerticalAlign::Bottom
                    && block.background_color.as_ref().map(|color| color.as_str()) == Some("yellow")
            })
        && verify_table_shape(snapshot, &ids[15], 2, 3, true)
}

fn verify_supplemental_rich_snapshot(
    snapshot: &BodySnapshot,
    ids: &[String],
    target: &str,
) -> bool {
    if ids.len() != 7 || !rich_root_contains_exact_suffix(snapshot, ids) {
        return false;
    }
    let by_id = |index: usize| {
        snapshot
            .iter()
            .find(|block| block.id.as_str() == ids[index])
            .map(|block| &block.content)
    };
    matches!(by_id(0), Some(BlockContent::Divider(DividerStyle::Line)))
        && matches!(
            by_id(1),
            Some(BlockContent::Embed(embed))
                if embed.processor == EmbedProcessor::Latex && embed.text == "x^2 + y^2"
        )
        && matches!(
            by_id(2),
            Some(BlockContent::Embed(embed))
                if embed.processor == EmbedProcessor::Youtube
                    && embed.text == "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        )
        && matches!(by_id(3), Some(BlockContent::TableOfContents))
        && matches!(
            by_id(4),
            Some(BlockContent::Link(link))
                if link.target_object_id == target
                    && link.card_style == LinkCardStyle::Text
                    && link.icon_size == LinkIconSize::None
                    && link.description == LinkDescriptionMode::None
                    && link.relations.is_empty()
        )
        && matches!(
            by_id(5),
            Some(BlockContent::Link(link))
                if link.target_object_id == target
                    && link.card_style == LinkCardStyle::Inline
                    && link.icon_size == LinkIconSize::Medium
                    && link.description == LinkDescriptionMode::Added
                    && link.relations == ["tag"]
        )
        && verify_table_shape(snapshot, &ids[6], 1, 1, false)
}

/// Runs ordinary body behavior through any direct or protocol driver while an
/// independent `anytype-api` client owns and verifies every live fixture.
///
/// The erased, heap-owned return type keeps the complete workflow state out of
/// the caller's async frame so the acceptance suite fits the default test
/// thread stack.
pub fn run_body_scenario<'a>(
    driver: &'a mut impl McpDriver,
    ctx: &'a TestContext,
    transport: &'a str,
) -> BodyScenarioFuture<'a> {
    Box::pin(run_body_scenario_inner(driver, ctx, transport))
}

async fn run_body_scenario_inner(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    transport: &str,
) -> Result<BodyScenarioEvidence, BodyScenarioFailure> {
    let mut stage = BodyScenarioStage::Fixture;
    let outcome: Result<BodyScenarioEvidence, String> = async {
    let mut normalized_results = Vec::new();
    let suffix = unique_suffix();
    let page = ctx
        .client
        .new_object(&ctx.space_id, "page")
        .name(body_scenario_fixture_name())
        .create()
        .await
        .map_err(|_| "body fixture page create failed".to_owned())?;
    ctx.register_object(&page.id);
    let initial = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &page.id)
        .fetch()
        .await
        .map_err(|_| "body initial fixture read failed".to_owned())?;
    let initial_blocks = initial.iter().cloned().collect::<Vec<_>>();
    let append_count = BODY_PAGINATION_ITEM_COUNT
        .checked_sub(initial_blocks.len())
        .filter(|count| *count > 0)
        .ok_or_else(|| "body initial fixture already contains twenty or more blocks".to_owned())?;
    let expected_suffix = body_pagination_suffix_spec(append_count);
    let fixture_operations = body_pagination_append_operations(append_count)?;
    let operation_count = fixture_operations.len();
    let fixture_outcome = initial
        .edit(&ctx.client)
        .apply_all(fixture_operations)
        .await
        .map_err(|_| "body deterministic fixture batch failed".to_owned())?;
    if fixture_outcome.failed.is_some()
        || !fixture_outcome.not_attempted.is_empty()
        || fixture_outcome.applied.len() != operation_count
    {
        return Err("body deterministic fixture batch did not complete".to_owned());
    }
    let mut created_ids = Vec::with_capacity(operation_count);
    for (receipt, (expected_style, expected_text)) in
        fixture_outcome.applied.iter().zip(&expected_suffix)
    {
        let Some(affected) = receipt.affected.first() else {
            return Err("body fixture append receipt omitted the created block".to_owned());
        };
        let address_ok = affected.space_id == ctx.space_id && affected.object_id == page.id;
        let receipt_block = receipt.snapshot.get(&affected.block_id);
        let block_present = receipt_block.is_some();
        let content_ok = receipt_block.is_some_and(|block| {
            matches!(
                &block.content,
                BlockContent::Text(content)
                    if content.style == *expected_style && content.text == *expected_text
            )
        });
        let root_last_ok = receipt.snapshot.root().children.last() == Some(&affected.block_id);
        let receipt_is_exact = receipt.affected.len() == 1
            && address_ok
            && block_present
            && content_ok
            && root_last_ok;
        if !receipt_is_exact {
            return Err("body fixture append receipt did not prove the exact suffix".to_owned());
        }
        created_ids.push(affected.block_id.as_str().to_owned());
    }
    if created_ids.len() != expected_suffix.len() {
        return Err("body fixture append receipts did not cover the exact suffix".to_owned());
    }
    let heading_id = created_ids
        .first()
        .cloned()
        .ok_or_else(|| "body fixture omitted its created heading receipt".to_owned())?;
    let fixture = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &page.id)
        .fetch()
        .await
        .map_err(|_| "body deterministic fixture read failed".to_owned())?;
    if !is_exact_body_pagination_fixture(&fixture, &initial_blocks, &created_ids, &expected_suffix)
    {
        return Err(
            "body deterministic fixture did not contain the exact ordered blocks".to_owned(),
        );
    }

    stage = BodyScenarioStage::Pagination;
    let tools = driver.list_tools().await?;
    for name in [
        "body_block_list",
        "body_block_create",
        "body_block_update",
        "body_block_delete",
        "body_block_move",
        "rich_page_create",
    ] {
        if !tools.iter().any(|candidate| candidate == name) {
            return Err(format!("{transport} catalog omitted {name}"));
        }
    }

    let first = driver
        .call_tool(
            "body_block_list",
            json!({"space":ctx.space_id,"object_id":page.id,"limit":8}),
        )
        .await?;
    normalized_results.push(normalize_body_result(&first));
    let root_id = body_string(&first, "/root_id", "root ID")?.to_owned();
    body_string(&first, "/snapshot_hash", "snapshot hash")?;
    let cursor = body_string(&first, "/next_cursor", "continuation cursor")?.to_owned();
    let mut listed_block_ids = first["items"]
        .as_array()
        .ok_or_else(|| "body first page omitted items".to_owned())?
        .iter()
        .map(|item| body_string(item, "/id", "listed block ID").map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    if listed_block_ids.len() != 8 {
        return Err("body first page did not contain the exact limit of eight".to_owned());
    }
    let second = driver
        .call_tool(
            "body_block_list",
            json!({
                "space":ctx.space_id,"object_id":page.id,"limit":8,"cursor":cursor
            }),
        )
        .await?;
    normalized_results.push(normalize_body_result(&second));
    if second["snapshot_hash"] != first["snapshot_hash"] {
        return Err("body pages mixed snapshot hashes".to_owned());
    }
    let second_cursor =
        body_string(&second, "/next_cursor", "second continuation cursor")?.to_owned();
    listed_block_ids.extend(
        second["items"]
            .as_array()
            .ok_or_else(|| "body second page omitted items".to_owned())?
            .iter()
            .map(|item| body_string(item, "/id", "listed block ID").map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?,
    );
    if listed_block_ids.len() != 16 {
        return Err("body second page did not consume the next eight blocks".to_owned());
    }
    let third = driver
        .call_tool(
            "body_block_list",
            json!({
                "space":ctx.space_id,"object_id":page.id,"limit":8,"cursor":second_cursor
            }),
        )
        .await?;
    normalized_results.push(normalize_body_result(&third));
    if third["snapshot_hash"] != first["snapshot_hash"] {
        return Err("body pages mixed snapshot hashes".to_owned());
    }
    let third_ids = third["items"]
        .as_array()
        .ok_or_else(|| "body third page omitted items".to_owned())?
        .iter()
        .map(|item| body_string(item, "/id", "listed block ID").map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    if third_ids.len() != BODY_PAGINATION_ITEM_COUNT - 16 {
        return Err("body third page did not contain the final four blocks".to_owned());
    }
    listed_block_ids.extend(third_ids);
    if listed_block_ids.len() != BODY_PAGINATION_ITEM_COUNT {
        return Err("body pagination did not contain exactly twenty blocks".to_owned());
    }
    if third.get("next_cursor").is_some() {
        return Err("body three-page fixture unexpectedly returned a fourth cursor".to_owned());
    }
    let independent = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &page.id)
        .fetch()
        .await
        .map_err(|_| "independent body read failed".to_owned())?;
    let independent_ids = independent
        .iter()
        .map(|block| block.id.as_str().to_owned())
        .collect::<Vec<_>>();
    if listed_block_ids != independent_ids {
        return Err("body pages did not preserve exact DFS order".to_owned());
    }

    stage = BodyScenarioStage::StaleCursor;
    let stale_first = driver
        .call_tool(
            "body_block_list",
            json!({"space":ctx.space_id,"object_id":page.id,"limit":8}),
        )
        .await?;
    let stale_cursor = body_string(&stale_first, "/next_cursor", "stale cursor")?.to_owned();
    independent
        .edit(&ctx.client)
        .create(
            NewBlock::paragraph("independent revision")
                .map_err(|_| "independent revision constructor failed".to_owned())?,
            &independent.root_id,
            InsertPosition::LastChild,
        )
        .await
        .map_err(|_| "independent revision write failed".to_owned())?;
    let stale_error = driver
        .call_tool_error(
            "body_block_list",
            json!({
                "space":ctx.space_id,"object_id":page.id,"limit":8,"cursor":stale_cursor
            }),
        )
        .await?;
    if stale_error.code() != "conflict" {
        return Err("body continuation did not reject revision drift".to_owned());
    }
    normalized_results.push(stale_error.normalized_result().clone());

    stage = BodyScenarioStage::Primitive;
    let fresh = driver
        .call_tool(
            "body_block_list",
            json!({"space":ctx.space_id,"object_id":page.id,"limit":8}),
        )
        .await?;
    normalized_results.push(normalize_body_result(&fresh));
    let mut snapshot_hash = body_string(&fresh, "/snapshot_hash", "fresh hash")?.to_owned();
    let create_input = json!({
        "space":ctx.space_id,"object_id":page.id,
        "expected_snapshot_hash":snapshot_hash,"target_block_id":root_id,
        "position":"last_child",
        "block":{"kind":"text","style":"paragraph","text":"created block","marks":[]},
        "idempotency_key":format!("body-{transport}-{suffix}")
    });
    let created = call_body_tool_with_metrics(
        driver,
        "body_block_create",
        create_input.clone(),
        BodyMetricExpectation::PrimitiveMutation,
        "primitive create",
    )
    .await?;
    normalized_results.push(normalize_body_result(&created));
    let created_block_id = body_string(&created, "/block/id", "created block ID")?.to_owned();
    let replay = call_body_tool_with_metrics(
        driver,
        "body_block_create",
        create_input,
        BodyMetricExpectation::Exact(expected_create_replay_metrics()),
        "primitive create replay",
    )
    .await?;
    normalized_results.push(normalize_body_result(&replay));
    let replay_id_matches = replay["block"]["id"] == created["block"]["id"];
    let replay_key_reused = replay["idempotency"]["key_reused"] == true;
    if !replay_id_matches || !replay_key_reused {
        return Err("body create replay did not retain one assigned ID".to_owned());
    }
    snapshot_hash = body_string(&replay, "/snapshot_hash", "replay hash")?.to_owned();

    let child = call_body_tool_with_metrics(
        driver,
        "body_block_create",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"target_block_id":heading_id,
            "position":"last_child",
            "block":{"kind":"text","style":"paragraph","text":"targeted child","marks":[]},
            "idempotency_key":format!("body-child-{transport}-{suffix}")
        }),
        BodyMetricExpectation::PrimitiveMutation,
        "heading append",
    )
    .await?;
    normalized_results.push(normalize_body_result(&child));
    let child_id = body_string(&child, "/block/id", "child ID")?.to_owned();
    snapshot_hash = body_string(&child, "/snapshot_hash", "child hash")?.to_owned();
    let heading_snapshot = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &page.id)
        .fetch()
        .await
        .map_err(|_| "independent heading append read failed".to_owned())?;
    let appended_under_heading = heading_snapshot
        .iter()
        .find(|block| block.id.as_str() == heading_id)
        .is_some_and(|heading| {
            heading
                .children
                .iter()
                .any(|child| child.as_str() == child_id)
        });
    if !appended_under_heading {
        return Err("targeted append did not land beneath the existing heading".to_owned());
    }
    let moved = call_body_tool_with_metrics(
        driver,
        "body_block_move",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"block_id":child_id,
            "target_block_id":created_block_id,"position":"after"
        }),
        BodyMetricExpectation::PrimitiveMutation,
        "primitive move",
    )
    .await?;
    normalized_results.push(normalize_body_result(&moved));
    snapshot_hash = body_string(&moved, "/snapshot_hash", "move hash")?.to_owned();
    let deleted = call_body_tool_with_metrics(
        driver,
        "body_block_delete",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"block_id":child_id,
            "expected_subtree_blocks":1,"confirm_delete":"delete_subtree"
        }),
        BodyMetricExpectation::PrimitiveMutation,
        "primitive delete",
    )
    .await?;
    normalized_results.push(normalize_body_result(&deleted));
    snapshot_hash = body_string(&deleted, "/snapshot_hash", "delete hash")?.to_owned();
    let relation = call_body_tool_with_metrics(
        driver,
        "body_block_create",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"target_block_id":root_id,
            "position":"last_child","block":{"kind":"relation","key":"tag"},
            "idempotency_key":format!("body-relation-{transport}-{suffix}")
        }),
        BodyMetricExpectation::PrimitiveMutation,
        "relation create",
    )
    .await?;
    normalized_results.push(normalize_body_result(&relation));
    let relation_id = body_string(&relation, "/block/id", "relation block ID")?.to_owned();
    snapshot_hash = body_string(&relation, "/snapshot_hash", "relation hash")?.to_owned();
    let relation_snapshot = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &page.id)
        .fetch()
        .await
        .map_err(|_| "independent relation detection read failed".to_owned())?;
    let relation_detected = relation_snapshot.iter().any(|block| {
        block.id.as_str() == relation_id
            && matches!(
                block.content,
                BlockContent::Relation(ref relation) if relation.key == "tag"
            )
    });
    if !relation_detected {
        return Err("created relation block was not independently detected".to_owned());
    }
    let relation_deleted = call_body_tool_with_metrics(
        driver,
        "body_block_delete",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"block_id":relation_id,
            "expected_subtree_blocks":1,"confirm_delete":"delete_subtree"
        }),
        BodyMetricExpectation::PrimitiveMutation,
        "relation delete",
    )
    .await?;
    normalized_results.push(normalize_body_result(&relation_deleted));
    snapshot_hash =
        body_string(&relation_deleted, "/snapshot_hash", "relation delete hash")?.to_owned();
    let recreated_relation = call_body_tool_with_metrics(
        driver,
        "body_block_create",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"target_block_id":root_id,
            "position":"last_child","block":{"kind":"relation","key":"tag"},
            "idempotency_key":format!("body-relation-recreate-{transport}-{suffix}")
        }),
        BodyMetricExpectation::PrimitiveMutation,
        "relation recreate",
    )
    .await?;
    normalized_results.push(normalize_body_result(&recreated_relation));
    let recreated_relation_id =
        body_string(&recreated_relation, "/block/id", "recreated relation ID")?.to_owned();
    snapshot_hash = body_string(
        &recreated_relation,
        "/snapshot_hash",
        "recreated relation hash",
    )?
    .to_owned();
    let relation_moved = call_body_tool_with_metrics(
        driver,
        "body_block_move",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"block_id":recreated_relation_id,
            "target_block_id":heading_id,"position":"before"
        }),
        BodyMetricExpectation::PrimitiveMutation,
        "relation move",
    )
    .await?;
    normalized_results.push(normalize_body_result(&relation_moved));
    let moved_relation_snapshot = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &page.id)
        .fetch()
        .await
        .map_err(|_| "independent moved relation read failed".to_owned())?;
    let root_children = moved_relation_snapshot.children(&moved_relation_snapshot.root_id);
    let adjacent = root_children
        .windows(2)
        .any(|pair| pair[0].as_str() == recreated_relation_id && pair[1].as_str() == heading_id);
    let recreated_relation_detected = moved_relation_snapshot.iter().any(|block| {
        block.id.as_str() == recreated_relation_id
            && matches!(
                    block.content,
                    BlockContent::Relation(ref relation) if relation.key == "tag"
            )
    });
    if !adjacent || !recreated_relation_detected {
        return Err("relation recreation/move was not independently verified".to_owned());
    }
        stage = BodyScenarioStage::RichPrimaryCreate;
    let rich_input = json!({
        "space":ctx.space_id,"name":format!("Rich {transport} {suffix}"),
        "idempotency_key":format!("rich-{transport}-{suffix}"),
        "blocks":[
            {"local_key":"paragraph","block":{"kind":"text","style":"paragraph","text":"Paragraph","marks":[],"text_color":"blue","horizontal_align":"center","vertical_align":"middle","background_color":"grey"}},
            {"local_key":"heading1","block":{"kind":"text","style":"heading_1","text":"Heading 1","marks":[]}},
            {"local_key":"heading2","block":{"kind":"text","style":"heading_2","text":"Heading 2","marks":[]}},
            {"local_key":"heading3","block":{"kind":"text","style":"heading_3","text":"Heading 3","marks":[]}},
            {"local_key":"quote","block":{"kind":"text","style":"quote","text":"Quote","marks":[]}},
            {"local_key":"code","block":{"kind":"text","style":"code","text":"let answer = 42;","marks":[]}},
            {"local_key":"bulleted","block":{"kind":"text","style":"bulleted","text":"Bulleted","marks":[]}},
            {"local_key":"numbered","block":{"kind":"text","style":"numbered","text":"Numbered","marks":[]}},
            {"local_key":"checkbox","block":{"kind":"text","style":"checkbox","text":"Checked","checked":true,"marks":[]}},
            {"local_key":"toggle","block":{"kind":"text","style":"toggle","text":"Toggle","marks":[]}},
            {"local_key":"callout","block":{"kind":"text","style":"callout","text":"Callout","icon":{"kind":"emoji","emoji":"💡"},"marks":[]}},
            {"local_key":"divider","block":{"kind":"divider","style":"dots"}},
            {"local_key":"relation","block":{"kind":"relation","key":"tag","horizontal_align":"justify"}},
            {"local_key":"link","block":{"kind":"link","target_object_id":page.id,"card_style":"card","icon_size":"small","description":"content","relations":["tag"],"horizontal_align":"right","vertical_align":"bottom","background_color":"yellow"}},
            {"local_key":"mermaid","block":{"kind":"embed","processor":"mermaid","source":"graph TD; A-->B"}},
            {"local_key":"table","block":{"kind":"table","rows":2,"columns":3,"header_row":true}}
        ]
    });
    let primary_keys = [
        "paragraph",
        "heading1",
        "heading2",
        "heading3",
        "quote",
        "code",
        "bulleted",
        "numbered",
        "checkbox",
        "toggle",
        "callout",
        "divider",
        "relation",
        "link",
        "mermaid",
        "table",
    ];
    let before_rich_metrics = driver.body_acceptance_metrics();
    let rich = driver
        .call_tool("rich_page_create", rich_input.clone())
        .await?;
    if let (Some(before), Some(after)) = (before_rich_metrics, driver.body_acceptance_metrics()) {
        let observed = body_metrics_delta(before, after)?;
        if observed != expected_rich_metrics(1, primary_keys.len()) {
            return Err(format!(
                "primary rich production metrics diverged: {observed:?}"
            ));
        }
    }
    let rich_page_id = body_string(&rich, "/object_id", "rich page ID")?.to_owned();
    ctx.register_object(&rich_page_id);
    normalized_results.push(normalize_body_result(&rich));
    if rich["status"] != "complete" {
        return Err("rich page workflow did not complete".to_owned());
    }
        let primary_ids = rich_applied_ids(&rich, &primary_keys)?;
        stage = BodyScenarioStage::RichPrimaryReadback;
        let rich_snapshot = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &rich_page_id)
        .fetch()
        .await
        .map_err(|_| "independent rich body read failed".to_owned())?;
        if !verify_primary_rich_snapshot(&rich_snapshot, &primary_ids, &page.id) {
            return Err("independent primary rich ObjectShow verification failed".to_owned());
        }
        stage = BodyScenarioStage::RichPrimaryReplay;
        let before_replay_metrics = driver.body_acceptance_metrics();
    let rich_replay = driver.call_tool("rich_page_create", rich_input).await?;
    if let (Some(before), Some(after)) = (before_replay_metrics, driver.body_acceptance_metrics()) {
        let observed = body_metrics_delta(before, after)?;
        if observed != expected_rich_metrics(0, 0) {
            return Err(format!(
                "rich replay production metrics diverged: {observed:?}"
            ));
        }
    }
    let rich_replay_id_matches = rich_replay["object_id"] == rich["object_id"];
    let rich_replay_key_reused = rich_replay["idempotency"]["key_reused"] == true;
    if !rich_replay_id_matches || !rich_replay_key_reused {
        return Err("rich page replay did not retain one exact page".to_owned());
    }
    normalized_results.push(normalize_body_result(&rich_replay));

    stage = BodyScenarioStage::RichUpdates;
    let mut rich_snapshot_hash = body_string(
        &rich_replay,
        "/final_snapshot_hash",
        "rich replay snapshot hash",
    )?
    .to_owned();
    let update_arms = [
        (
            "set_text",
            primary_ids[0].as_str(),
            json!({
                "kind":"set_text","text":"matrix text",
                "marks":[{"kind":"bold","start":0,"end":6}]
            }),
            BodyUpdateExpectation::Text,
        ),
        (
            "set_text_style",
            primary_ids[1].as_str(),
            json!({"kind":"set_text_style","style":"heading_2"}),
            BodyUpdateExpectation::TextStyle,
        ),
        (
            "set_checked",
            primary_ids[8].as_str(),
            json!({"kind":"set_checked","checked":false}),
            BodyUpdateExpectation::Checked(false),
        ),
        (
            "set_text_color",
            primary_ids[0].as_str(),
            json!({"kind":"set_text_color","color":"red"}),
            BodyUpdateExpectation::TextColor(Some("red")),
        ),
        (
            "clear_text_color",
            primary_ids[0].as_str(),
            json!({"kind":"clear_text_color"}),
            BodyUpdateExpectation::TextColor(None),
        ),
        (
            "set_callout_icon",
            primary_ids[10].as_str(),
            json!({"kind":"set_callout_icon","icon":{"kind":"emoji","emoji":"📌"}}),
            BodyUpdateExpectation::CalloutIcon(true),
        ),
        (
            "clear_callout_icon",
            primary_ids[10].as_str(),
            json!({"kind":"clear_callout_icon"}),
            BodyUpdateExpectation::CalloutIcon(false),
        ),
        (
            "set_divider_style",
            primary_ids[11].as_str(),
            json!({"kind":"set_divider_style","style":"line"}),
            BodyUpdateExpectation::DividerStyle,
        ),
        (
            "set_background_color",
            primary_ids[0].as_str(),
            json!({"kind":"set_background_color","color":"green"}),
            BodyUpdateExpectation::BackgroundColor(Some("green")),
        ),
        (
            "clear_background_color",
            primary_ids[0].as_str(),
            json!({"kind":"clear_background_color"}),
            BodyUpdateExpectation::BackgroundColor(None),
        ),
        (
            "set_horizontal_align",
            primary_ids[0].as_str(),
            json!({"kind":"set_horizontal_align","align":"left"}),
            BodyUpdateExpectation::HorizontalAlign,
        ),
        (
            "set_vertical_align",
            primary_ids[0].as_str(),
            json!({"kind":"set_vertical_align","align":"top"}),
            BodyUpdateExpectation::VerticalAlign,
        ),
        (
            "set_embed_source",
            primary_ids[14].as_str(),
            json!({"kind":"set_embed_source","source":"graph LR; Updated-->Verified"}),
            BodyUpdateExpectation::EmbedSource,
        ),
        (
            "set_link_appearance",
            primary_ids[13].as_str(),
            json!({
                "kind":"set_link_appearance","card_style":"inline","icon_size":"medium",
                "description":"added","relations":[]
            }),
            BodyUpdateExpectation::LinkAppearance,
        ),
    ];
    if update_arms.len() != 14 {
        return Err("body update matrix did not own exactly fourteen arms".to_owned());
    }
    for (label, block_id, change, expectation) in update_arms {
        let (evidence, next_hash) = run_body_update_arm(
            driver,
            ctx,
            &rich_page_id,
            &rich_snapshot_hash,
            block_id,
            change,
            expectation,
            label,
        )
        .await?;
        normalized_results.push(evidence);
        rich_snapshot_hash = next_hash;
    }

    stage = BodyScenarioStage::RichSupplemental;
    let supplemental_input = json!({
        "space":ctx.space_id,"name":format!("Rich variants {transport} {suffix}"),
        "idempotency_key":format!("rich-variants-{transport}-{suffix}"),
        "blocks":[
            {"local_key":"line","block":{"kind":"divider","style":"line"}},
            {"local_key":"latex","block":{"kind":"embed","processor":"latex","source":"x^2 + y^2"}},
            {"local_key":"youtube","block":{"kind":"embed","processor":"youtube","source":"dQw4w9WgXcQ"}},
            {"local_key":"toc","block":{"kind":"table_of_contents"}},
            {"local_key":"link_text","block":{"kind":"link","target_object_id":page.id,"card_style":"text","icon_size":"none","description":"none","relations":[]}},
            {"local_key":"link_inline","block":{"kind":"link","target_object_id":page.id,"card_style":"inline","icon_size":"medium","description":"added","relations":["tag"]}},
            {"local_key":"table_plain","block":{"kind":"table","rows":1,"columns":1,"header_row":false}}
        ]
    });
    let supplemental_keys = [
        "line",
        "latex",
        "youtube",
        "toc",
        "link_text",
        "link_inline",
        "table_plain",
    ];
    let before_supplemental_metrics = driver.body_acceptance_metrics();
    let supplemental = driver
        .call_tool("rich_page_create", supplemental_input)
        .await?;
    if let (Some(before), Some(after)) = (
        before_supplemental_metrics,
        driver.body_acceptance_metrics(),
    ) {
        let observed = body_metrics_delta(before, after)?;
        if observed != expected_rich_metrics(1, supplemental_keys.len()) {
            return Err(format!(
                "supplemental rich production metrics diverged: {observed:?}"
            ));
        }
    }
    let supplemental_page_id =
        body_string(&supplemental, "/object_id", "supplemental rich page ID")?.to_owned();
    ctx.register_object(&supplemental_page_id);
    normalized_results.push(normalize_body_result(&supplemental));
    if supplemental["status"] != "complete" {
        return Err("supplemental rich workflow did not complete".to_owned());
    }
    let supplemental_ids = rich_applied_ids(&supplemental, &supplemental_keys)?;
    let supplemental_snapshot = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &supplemental_page_id)
        .fetch()
        .await
        .map_err(|_| "independent supplemental rich body read failed".to_owned())?;
    if !verify_supplemental_rich_snapshot(&supplemental_snapshot, &supplemental_ids, &page.id) {
        return Err("independent supplemental rich ObjectShow verification failed".to_owned());
    }

    Ok(BodyScenarioEvidence {
        normalized_results,
        listed_block_count: listed_block_ids.len(),
    })
    }
    .await;
    outcome.map_err(|_| BodyScenarioFailure { stage })
}
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Test-only transport seam used by both direct-router and stdio drivers.
pub trait McpDriver {
    fn body_acceptance_metrics(&self) -> Option<BodyDriverMetrics> {
        None
    }

    fn call_tool<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;

    fn call_tool_error<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolErrorEvidence, String>> + 'a>>;

    fn list_tools<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>>;

    fn list_resources<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;

    fn list_resource_templates<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;

    fn read_resource<'a>(
        &'a mut self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;
}

/// Runs a fixture-heavy live scenario on an isolated test runtime.
pub fn run_live_scenario_on_large_stack<F, Fut>(thread_name: &str, scenario: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + 'static,
{
    std::thread::Builder::new()
        .name(thread_name.to_owned())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build fixture-heavy live scenario runtime")
                .block_on(scenario());
        })
        .expect("spawn fixture-heavy live scenario thread")
        .join()
        .expect("fixture-heavy live scenario thread");
}

/// Content-free result of the representative-layout scenario.
#[derive(Debug, PartialEq, Eq)]
pub struct LayoutScenarioEvidence {
    pub collection_id: String,
    pub grid_view_id: String,
    pub kanban_view_id: String,
    pub moved_item_id: String,
    pub member_ids: Vec<String>,
}

struct PageWalk {
    items: Vec<Value>,
    pages: usize,
}

async fn collect_id_pages(
    driver: &mut impl McpDriver,
    tool: &'static str,
    base: Value,
    id_pointer: &'static str,
    max_pages: usize,
) -> Result<PageWalk, String> {
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    let mut seen_ids = HashSet::new();
    let mut items = Vec::new();
    for page_number in 0..max_pages {
        let mut input = base
            .as_object()
            .cloned()
            .ok_or_else(|| format!("{tool} page input must be an object"))?;
        input.insert("limit".to_owned(), json!(1));
        if let Some(cursor) = cursor.take() {
            input.insert("cursor".to_owned(), Value::String(cursor));
        }
        let page = driver.call_tool(tool, Value::Object(input)).await?;
        let page_items = page["items"]
            .as_array()
            .ok_or_else(|| format!("{tool} items must be an array"))?;
        require(page_items.len() <= 1, &format!("{tool} honors limit one"))?;
        for item in page_items {
            let id = item
                .pointer(id_pointer)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{tool} item omitted its identity"))?;
            require(
                seen_ids.insert(id.to_owned()),
                &format!("{tool} item progress"),
            )?;
            items.push(item.clone());
        }
        let Some(next) = page.get("next_cursor").and_then(Value::as_str) else {
            return Ok(PageWalk {
                items,
                pages: page_number + 1,
            });
        };
        require(
            seen_cursors.insert(next.to_owned()),
            &format!("{tool} cursor progress"),
        )?;
        cursor = Some(next.to_owned());
    }
    Err(format!("{tool} did not terminate within {max_pages} pages"))
}

async fn canonical_members(ctx: &TestContext, collection_id: &str) -> Result<Vec<String>, String> {
    let page = ctx
        .client
        .collection_membership_page(&ctx.space_id, collection_id, 61, None)
        .await
        .map_err(|_| "read independent canonical membership".to_owned())?;
    require(
        page.continuation.is_none(),
        "independent canonical membership terminates",
    )?;
    Ok(page.object_ids)
}

async fn mcp_members(
    driver: &mut impl McpDriver,
    space_id: &str,
    collection_id: &str,
) -> Result<(Vec<String>, usize), String> {
    let walk = collect_id_pages(
        driver,
        "collection_member_list",
        json!({"space":space_id,"collection_id":collection_id}),
        "/object_id",
        16,
    )
    .await?;
    let members = walk
        .items
        .into_iter()
        .map(|item| {
            item["object_id"]
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| "collection_member_list omitted object_id".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((members, walk.pages))
}

/// Exercises ordinary MCP workflows across basic, collection, grid, filtered,
/// and Kanban layouts without introducing a layout-specific protocol surface.
pub fn run_representative_layout_scenario<'a>(
    driver: &'a mut impl McpDriver,
    ctx: &'a TestContext,
) -> Pin<Box<dyn Future<Output = Result<LayoutScenarioEvidence, String>> + 'a>> {
    Box::pin(async move {
        const REQUIRED: [&str; 8] = [
            "collection_member_add",
            "collection_member_list",
            "collection_member_remove",
            "object_get",
            "object_update",
            "type_list",
            "view_list",
            "view_object_list",
        ];
        const FORBIDDEN: [&str; 4] = [
            "kanban_column_move",
            "kanban_get",
            "layout_get",
            "view_filter_set",
        ];
        let tools = driver.list_tools().await?;
        for required in REQUIRED {
            require(
                tools.iter().any(|name| name == required),
                &format!("representative layout scenario requires {required}"),
            )?;
        }
        for forbidden in FORBIDDEN {
            require(
                !tools.iter().any(|name| name == forbidden),
                &format!("layout-specific tool must remain absent: {forbidden}"),
            )?;
        }

        let suffix = unique_suffix();
        let fixture_name = format!("MCP representative layout {suffix}");
        let mut fixture = Box::pin(ctx.create_kanban_fixture(&fixture_name))
            .await
            .map_err(|_| "create cleanup-owned representative Kanban fixture".to_owned())?;
        let first_item = fixture
            .items
            .first()
            .ok_or_else(|| "Kanban fixture omitted its first item".to_owned())?;
        let first_item_id = first_item.object.id.clone();
        let first_item_name = first_item
            .object
            .name
            .clone()
            .ok_or_else(|| "Kanban fixture item omitted its name".to_owned())?;
        let removed_item_id = fixture
            .items
            .get(2)
            .map(|item| item.object.id.clone())
            .ok_or_else(|| "Kanban fixture omitted its third item".to_owned())?;
        let destination_id = fixture
            .columns
            .get(1)
            .map(|column| column.id.clone())
            .ok_or_else(|| "Kanban fixture omitted its destination column".to_owned())?;

        let expected_type_layouts = BTreeMap::from([
            (fixture.item_type.id.clone(), "basic".to_owned()),
            (fixture.collection_type.id.clone(), "collection".to_owned()),
        ]);
        let mut observed_type_layouts = BTreeMap::new();
        let mut observed_type_pages = 0;
        for _ in 0..10 {
            let types = collect_id_pages(
                driver,
                "type_list",
                json!({
                    "space":ctx.space_id,
                    "filters":{
                        "operator":"and",
                        "conditions":[{
                            "format":"text",
                            "property_key":"name",
                            "condition":"contains",
                            "value":fixture_name
                        }]
                    }
                }),
                "/id",
                8,
            )
            .await?;
            observed_type_pages = types.pages;
            observed_type_layouts.clear();
            for item in &types.items {
                if let (Some(id), Some(layout)) = (item["id"].as_str(), item["layout"].as_str())
                    && expected_type_layouts.contains_key(id)
                {
                    observed_type_layouts.insert(id.to_owned(), layout.to_owned());
                }
            }
            if observed_type_layouts == expected_type_layouts && types.pages == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        require(
            observed_type_layouts == expected_type_layouts && observed_type_pages == 2,
            "type_list preserves representative basic and collection layouts",
        )?;

        for (object_id, expected_type_key) in [
            (first_item_id.as_str(), fixture.item_type.key.as_str()),
            (
                fixture.collection.id.as_str(),
                fixture.collection_type.key.as_str(),
            ),
        ] {
            let object = driver
                .call_tool(
                    "object_get",
                    json!({"space":ctx.space_id,"object_id":object_id}),
                )
                .await?;
            require(
                object.pointer("/object/summary/id").and_then(Value::as_str) == Some(object_id)
                    && object
                        .pointer("/object/summary/type_key")
                        .and_then(Value::as_str)
                        == Some(expected_type_key),
                "object_get preserves representative object type identity",
            )?;
        }

        let views = collect_id_pages(
            driver,
            "view_list",
            json!({"space":ctx.space_id,"list_id":fixture.collection.id}),
            "/id",
            16,
        )
        .await?;
        require(
            views.pages >= 2,
            "view_list follows a limit-one continuation",
        )?;
        let view_layouts = views
            .items
            .iter()
            .filter_map(|item| Some((item["id"].as_str()?, item["layout"].as_str()?)))
            .collect::<BTreeMap<_, _>>();
        require(
            view_layouts.get(fixture.view.id.as_str()) == Some(&"kanban"),
            "view_list preserves the Kanban layout",
        )?;
        let grid_view_id = view_layouts
            .iter()
            .find_map(|(id, layout)| (*layout == "grid").then(|| (*id).to_owned()))
            .ok_or_else(|| "view_list omitted the representative grid layout".to_owned())?;

        let canonical_before = canonical_members(ctx, &fixture.collection.id).await?;
        require(
            canonical_before.len() == fixture.items.len()
                && fixture
                    .items
                    .iter()
                    .all(|item| canonical_before.contains(&item.object.id)),
            "independent canonical membership contains every Kanban card",
        )?;
        let (mcp_before, before_pages) =
            mcp_members(driver, &ctx.space_id, &fixture.collection.id).await?;
        require(
            mcp_before == canonical_before && before_pages == canonical_before.len(),
            "collection_member_list matches independent canonical order",
        )?;

        let updated = driver
            .call_tool(
                "object_update",
                json!({
                    "space":ctx.space_id,
                    "object_id":first_item_id,
                    "properties":[{
                        "format":"select",
                        "key":fixture.status_property.key,
                        "select":destination_id
                    }]
                }),
            )
            .await?;
        require(
            updated.pointer("/object/id").and_then(Value::as_str) == Some(first_item_id.as_str()),
            "ordinary object_update preserves moved card identity",
        )?;
        let moved = ctx
            .client
            .object(&ctx.space_id, &first_item_id)
            .get()
            .await
            .map_err(|_| "independently read moved Kanban card".to_owned())?;
        require(
            moved
                .get_property_select(&fixture.status_property.key)
                .map(|tag| tag.id.as_str())
                == Some(destination_id.as_str()),
            "ordinary Select-property update moves the Kanban card",
        )?;
        if let Some(item) = fixture
            .items
            .iter_mut()
            .find(|item| item.object.id == first_item_id)
        {
            item.object = moved;
            item.column_id = Some(destination_id.clone());
        }

        let destination_items = collect_id_pages(
            driver,
            "view_object_list",
            json!({
                "space":ctx.space_id,
                "list_id":fixture.collection.id,
                "view":fixture.view.id,
                "property_keys":[fixture.status_property.key],
                "filters":{
                    "operator":"and",
                    "conditions":[{
                        "format":"select",
                        "property_key":fixture.status_property.key,
                        "condition":"in",
                        "values":[destination_id]
                    }]
                }
            }),
            "/summary/id",
            8,
        )
        .await?;
        let expected_destination_ids = fixture
            .items
            .iter()
            .filter(|item| item.column_id.as_deref() == Some(destination_id.as_str()))
            .map(|item| item.object.id.as_str())
            .collect::<BTreeSet<_>>();
        require(
            destination_items.pages == 2,
            "filtered Kanban view follows its limit-one continuation",
        )?;
        let actual_destination_ids = destination_items
            .items
            .iter()
            .filter_map(|item| item.pointer("/summary/id").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();
        require(
            expected_destination_ids.len() == 2
                && actual_destination_ids == expected_destination_ids,
            "filtered Kanban pagination returns the exact destination column",
        )?;

        let removed = driver
            .call_tool(
                "collection_member_remove",
                json!({
                    "space":ctx.space_id,
                    "collection_id":fixture.collection.id,
                    "object_id":removed_item_id
                }),
            )
            .await?;
        require(
            removed["collection_id"] == fixture.collection.id
                && removed["object_id"] == removed_item_id
                && removed["membership"] == "absent",
            "collection_member_remove returns exact absence evidence",
        )?;
        let after_remove = canonical_members(ctx, &fixture.collection.id).await?;
        let (mcp_after_remove, remove_pages) =
            mcp_members(driver, &ctx.space_id, &fixture.collection.id).await?;
        require(
            !after_remove.contains(&removed_item_id)
                && mcp_after_remove == after_remove
                && remove_pages == after_remove.len(),
            "removed card is absent from canonical membership",
        )?;
        let survived = ctx
            .client
            .object(&ctx.space_id, &removed_item_id)
            .get()
            .await
            .map_err(|_| "removed collection member object must survive".to_owned())?;
        require(
            survived.id == removed_item_id,
            "collection removal does not delete the card",
        )?;

        let added = driver
            .call_tool(
                "collection_member_add",
                json!({
                    "space":ctx.space_id,
                    "collection_id":fixture.collection.id,
                    "object_id":removed_item_id
                }),
            )
            .await?;
        require(
            added["collection_id"] == fixture.collection.id
                && added["object_id"] == removed_item_id
                && added["membership"] == "present",
            "collection_member_add returns exact presence evidence",
        )?;
        let canonical_after = canonical_members(ctx, &fixture.collection.id).await?;
        let (mcp_after_add, add_pages) =
            mcp_members(driver, &ctx.space_id, &fixture.collection.id).await?;
        require(
            canonical_after.contains(&removed_item_id)
                && mcp_after_add == canonical_after
                && add_pages == canonical_after.len(),
            "re-added card returns to canonical paginated membership",
        )?;

        Box::pin(ctx.add_collection_name_filter_fixture(
            &fixture.collection.id,
            &fixture.view.id,
            &first_item_name,
        ))
        .await
        .map_err(|_| "configure cleanup-owned filtered Kanban view".to_owned())?;
        let canonical_filtered = canonical_members(ctx, &fixture.collection.id).await?;
        let (mcp_filtered, filtered_member_pages) =
            mcp_members(driver, &ctx.space_id, &fixture.collection.id).await?;
        require(
            canonical_filtered == canonical_after
                && mcp_filtered == canonical_after
                && filtered_member_pages == canonical_after.len(),
            "saved-view filtering preserves exact canonical membership",
        )?;
        let filtered_after = collect_id_pages(
            driver,
            "view_object_list",
            json!({
                "space":ctx.space_id,
                "list_id":fixture.collection.id,
                "view":fixture.view.id
            }),
            "/summary/id",
            8,
        )
        .await?;
        require(
            filtered_after.pages == 1
                && filtered_after.items.len() == 1
                && filtered_after.items[0]
                    .pointer("/summary/id")
                    .and_then(Value::as_str)
                    == Some(first_item_id.as_str()),
            "membership and column mutations preserve saved-view filtering",
        )?;
        require(
            canonical_filtered.len() > filtered_after.items.len()
                && canonical_filtered.iter().any(|id| id == &first_item_id),
            "canonical membership remains independent of filter visibility",
        )?;

        Ok(LayoutScenarioEvidence {
            collection_id: fixture.collection.id,
            grid_view_id,
            kanban_view_id: fixture.view.id,
            moved_item_id: first_item_id,
            member_ids: canonical_filtered,
        })
    })
}

/// Cleanup-owned identities and unique text for the complete chats scenario.
pub struct ChatsRegistryFixture<'a> {
    pub space_id: &'a str,
    pub chat_id: &'a str,
    pub seed_message_id: &'a str,
    pub search_query: &'a str,
    pub add_text: &'a str,
    pub idempotency_key: &'a str,
}

/// Content-minimized evidence returned by the complete chats scenario.
#[derive(Debug, PartialEq, Eq)]
pub struct ChatsRegistryEvidence {
    pub chat_id: String,
    pub seed_message_id: String,
    pub added_message_id: String,
    pub deleted: bool,
}

fn page_contains_id(page: &Value, id: &str) -> bool {
    page["items"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["id"] == id))
}

fn search_contains_id(page: &Value, id: &str) -> bool {
    page["items"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["message"]["id"] == id))
}

/// Runs all six production chat tools through one transport-neutral driver.
pub async fn run_chats_registry_scenario(
    driver: &mut impl McpDriver,
    fixture: ChatsRegistryFixture<'_>,
) -> Result<ChatsRegistryEvidence, String> {
    const CHAT_NAMES: [&str; 6] = [
        "chat_list",
        "chat_message_add",
        "chat_message_delete",
        "chat_message_get",
        "chat_message_list",
        "chat_message_search",
    ];
    let tools = driver.list_tools().await?;
    let actual = tools
        .iter()
        .filter(|name| name.starts_with("chat_"))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if actual != CHAT_NAMES {
        return Err("chats registry inventory differs from the reviewed six tools".to_owned());
    }

    let chats = driver
        .call_tool("chat_list", json!({"space":fixture.space_id,"limit":20}))
        .await?;
    if !page_contains_id(&chats, fixture.chat_id) {
        return Err("chat_list omitted the cleanup-owned chat".to_owned());
    }
    let history = driver
        .call_tool(
            "chat_message_list",
            json!({"space":fixture.space_id,"chat_id":fixture.chat_id,"limit":12}),
        )
        .await?;
    if !page_contains_id(&history, fixture.seed_message_id) {
        return Err("chat_message_list omitted the cleanup-owned seed".to_owned());
    }
    let mut search_observed = false;
    for attempt in 0..20 {
        let search = driver
            .call_tool(
                "chat_message_search",
                json!({
                    "space":fixture.space_id,
                    "chat_id":fixture.chat_id,
                    "query":fixture.search_query,
                    "limit":12
                }),
            )
            .await?;
        if search_contains_id(&search, fixture.seed_message_id) {
            search_observed = true;
            break;
        }
        if attempt != 19 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    if !search_observed {
        return Err("chat_message_search omitted the cleanup-owned seed".to_owned());
    }

    let add_input = json!({
        "space":fixture.space_id,
        "chat_id":fixture.chat_id,
        "text":fixture.add_text,
        "idempotency_key":fixture.idempotency_key,
    });
    let added = driver
        .call_tool("chat_message_add", add_input.clone())
        .await?;
    let message_id = added["message"]["id"]
        .as_str()
        .ok_or_else(|| "chat_message_add omitted the assigned ID".to_owned())?
        .to_owned();
    let modified_at = added["message"]["modified_at"]
        .as_str()
        .ok_or_else(|| "chat_message_add omitted the canonical timestamp".to_owned())?
        .to_owned();
    if added["message"]["text"] != fixture.add_text || added["idempotency"]["key_reused"] != false {
        return Err("chat_message_add returned incorrect first-call evidence".to_owned());
    }

    let replay = driver
        .call_tool("chat_message_add", add_input.clone())
        .await?;
    if replay["message"]["id"] != message_id || replay["idempotency"]["key_reused"] != true {
        return Err("chat_message_add replay changed identity or missed reuse".to_owned());
    }
    let detail = driver
        .call_tool(
            "chat_message_get",
            json!({
                "space":fixture.space_id,
                "chat_id":fixture.chat_id,
                "message_id":message_id,
            }),
        )
        .await?;
    if detail["message"]["id"] != message_id || detail["message"]["text"] != fixture.add_text {
        return Err("chat_message_get disagreed with verified add".to_owned());
    }

    let conflict = driver
        .call_tool_error(
            "chat_message_add",
            json!({
                "space":fixture.space_id,
                "chat_id":fixture.chat_id,
                "text":format!("{} conflict", fixture.add_text),
                "idempotency_key":fixture.idempotency_key,
            }),
        )
        .await?;
    if conflict.code() != "conflict" {
        return Err("changed chat add replay was not a conflict".to_owned());
    }

    let deleted = driver
        .call_tool(
            "chat_message_delete",
            json!({
                "space":fixture.space_id,
                "chat_id":fixture.chat_id,
                "message_id":message_id,
                "expected_modified_at":modified_at,
                "confirm_delete":"delete_message",
            }),
        )
        .await?;
    if deleted["message_id"] != message_id || deleted["deleted"] != true {
        return Err("chat_message_delete omitted verified absence".to_owned());
    }
    let absence = driver
        .call_tool_error(
            "chat_message_get",
            json!({
                "space":fixture.space_id,
                "chat_id":fixture.chat_id,
                "message_id":message_id,
            }),
        )
        .await?;
    if absence.code() != "not_found" {
        return Err("deleted chat message remained readable".to_owned());
    }

    Ok(ChatsRegistryEvidence {
        chat_id: fixture.chat_id.to_owned(),
        seed_message_id: fixture.seed_message_id.to_owned(),
        added_message_id: message_id,
        deleted: true,
    })
}

/// Stable identifiers for every executable live scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScenarioId {
    Discovery,
    Documents,
    Views,
    Mutations,
    MarkdownNoop,
    Archive,
    #[cfg(test)]
    SyntheticNonExecutable,
}

impl ScenarioId {
    pub const EXECUTABLE: [Self; 6] = [
        Self::Discovery,
        Self::Documents,
        Self::Views,
        Self::Mutations,
        Self::MarkdownNoop,
        Self::Archive,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discovery => "standard_discovery",
            Self::Documents => "standard_documents",
            Self::Views => "standard_views",
            Self::Mutations => "standard_mutations",
            Self::MarkdownNoop => "standard_markdown_noop",
            Self::Archive => "standard_archive",
            #[cfg(test)]
            Self::SyntheticNonExecutable => "synthetic_non_executable",
        }
    }

    pub const fn is_executable(self) -> bool {
        match self {
            Self::Discovery
            | Self::Documents
            | Self::Views
            | Self::Mutations
            | Self::MarkdownNoop
            | Self::Archive => true,
            #[cfg(test)]
            Self::SyntheticNonExecutable => false,
        }
    }
}

/// Bounded non-content evidence accumulated while a scenario builds fixtures.
#[derive(Debug)]
pub struct ScenarioEvidence {
    pub scenario: ScenarioId,
    pub fixture_ids: Vec<String>,
    redactions: Vec<String>,
}

impl ScenarioEvidence {
    pub fn new(scenario: ScenarioId) -> Self {
        Self {
            scenario,
            fixture_ids: Vec::new(),
            redactions: Vec::new(),
        }
    }

    pub fn fixture(&mut self, id: &str) {
        self.fixture_ids.push(id.to_owned());
    }

    pub fn sensitive(&mut self, value: &str) {
        if !value.is_empty() {
            self.redactions.push(value.to_owned());
        }
    }

    pub fn sanitize(&self, value: &str) -> String {
        let mut sanitized = value.to_owned();
        for secret in &self.redactions {
            sanitized = sanitized.replace(secret, "<redacted-content>");
        }
        const MAX_EVIDENCE_CHARS: usize = 16_384;
        sanitized.chars().take(MAX_EVIDENCE_CHARS).collect()
    }
}

/// Inputs owned by the fixture rather than by a transport driver.
pub struct DocumentFixture<'a> {
    pub space_id: &'a str,
    pub object_id: &'a str,
    pub name: &'a str,
    pub initial_body: &'a str,
    pub old_text: &'a str,
    pub new_text: &'a str,
}

/// Observable result used for an independent backend readback assertion.
pub struct DocumentScenarioEvidence {
    pub expected_body: String,
    pub edited_sha256: String,
}

/// Transport-neutral inputs for an exact exported-Markdown replacement.
pub struct MarkdownNoopFixture<'a> {
    pub space_id: &'a str,
    pub object_id: &'a str,
    pub exported_body: &'a str,
}

/// Content-free protocol evidence for an exact Markdown replacement.
#[derive(Debug, PartialEq, Eq)]
pub struct MarkdownNoopProtocolEvidence {
    pub body_sha256: String,
    pub before_bytes: usize,
    pub after_bytes: usize,
}

/// Runs the MCP-only portion of an exported-Markdown no-op workflow.
pub async fn run_markdown_noop_protocol(
    driver: &mut impl McpDriver,
    fixture: MarkdownNoopFixture<'_>,
) -> Result<MarkdownNoopProtocolEvidence, String> {
    let before = driver
        .call_tool(
            "object_get",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "body": {"max_chars": 100_000}
            }),
        )
        .await?;
    require(
        before.pointer("/object/summary/id").and_then(Value::as_str) == Some(fixture.object_id),
        "Markdown no-op pre-read object identity",
    )?;
    require(
        before
            .pointer("/object/summary/space_id")
            .and_then(Value::as_str)
            == Some(fixture.space_id),
        "Markdown no-op pre-read space identity",
    )?;
    let before_body = required_string(&before, "/body/text")?;
    require(
        before_body == fixture.exported_body,
        "Markdown no-op pre-read complete export",
    )?;
    let body_sha256 = required_string(&before, "/body/sha256")?;
    let independent_hash = Sha256::digest(fixture.exported_body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    require(
        body_sha256 == independent_hash,
        "Markdown no-op pre-read hash",
    )?;

    let updated = driver
        .call_tool(
            "object_update",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "body_markdown": fixture.exported_body,
                "expected_body_sha256": body_sha256
            }),
        )
        .await?;
    require(
        updated.pointer("/object/id").and_then(Value::as_str) == Some(fixture.object_id),
        "Markdown no-op update object identity",
    )?;
    require(
        updated.pointer("/object/space_id").and_then(Value::as_str) == Some(fixture.space_id),
        "Markdown no-op update space identity",
    )?;
    require(
        updated.get("body_sha256").and_then(Value::as_str) == Some(body_sha256.as_str()),
        "Markdown no-op update hash",
    )?;

    let after = driver
        .call_tool(
            "object_get",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "body": {"max_chars": 100_000}
            }),
        )
        .await?;
    require(
        after.pointer("/object/summary/id").and_then(Value::as_str) == Some(fixture.object_id),
        "Markdown no-op repeated export object identity",
    )?;
    let after_body = required_string(&after, "/body/text")?;
    require(
        after_body == fixture.exported_body,
        "Markdown no-op repeated export byte identity",
    )?;
    require(
        after.pointer("/body/sha256").and_then(Value::as_str) == Some(body_sha256.as_str()),
        "Markdown no-op repeated export hash",
    )?;
    Ok(MarkdownNoopProtocolEvidence {
        body_sha256,
        before_bytes: before_body.len(),
        after_bytes: after_body.len(),
    })
}

/// Runs the compact document workflow through an arbitrary MCP transport.
pub async fn run_document_scenario(
    driver: &mut impl McpDriver,
    fixture: DocumentFixture<'_>,
) -> Result<DocumentScenarioEvidence, String> {
    let status = driver.call_tool("server_status", json!({})).await?;
    require(
        status["http_available"] == true,
        "server_status HTTP availability",
    )?;

    let mut found = false;
    for _ in 0..10 {
        let search = driver
            .call_tool(
                "object_search",
                json!({
                    "space": fixture.space_id,
                    "text": fixture.name,
                    "limit": 100
                }),
            )
            .await?;
        found = search["items"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item.pointer("/summary/id").and_then(Value::as_str) == Some(fixture.object_id)
            })
        });
        if found {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    require(found, "object_search observes the fixture object")?;

    let object = driver
        .call_tool(
            "object_get",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "body": {"max_chars": 100_000}
            }),
        )
        .await?;
    require(
        object.pointer("/object/summary/id").and_then(Value::as_str) == Some(fixture.object_id),
        "object_get identity",
    )?;
    require(
        object.pointer("/body/text").and_then(Value::as_str) == Some(fixture.initial_body),
        "object_get complete body",
    )?;
    let uri = required_string(&object, "/object/summary/resource_uri")?;

    let resource = driver.read_resource(&uri).await?;
    require(
        resource.pointer("/contents/0/uri").and_then(Value::as_str) == Some(uri.as_str()),
        "resources/read canonical URI",
    )?;
    require(
        resource.pointer("/contents/0/text").and_then(Value::as_str) == Some(fixture.initial_body),
        "resources/read complete body",
    )?;

    // Refresh the optimistic-concurrency token immediately before the edit.
    // A newly created Anytype document can still be converging while the
    // independent resource observation above completes.
    let current = driver
        .call_tool(
            "object_get",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "body": {"max_chars": 100_000}
            }),
        )
        .await?;
    require(
        current.pointer("/body/text").and_then(Value::as_str) == Some(fixture.initial_body),
        "object_get body remains stable before edit",
    )?;
    let body_sha256 = required_string(&current, "/body/sha256")?;
    let independently_hashed = Sha256::digest(fixture.initial_body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    require(
        body_sha256 == independently_hashed,
        "object_get hash matches the complete observed body",
    )?;
    require(
        fixture.initial_body.match_indices(fixture.old_text).count() == 1,
        "fixture body contains exactly one edit match",
    )?;

    let edited = driver
        .call_tool(
            "object_edit",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "edits": [{
                    "old_text": fixture.old_text,
                    "new_text": fixture.new_text,
                    "expected_matches": 1
                }],
                "expected_body_sha256": body_sha256
            }),
        )
        .await?;
    let edited_sha256 = required_string(&edited, "/body_sha256")?;
    let expected_body = fixture
        .initial_body
        .replacen(fixture.old_text, fixture.new_text, 1);
    require(
        expected_body != fixture.initial_body,
        "fixture edit changes exactly one fragment",
    )?;
    Ok(DocumentScenarioEvidence {
        expected_body,
        edited_sha256,
    })
}

/// Heap-owned dispatch future for a complete live scenario.
pub type ScenarioFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + 'a>>;

/// Executes one complete standard-baseline scenario through the selected driver.
///
/// The erased, heap-owned return type is intentional: these fixture-heavy
/// debug futures exceed Tokio's worker-stack budget when their state is kept
/// inline by an ordinary `async fn` dispatcher.
pub fn run_scenario<'a>(
    scenario: ScenarioId,
    driver: &'a mut impl McpDriver,
    ctx: &'a TestContext,
    evidence: &'a mut ScenarioEvidence,
) -> ScenarioFuture<'a> {
    Box::pin(async move {
        match scenario {
            ScenarioId::Discovery => Box::pin(run_discovery(driver, ctx, evidence)).await,
            ScenarioId::Documents => Box::pin(run_documents(driver, ctx, evidence)).await,
            ScenarioId::Views => Box::pin(run_views(driver, ctx, evidence)).await,
            ScenarioId::Mutations => Box::pin(run_mutations(driver, ctx, evidence)).await,
            ScenarioId::MarkdownNoop => Box::pin(run_markdown_noop(driver, ctx, evidence)).await,
            ScenarioId::Archive => Box::pin(run_archive(driver, ctx, evidence)).await,
            #[cfg(test)]
            ScenarioId::SyntheticNonExecutable => {
                Err("scenario is intentionally non-executable".to_owned())
            }
        }
    })
}

async fn run_discovery(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let status = driver.call_tool("server_status", json!({})).await?;
    require(
        status["http_available"] == true,
        "server_status HTTP availability",
    )?;

    let first_space = ctx
        .create_space_fixture(format!("MCP shared space {}", unique_suffix()))
        .await
        .map_err(|_| "create first disposable space fixture".to_owned())?;
    evidence.fixture(&first_space.id);
    let second_space = ctx
        .create_space_fixture(format!("MCP shared space {}", unique_suffix()))
        .await
        .map_err(|_| "create second disposable space fixture".to_owned())?;
    evidence.fixture(&second_space.id);

    let duplicate_name = format!("MCP shared ambiguous {}", unique_suffix());
    evidence.sensitive(&duplicate_name);
    let first_type = ctx
        .client
        .new_type(&ctx.space_id, &duplicate_name)
        .key(format!("mcp_shared_a_{}", unique_suffix()))
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create first type fixture".to_owned())?;
    ctx.register_type(&first_type.id);
    evidence.fixture(&first_type.id);
    let second_type = ctx
        .client
        .new_type(&ctx.space_id, &duplicate_name)
        .key(format!("mcp_shared_b_{}", unique_suffix()))
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create second type fixture".to_owned())?;
    ctx.register_type(&second_type.id);
    evidence.fixture(&second_type.id);

    let property = ctx
        .client
        .new_property(
            &ctx.space_id,
            format!("MCP shared select {}", unique_suffix()),
            PropertyFormat::Select,
        )
        .create()
        .await
        .map_err(|_| "create select property fixture".to_owned())?;
    ctx.register_property(&property.id);
    evidence.fixture(&property.id);
    let mut tag_ids = Vec::new();
    for (name, color) in [("First", Color::Blue), ("Second", Color::Red)] {
        let tag = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name(format!("{name} {}", unique_suffix()))
            .color(color)
            .create()
            .await
            .map_err(|_| "create tag fixture".to_owned())?;
        evidence.fixture(&tag.id);
        tag_ids.push(tag.id);
    }

    let templates = ctx
        .create_template_fixtures(
            format!("MCP shared template type {}", unique_suffix()),
            [
                format!("MCP shared template A {}", unique_suffix()),
                format!("MCP shared template B {}", unique_suffix()),
            ],
        )
        .await
        .map_err(|_| "create template fixtures".to_owned())?;
    evidence.fixture(&templates.type_.id);
    let template_ids = templates
        .templates
        .iter()
        .map(|template| {
            evidence.fixture(&template.id);
            template.id.clone()
        })
        .collect::<Vec<_>>();

    let search_term = format!("McpSharedSearch{}", unique_suffix());
    evidence.sensitive(&search_term);
    let mut object_ids = Vec::new();
    for ordinal in ["first", "second"] {
        let object = create_object(ctx, &format!("{search_term} {ordinal}"), "").await?;
        evidence.fixture(&object.id);
        object_ids.push(object.id);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    walk_pages(
        driver,
        "space_list",
        json!({}),
        &[first_space.id, second_space.id],
        1_000,
    )
    .await?;
    walk_pages(
        driver,
        "type_list",
        json!({"space": ctx.space_id}),
        &[first_type.id.clone(), second_type.id.clone()],
        1_000,
    )
    .await?;
    assert_filtered_cursor_contract(
        driver,
        "type_list",
        json!({"space": ctx.space_id}),
        &duplicate_name,
        &[first_type.id.clone(), second_type.id.clone()],
    )
    .await?;
    walk_pages(
        driver,
        "property_list",
        json!({"space": ctx.space_id}),
        std::slice::from_ref(&property.id),
        1_000,
    )
    .await?;
    walk_pages(
        driver,
        "tag_list",
        json!({"space": ctx.space_id, "property": property.id}),
        &tag_ids,
        32,
    )
    .await?;
    walk_pages(
        driver,
        "template_list",
        json!({"space": ctx.space_id, "type": templates.type_.id}),
        &template_ids,
        32,
    )
    .await?;
    walk_pages(
        driver,
        "object_search",
        json!({"space": ctx.space_id, "text": search_term}),
        &object_ids,
        32,
    )
    .await?;
    let ambiguity = driver
        .call_tool_error(
            "property_list",
            json!({"space": ctx.space_id, "type": duplicate_name, "limit": 1}),
        )
        .await?;
    require(ambiguity.code() == "ambiguous", "ambiguous type resolution")
}

async fn run_documents(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let name = format!("MCP shared document {}", unique_suffix());
    let body = "gamma concurrent body";
    evidence.sensitive(&name);
    evidence.sensitive(body);
    evidence.sensitive("gamma");
    evidence.sensitive("delta");
    let object = create_object(ctx, &name, "").await?;
    evidence.fixture(&object.id);
    ctx.client
        .update_object(&ctx.space_id, &object.id)
        .body(body)
        .ensure_available()
        .update()
        .await
        .map_err(|_| "set document scenario body".to_owned())?;
    let initial = read_body(ctx, &object.id).await?;

    let resources = driver.list_resources().await?;
    require(
        resources["resources"] == json!([]),
        "resources/list is empty",
    )?;
    let templates = driver.list_resource_templates().await?;
    require(
        templates["resourceTemplates"][0]["uriTemplate"]
            == "anytype://spaces/{space_id}/objects/{object_id}",
        "resource template identity",
    )?;
    let result = run_document_scenario(
        driver,
        DocumentFixture {
            space_id: &ctx.space_id,
            object_id: &object.id,
            name: &name,
            initial_body: &initial,
            old_text: "gamma",
            new_text: "delta",
        },
    )
    .await?;
    let stored_body = read_body(ctx, &object.id).await?;
    require(
        stored_body == result.expected_body,
        "independent document edit readback",
    )?;
    let stored_sha256 = Sha256::digest(stored_body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    require(
        stored_sha256 == result.edited_sha256,
        "edit result hash matches independent backend readback",
    )
}

async fn run_views(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let collection_type = ctx
        .create_collection_type_fixture(format!("MCP shared collection type {}", unique_suffix()))
        .await
        .map_err(|_| "create collection type fixture".to_owned())?;
    require(
        collection_type.layout == ObjectLayout::Collection,
        "collection fixture layout",
    )?;
    evidence.fixture(&collection_type.id);
    let collection = ctx
        .create_collection_fixture(
            &collection_type,
            format!("MCP shared collection {}", unique_suffix()),
        )
        .await
        .map_err(|_| "create collection fixture".to_owned())?;
    evidence.fixture(&collection.id);
    let second_view = ctx
        .create_collection_view_fixture(
            &collection.id,
            &format!("MCP shared second view {}", unique_suffix()),
        )
        .await
        .map_err(|_| "create second view fixture".to_owned())?;
    evidence.fixture(&second_view.id);
    let first = create_object(ctx, &format!("MCP view A {}", unique_suffix()), "").await?;
    let second = create_object(ctx, &format!("MCP view B {}", unique_suffix()), "").await?;
    evidence.fixture(&first.id);
    evidence.fixture(&second.id);
    ctx.client
        .view_add_objects(
            &ctx.space_id,
            &collection.id,
            vec![first.id.clone(), second.id.clone()],
        )
        .await
        .map_err(|_| "add collection members".to_owned())?;
    let views = ctx
        .client
        .list_views(&ctx.space_id, &collection.id)
        .limit(100)
        .offset(0)
        .list()
        .await
        .map_err(|_| "read collection views".to_owned())?
        .into_response();
    let view_ids = views
        .items
        .into_iter()
        .map(|view| view.id)
        .collect::<Vec<_>>();
    walk_pages(
        driver,
        "view_list",
        json!({"space": ctx.space_id, "list_id": collection.id}),
        &view_ids,
        16,
    )
    .await?;
    for _ in 0..10 {
        match walk_pages(
            driver,
            "view_object_list",
            json!({
                "space": ctx.space_id,
                "list_id": collection.id,
                "view": second_view.id
            }),
            &[first.id.clone(), second.id.clone()],
            16,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(_) => tokio::time::sleep(Duration::from_millis(300)).await,
        }
    }
    Err("view_object_list did not converge".to_owned())
}

async fn run_mutations(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let suffix = unique_suffix();
    let name = format!("MCP shared mutation {suffix}");
    evidence.sensitive(&name);
    let create_input = json!({
        "space": ctx.space_id,
        "type": "page",
        "name": name,
        "idempotency_key": format!("mcp-shared-{suffix}")
    });
    let created = driver
        .call_tool("object_create", create_input.clone())
        .await?;
    let object_id = required_string(&created, "/object/id")?;
    ctx.register_object(&object_id);
    evidence.fixture(&object_id);
    let replay = driver.call_tool("object_create", create_input).await?;
    require(
        replay["object"]["id"] == object_id,
        "idempotent create replay identity",
    )?;
    let visible = ctx
        .client
        .object(&ctx.space_id, &object_id)
        .get()
        .await
        .map_err(|_| "read created object".to_owned())?;
    require(
        visible.name.as_deref() == Some(name.as_str()),
        "create readback",
    )?;

    let current = driver
        .call_tool(
            "object_get",
            json!({"space": ctx.space_id, "object_id": object_id, "body": {"max_chars": 100}}),
        )
        .await?;
    let updated_name = format!("MCP shared updated {suffix}");
    evidence.sensitive(&updated_name);
    driver
        .call_tool(
            "object_update",
            json!({
                "space": ctx.space_id,
                "object_id": object_id,
                "name": updated_name,
                "expected_body_sha256": current["body"]["sha256"]
            }),
        )
        .await?;
    let visible = ctx
        .client
        .object(&ctx.space_id, &object_id)
        .get()
        .await
        .map_err(|_| "read updated object".to_owned())?;
    require(
        visible.name.as_deref() == Some(updated_name.as_str()),
        "update readback",
    )?;

    ctx.client
        .update_object(&ctx.space_id, &object_id)
        .body("gamma concurrent body")
        .ensure_available()
        .update()
        .await
        .map_err(|_| "create concurrent body state".to_owned())?;
    let stale = driver
        .call_tool_error(
            "object_edit",
            json!({
                "space": ctx.space_id,
                "object_id": object_id,
                "edits": [{"old_text": "gamma", "new_text": "stale"}],
                "expected_body_sha256": current["body"]["sha256"]
            }),
        )
        .await?;
    require(stale.code() == "conflict", "stale edit conflict")?;
    let fresh = driver
        .call_tool(
            "object_get",
            json!({"space": ctx.space_id, "object_id": object_id, "body": {"max_chars": 100}}),
        )
        .await?;
    let count = driver
        .call_tool_error(
            "object_edit",
            json!({
                "space": ctx.space_id,
                "object_id": object_id,
                "edits": [{"old_text": "absent", "new_text": "never", "expected_matches": 1}],
                "expected_body_sha256": fresh["body"]["sha256"]
            }),
        )
        .await?;
    require(count.code() == "conflict", "match-count conflict")?;
    driver
        .call_tool(
            "object_edit",
            json!({
                "space": ctx.space_id,
                "object_id": object_id,
                "edits": [{"old_text": "gamma", "new_text": "delta"}],
                "expected_body_sha256": fresh["body"]["sha256"]
            }),
        )
        .await?;
    require(
        read_body(ctx, &object_id)
            .await?
            .contains("delta concurrent body"),
        "edit readback",
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MarkdownBlockEvidence {
    identity: Vec<(String, String, Vec<String>)>,
    semantics: Vec<(String, usize)>,
    has_link: bool,
}

fn markdown_block_kind(content: &BlockContent) -> String {
    match content {
        BlockContent::Text(text) => format!("text:{:?}", text.style),
        BlockContent::Layout(_) => "layout".to_owned(),
        BlockContent::Divider(_) => "divider".to_owned(),
        BlockContent::Bookmark(_) => "bookmark".to_owned(),
        BlockContent::Link(_) => "link".to_owned(),
        BlockContent::Relation(_) => "relation".to_owned(),
        BlockContent::FeaturedRelations => "featured_relations".to_owned(),
        BlockContent::Embed(_) => "embed".to_owned(),
        BlockContent::TableOfContents => "table_of_contents".to_owned(),
        BlockContent::Table => "table".to_owned(),
        BlockContent::TableRow { .. } => "table_row".to_owned(),
        BlockContent::TableColumn => "table_column".to_owned(),
        BlockContent::File(_) => "file".to_owned(),
        BlockContent::Unsupported(_) => "unsupported".to_owned(),
        _ => "future".to_owned(),
    }
}

fn markdown_block_evidence(snapshot: &BodySnapshot) -> Result<MarkdownBlockEvidence, String> {
    let mut identity = Vec::with_capacity(snapshot.len());
    let mut semantics = Vec::with_capacity(snapshot.len());
    let mut has_link = false;
    for block in snapshot.iter() {
        let kind = markdown_block_kind(&block.content);
        if let BlockContent::Text(text) = &block.content {
            has_link |= text
                .marks
                .iter()
                .any(|mark| matches!(mark.kind, MarkKind::Link { .. }));
        }
        let content = serde_json::to_string(&block.content)
            .map_err(|_| "serialize Markdown block evidence".to_owned())?;
        identity.push((
            block.id.as_str().to_owned(),
            kind,
            block
                .children
                .iter()
                .map(|child| child.as_str().to_owned())
                .collect(),
        ));
        semantics.push((content, block.children.len()));
    }
    Ok(MarkdownBlockEvidence {
        identity,
        semantics,
        has_link,
    })
}

async fn stable_markdown_export(ctx: &TestContext, object_id: &str) -> Result<String, String> {
    let mut previous = None;
    for _ in 0..12 {
        let markdown = ctx
            .client
            .object(&ctx.space_id, object_id)
            .get()
            .await
            .map_err(|_| "read independent Markdown export".to_owned())?
            .markdown
            .ok_or_else(|| "independent readback omitted Markdown".to_owned())?;
        if previous.as_ref() == Some(&markdown) {
            return Ok(markdown);
        }
        previous = Some(markdown);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("independent Markdown export did not stabilize".to_owned())
}

async fn stable_markdown_blocks(
    ctx: &TestContext,
    object_id: &str,
) -> Result<MarkdownBlockEvidence, String> {
    let mut previous = None;
    for _ in 0..12 {
        let snapshot = ctx
            .client
            .blocks()
            .body(&ctx.space_id, object_id)
            .fetch()
            .await
            .map_err(|_| "read independent Markdown ObjectShow evidence".to_owned())?;
        let evidence = markdown_block_evidence(&snapshot)?;
        if previous.as_ref() == Some(&evidence) {
            return Ok(evidence);
        }
        previous = Some(evidence);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("independent Markdown ObjectShow evidence did not stabilize".to_owned())
}

async fn run_markdown_noop(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let name = format!("MCP Markdown no-op {}", unique_suffix());
    let requested = concat!(
        "# Document heading\n\n",
        "## Stable heading\n\n",
        "- bullet one\n",
        "- bullet two\n\n",
        "1. numbered one\n",
        "2. numbered two\n\n",
        "- [ ] unchecked\n",
        "- [x] checked\n\n",
        "> one-line quote\n\n",
        "A [bounded link](https://example.com/path?q=one) with Unicode こんにちは 👋.\n\n",
        "First paragraph spans\n",
        "multiple source lines.\n\n",
        "Final paragraph."
    );
    evidence.sensitive(&name);
    evidence.sensitive(requested);
    let object = create_object(ctx, &name, requested).await?;
    evidence.fixture(&object.id);

    let before_export = stable_markdown_export(ctx, &object.id).await?;
    evidence.sensitive(&before_export);
    let before_blocks = stable_markdown_blocks(ctx, &object.id).await?;
    let before_kinds = before_blocks
        .identity
        .iter()
        .map(|(_, kind, _)| kind.as_str())
        .collect::<Vec<_>>();
    for expected in [
        "text:Header2",
        "text:Bulleted",
        "text:Numbered",
        "text:Checkbox",
        "text:Quote",
        "text:Paragraph",
    ] {
        if !before_kinds.contains(&expected) {
            return Err(format!(
                "Markdown no-op fixture expected block kind={expected} observed={before_kinds:?}"
            ));
        }
    }
    require(before_blocks.has_link, "Markdown no-op fixture link mark")?;

    let protocol = run_markdown_noop_protocol(
        driver,
        MarkdownNoopFixture {
            space_id: &ctx.space_id,
            object_id: &object.id,
            exported_body: &before_export,
        },
    )
    .await?;
    let after_export = stable_markdown_export(ctx, &object.id).await?;
    let after_blocks = stable_markdown_blocks(ctx, &object.id).await?;
    require(
        after_export == before_export,
        "independent Markdown no-op byte identity",
    )?;
    require(
        Sha256::digest(after_export.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            == protocol.body_sha256,
        "independent Markdown no-op hash",
    )?;
    require(
        before_blocks.semantics == after_blocks.semantics,
        "independent Markdown no-op typed semantics and order",
    )?;
    require(
        before_blocks.has_link == after_blocks.has_link,
        "independent Markdown no-op link semantics",
    )?;
    eprintln!(
        "MCP Markdown no-op evidence: transport_scenario={} bytes_before={} bytes_after={} blocks_before={} blocks_after={} block_identity={}",
        ScenarioId::MarkdownNoop.as_str(),
        protocol.before_bytes,
        protocol.after_bytes,
        before_blocks.identity.len(),
        after_blocks.identity.len(),
        before_blocks.identity == after_blocks.identity,
    );
    Ok(())
}

async fn run_archive(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let type_key = format!("mcp_shared_archive_{}", unique_suffix());
    let archive_type = ctx
        .client
        .new_type(
            &ctx.space_id,
            format!("MCP shared archive type {}", unique_suffix()),
        )
        .key(&type_key)
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create archive type fixture".to_owned())?;
    ctx.register_type(&archive_type.id);
    evidence.fixture(&archive_type.id);
    let object = ctx
        .client
        .new_object(&ctx.space_id, &type_key)
        .name(format!("MCP shared archive {}", unique_suffix()))
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create archive object fixture".to_owned())?;
    ctx.register_object(&object.id);
    evidence.fixture(&object.id);
    let type_id = object
        .r#type
        .as_ref()
        .map(|value| value.id.clone())
        .ok_or_else(|| "archive fixture type".to_owned())?;
    let result = driver
        .call_tool(
            "object_archive",
            json!({"space": ctx.space_id, "object_id": object.id}),
        )
        .await?;
    require(result["archived"] == true, "archive result")?;
    for _ in 0..10 {
        let active = active_contains(ctx, &object.id, &type_id).await?;
        let archived = archived_contains(ctx, &object.id, &type_id).await?;
        if !active && archived {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("archive evidence did not converge".to_owned())
}

async fn active_contains(
    ctx: &TestContext,
    object_id: &str,
    type_id: &str,
) -> Result<bool, String> {
    let page = ctx
        .client
        .objects(&ctx.space_id)
        .filter(anytype::prelude::Filter::type_in([type_id.to_owned()]))
        .limit(100)
        .offset(0)
        .list()
        .await
        .map_err(|_| "read active archive evidence".to_owned())?;
    require(
        !page.pagination.has_more,
        "unique archive type unexpectedly exceeds active evidence page",
    )?;
    Ok(page
        .items
        .iter()
        .any(|object| object.id == object_id && !object.archived))
}

async fn archived_contains(
    ctx: &TestContext,
    object_id: &str,
    type_id: &str,
) -> Result<bool, String> {
    let page = ctx
        .client
        .list_archived(&ctx.space_id)
        .types([type_id])
        .limit(100)
        .offset(0)
        .list()
        .await
        .map_err(|_| "read archived evidence".to_owned())?;
    require(
        !page.pagination.has_more,
        "unique archive type unexpectedly exceeds archived evidence page",
    )?;
    Ok(page.items.iter().any(|object| object.id == object_id))
}

async fn create_object(
    ctx: &TestContext,
    name: &str,
    body: &str,
) -> Result<anytype::prelude::Object, String> {
    let object = ctx
        .client
        .new_object(&ctx.space_id, "page")
        .name(name)
        .body(body)
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create live object fixture".to_owned())?;
    ctx.register_object(&object.id);
    Ok(object)
}

async fn read_body(ctx: &TestContext, object_id: &str) -> Result<String, String> {
    ctx.client
        .object(&ctx.space_id, object_id)
        .get()
        .await
        .map_err(|_| "read live object fixture".to_owned())
        .map(|object| object.markdown.unwrap_or_default())
}

async fn walk_pages(
    driver: &mut impl McpDriver,
    tool: &'static str,
    base: Value,
    expected_ids: &[String],
    max_pages: usize,
) -> Result<(), String> {
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    let mut seen_ids = HashSet::new();
    let mut binding_checked = false;
    for _ in 0..max_pages {
        let mut input = base
            .as_object()
            .cloned()
            .ok_or_else(|| "page input must be an object".to_owned())?;
        input.insert("limit".to_owned(), json!(1));
        if let Some(cursor) = &cursor {
            input.insert("cursor".to_owned(), json!(cursor));
        }
        let page = driver.call_tool(tool, Value::Object(input.clone())).await?;
        for item in page["items"]
            .as_array()
            .ok_or_else(|| format!("{tool} items array"))?
        {
            if let Some(id) = item_id(item) {
                require(
                    seen_ids.insert(id.to_owned()),
                    &format!("{tool} item progress"),
                )?;
            }
        }
        let Some(next) = page.get("next_cursor").and_then(Value::as_str) else {
            for id in expected_ids {
                require(
                    seen_ids.contains(id),
                    &format!("{tool} observes fixture {id}"),
                )?;
            }
            return Ok(());
        };
        require(
            seen_cursors.insert(next.to_owned()),
            &format!("{tool} cursor progress"),
        )?;
        if !binding_checked {
            let mut mismatch = input;
            mismatch.insert("limit".to_owned(), json!(2));
            mismatch.insert("cursor".to_owned(), json!(next));
            let code = driver
                .call_tool_error(tool, Value::Object(mismatch))
                .await?;
            require(
                code.code() == "validation",
                &format!("{tool} cursor binding"),
            )?;
            binding_checked = true;
        }
        cursor = Some(next.to_owned());
    }
    Err(format!("{tool} did not terminate within {max_pages} pages"))
}

async fn assert_filtered_cursor_contract(
    driver: &mut impl McpDriver,
    tool: &'static str,
    base: Value,
    filter_value: &str,
    expected_ids: &[String],
) -> Result<(), String> {
    const MAX_PAGES: usize = 8;

    let filter = |value: &str| {
        json!({
            "operator": "and",
            "conditions": [{
                "format": "text",
                "property_key": "name",
                "condition": "contains",
                "value": value
            }]
        })
    };
    let mut request = base
        .as_object()
        .cloned()
        .ok_or_else(|| "filtered page input must be an object".to_owned())?;
    request.insert("filters".to_owned(), filter(filter_value));
    request.insert("limit".to_owned(), json!(1));

    let first = driver
        .call_tool(tool, Value::Object(request.clone()))
        .await?;
    let first_items = first["items"]
        .as_array()
        .ok_or_else(|| format!("{tool} filtered items array"))?;
    require(
        first_items.len() == 1,
        &format!("{tool} filtered first page"),
    )?;
    let first_id =
        item_id(&first_items[0]).ok_or_else(|| format!("{tool} filtered item identity"))?;
    let mut seen_ids = HashSet::from([first_id.to_owned()]);
    let first_cursor = first
        .get("next_cursor")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{tool} filtered continuation"))?
        .to_owned();

    let mut mismatch = request.clone();
    mismatch.insert(
        "filters".to_owned(),
        filter(&format!("{filter_value}-mismatch")),
    );
    mismatch.insert("cursor".to_owned(), json!(first_cursor));
    let code = driver
        .call_tool_error(tool, Value::Object(mismatch))
        .await?;
    require(
        code.code() == "validation",
        &format!("{tool} filter cursor binding"),
    )?;

    let mut cursor = Some(first_cursor);
    for _ in 1..MAX_PAGES {
        let Some(next) = cursor.take() else {
            break;
        };
        let mut continuation = request.clone();
        continuation.insert("cursor".to_owned(), json!(next));
        let page = driver.call_tool(tool, Value::Object(continuation)).await?;
        for item in page["items"]
            .as_array()
            .ok_or_else(|| format!("{tool} filtered continuation items"))?
        {
            let id =
                item_id(item).ok_or_else(|| format!("{tool} filtered continuation identity"))?;
            require(
                seen_ids.insert(id.to_owned()),
                &format!("{tool} filtered identity progress"),
            )?;
        }
        cursor = page
            .get("next_cursor")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }
    require(
        cursor.is_none(),
        &format!("{tool} filtered pagination terminates"),
    )?;
    require(
        seen_ids == expected_ids.iter().cloned().collect(),
        &format!("{tool} filtered exact identities"),
    )
}

fn item_id(item: &Value) -> Option<&str> {
    item.get("id")
        .or_else(|| item.pointer("/summary/id"))
        .or_else(|| item.pointer("/object/id"))
        .and_then(Value::as_str)
}

/// Closed inventory of standard tool and resource operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LiveOperation {
    ObjectArchive,
    ObjectCreate,
    ObjectEdit,
    ObjectGet,
    ObjectSearch,
    ObjectUpdate,
    PropertyList,
    ServerStatus,
    SpaceList,
    TagList,
    TemplateList,
    TypeList,
    ViewList,
    ViewObjectList,
    ResourcesList,
    ResourcesRead,
    ResourcesTemplatesList,
}

/// Typed binding from one advertised operation to one executable scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ownership {
    pub operation: LiveOperation,
    pub scenario: ScenarioId,
}

pub const LIVE_OWNERSHIP: &[Ownership] = &[
    own(LiveOperation::ObjectArchive, ScenarioId::Archive),
    own(LiveOperation::ObjectCreate, ScenarioId::Mutations),
    own(LiveOperation::ObjectEdit, ScenarioId::Documents),
    own(LiveOperation::ObjectGet, ScenarioId::Documents),
    own(LiveOperation::ObjectSearch, ScenarioId::Documents),
    own(LiveOperation::ObjectUpdate, ScenarioId::Mutations),
    own(LiveOperation::PropertyList, ScenarioId::Discovery),
    own(LiveOperation::ServerStatus, ScenarioId::Discovery),
    own(LiveOperation::SpaceList, ScenarioId::Discovery),
    own(LiveOperation::TagList, ScenarioId::Discovery),
    own(LiveOperation::TemplateList, ScenarioId::Discovery),
    own(LiveOperation::TypeList, ScenarioId::Discovery),
    own(LiveOperation::ViewList, ScenarioId::Views),
    own(LiveOperation::ViewObjectList, ScenarioId::Views),
    own(LiveOperation::ResourcesList, ScenarioId::Documents),
    own(LiveOperation::ResourcesRead, ScenarioId::Documents),
    own(LiveOperation::ResourcesTemplatesList, ScenarioId::Documents),
];

const fn own(operation: LiveOperation, scenario: ScenarioId) -> Ownership {
    Ownership {
        operation,
        scenario,
    }
}

#[path = "optional_workflow.rs"]
mod optional_workflow;
pub use optional_workflow::{
    OptionalFastWorkflow, OptionalOperation, OptionalRealWorkflow, OptionalRegistry,
};

/// Evidence tier required for every optional production operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OptionalEvidenceTier {
    Fast,
    RealHeadless,
}

/// Exact executable workflow bound to one optional scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OptionalExecutableWorkflow {
    Fast(OptionalFastWorkflow),
    RealHeadless(OptionalRealWorkflow),
}

impl OptionalExecutableWorkflow {
    /// Evidence tier executed by this workflow.
    pub const fn tier(self) -> OptionalEvidenceTier {
        match self {
            Self::Fast(_) => OptionalEvidenceTier::Fast,
            Self::RealHeadless(_) => OptionalEvidenceTier::RealHeadless,
        }
    }

    /// Production registry routed by this workflow.
    pub const fn registry(self) -> OptionalRegistry {
        match self {
            Self::Fast(workflow) => workflow.registry(),
            Self::RealHeadless(workflow) => workflow.registry(),
        }
    }
}

const STATUS_OPERATIONS: &[OptionalOperation] = &[OptionalOperation::OptionalToolsetStatus];
const BODY_READ_OPERATIONS: &[OptionalOperation] = &[OptionalOperation::BodyBlockList];
const BODY_CREATE_OPERATIONS: &[OptionalOperation] = &[OptionalOperation::BodyBlockCreate];
const BODY_UPDATE_OPERATIONS: &[OptionalOperation] = &[OptionalOperation::BodyBlockUpdate];
const BODY_DELETE_OPERATIONS: &[OptionalOperation] = &[OptionalOperation::BodyBlockDelete];
const BODY_MOVE_OPERATIONS: &[OptionalOperation] = &[OptionalOperation::BodyBlockMove];
const RICH_PAGE_OPERATIONS: &[OptionalOperation] = &[OptionalOperation::RichPageCreate];
const BODY_OPERATIONS: &[OptionalOperation] = &[
    OptionalOperation::BodyBlockList,
    OptionalOperation::BodyBlockCreate,
    OptionalOperation::BodyBlockUpdate,
    OptionalOperation::BodyBlockDelete,
    OptionalOperation::BodyBlockMove,
    OptionalOperation::RichPageCreate,
];
const CHAT_READ_OPERATIONS: &[OptionalOperation] = &[
    OptionalOperation::ChatList,
    OptionalOperation::ChatMessageList,
    OptionalOperation::ChatMessageGet,
    OptionalOperation::ChatMessageSearch,
];
const CHAT_ADD_OPERATIONS: &[OptionalOperation] = &[OptionalOperation::ChatMessageAdd];
const CHAT_DELETE_OPERATIONS: &[OptionalOperation] = &[OptionalOperation::ChatMessageDelete];
const CHAT_OPERATIONS: &[OptionalOperation] = &[
    OptionalOperation::ChatList,
    OptionalOperation::ChatMessageList,
    OptionalOperation::ChatMessageGet,
    OptionalOperation::ChatMessageSearch,
    OptionalOperation::ChatMessageAdd,
    OptionalOperation::ChatMessageDelete,
];
const MEMBER_OPERATIONS: &[OptionalOperation] =
    &[OptionalOperation::MemberList, OptionalOperation::MemberGet];
const MEMBER_REAL_OPERATIONS: &[OptionalOperation] = &[
    OptionalOperation::OptionalToolsetStatus,
    OptionalOperation::MemberList,
    OptionalOperation::MemberGet,
];
const FILE_READ_OPERATIONS: &[OptionalOperation] = &[
    OptionalOperation::FileMetadata,
    OptionalOperation::FileRead,
    OptionalOperation::FileByteResource,
];
const FILE_UPLOAD_OPERATIONS: &[OptionalOperation] = &[OptionalOperation::FileUpload];
const FILE_OPERATIONS: &[OptionalOperation] = &[
    OptionalOperation::FileMetadata,
    OptionalOperation::FileRead,
    OptionalOperation::FileUpload,
    OptionalOperation::FileByteResource,
];
const SPACE_OPERATIONS: &[OptionalOperation] = &[
    OptionalOperation::SpaceCreate,
    OptionalOperation::SpaceUpdate,
];
const TYPE_OPERATIONS: &[OptionalOperation] = &[
    OptionalOperation::TypeGet,
    OptionalOperation::TypeCreate,
    OptionalOperation::TypeUpdate,
];
const PROPERTY_OPERATIONS: &[OptionalOperation] = &[
    OptionalOperation::PropertyCreate,
    OptionalOperation::PropertyUpdate,
];
const TAG_OPERATIONS: &[OptionalOperation] =
    &[OptionalOperation::TagCreate, OptionalOperation::TagUpdate];
const SCHEMA_OPERATIONS: &[OptionalOperation] = &[
    OptionalOperation::SpaceCreate,
    OptionalOperation::SpaceUpdate,
    OptionalOperation::TypeGet,
    OptionalOperation::TypeCreate,
    OptionalOperation::TypeUpdate,
    OptionalOperation::PropertyCreate,
    OptionalOperation::PropertyUpdate,
    OptionalOperation::TagCreate,
    OptionalOperation::TagUpdate,
];
const VIEW_OPERATIONS: &[OptionalOperation] = &[
    OptionalOperation::CollectionMemberList,
    OptionalOperation::CollectionMemberAdd,
    OptionalOperation::CollectionMemberRemove,
];

/// Typed identifier for one reviewed optional-toolset scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OptionalScenarioId {
    name: &'static str,
    tier: OptionalEvidenceTier,
    registry: OptionalRegistry,
    workflow: OptionalExecutableWorkflow,
    operations: &'static [OptionalOperation],
}

impl OptionalScenarioId {
    /// Exact executable scenario inventory owned by the common foundation and
    /// the six linked production descriptors.
    pub const EXECUTABLE: [Self; 64] = [
        define_fast(
            "optional_toolset_status_direct_contract",
            OptionalFastWorkflow::OptionalStatus,
            STATUS_OPERATIONS,
        ),
        define_fast(
            "optional_toolset_status_stdio_contract",
            OptionalFastWorkflow::OptionalStatus,
            STATUS_OPERATIONS,
        ),
        define_fast(
            "body_list_ordered_pages",
            OptionalFastWorkflow::BodyBlocks,
            BODY_READ_OPERATIONS,
        ),
        define_fast(
            "body_list_revision_conflict",
            OptionalFastWorkflow::BodyBlocks,
            BODY_READ_OPERATIONS,
        ),
        define_fast(
            "body_limits_fail_closed",
            OptionalFastWorkflow::BodyBlocks,
            BODY_OPERATIONS,
        ),
        define_fast(
            "body_opaque_read_only",
            OptionalFastWorkflow::BodyBlocks,
            BODY_READ_OPERATIONS,
        ),
        define_fast(
            "body_create_idempotent",
            OptionalFastWorkflow::BodyBlocks,
            BODY_CREATE_OPERATIONS,
        ),
        define_fast(
            "body_update_one_change",
            OptionalFastWorkflow::BodyBlocks,
            BODY_UPDATE_OPERATIONS,
        ),
        define_fast(
            "body_delete_confirmed_subtree",
            OptionalFastWorkflow::BodyBlocks,
            BODY_DELETE_OPERATIONS,
        ),
        define_fast(
            "body_move_same_object",
            OptionalFastWorkflow::BodyBlocks,
            BODY_MOVE_OPERATIONS,
        ),
        define_fast(
            "body_relation_workflows",
            OptionalFastWorkflow::BodyBlocks,
            BODY_OPERATIONS,
        ),
        define_fast(
            "body_targeted_heading_append",
            OptionalFastWorkflow::BodyBlocks,
            BODY_UPDATE_OPERATIONS,
        ),
        define_fast(
            "rich_page_complete",
            OptionalFastWorkflow::BodyBlocks,
            RICH_PAGE_OPERATIONS,
        ),
        define_fast(
            "rich_page_partial",
            OptionalFastWorkflow::BodyBlocks,
            RICH_PAGE_OPERATIONS,
        ),
        define_fast(
            "rich_page_indeterminate",
            OptionalFastWorkflow::BodyBlocks,
            RICH_PAGE_OPERATIONS,
        ),
        define_fast(
            "rich_page_replay_drift",
            OptionalFastWorkflow::BodyBlocks,
            RICH_PAGE_OPERATIONS,
        ),
        define_fast(
            "body_read_only_catalog",
            OptionalFastWorkflow::BodyBlocks,
            BODY_READ_OPERATIONS,
        ),
        define_fast(
            "body_read_restricted",
            OptionalFastWorkflow::BodyBlocks,
            BODY_READ_OPERATIONS,
        ),
        define_fast(
            "body_network_closed",
            OptionalFastWorkflow::BodyBlocks,
            BODY_OPERATIONS,
        ),
        define_fast(
            "body_protocol_parity",
            OptionalFastWorkflow::BodyBlocks,
            BODY_OPERATIONS,
        ),
        define_fast(
            "body_redaction_and_budgets",
            OptionalFastWorkflow::BodyBlocks,
            BODY_OPERATIONS,
        ),
        define_real(
            "body_blocks_direct_real_headless",
            OptionalRealWorkflow::BodyBlocks,
            BODY_OPERATIONS,
        ),
        define_real(
            "body_blocks_stable_stdio_real_headless",
            OptionalRealWorkflow::BodyBlocks,
            BODY_OPERATIONS,
        ),
        define_real(
            "body_blocks_preview_stdio_real_headless",
            OptionalRealWorkflow::BodyBlocks,
            BODY_OPERATIONS,
        ),
        define_fast(
            "chats_read_direct",
            OptionalFastWorkflow::Chats,
            CHAT_READ_OPERATIONS,
        ),
        define_fast(
            "chats_read_stdio",
            OptionalFastWorkflow::Chats,
            CHAT_READ_OPERATIONS,
        ),
        define_fast(
            "chat_add_direct",
            OptionalFastWorkflow::Chats,
            CHAT_ADD_OPERATIONS,
        ),
        define_fast(
            "chat_add_stdio",
            OptionalFastWorkflow::Chats,
            CHAT_ADD_OPERATIONS,
        ),
        define_fast(
            "chat_delete_direct",
            OptionalFastWorkflow::Chats,
            CHAT_DELETE_OPERATIONS,
        ),
        define_fast(
            "chat_delete_stdio",
            OptionalFastWorkflow::Chats,
            CHAT_DELETE_OPERATIONS,
        ),
        define_fast(
            "chats_registry_direct_contract",
            OptionalFastWorkflow::Chats,
            CHAT_OPERATIONS,
        ),
        define_fast(
            "chats_registry_stable_stdio_contract",
            OptionalFastWorkflow::Chats,
            CHAT_OPERATIONS,
        ),
        define_fast(
            "chats_registry_preview_stdio_contract",
            OptionalFastWorkflow::Chats,
            CHAT_OPERATIONS,
        ),
        define_real(
            "chats_read_headless",
            OptionalRealWorkflow::Chats,
            CHAT_READ_OPERATIONS,
        ),
        define_real(
            "chat_add_headless",
            OptionalRealWorkflow::Chats,
            CHAT_ADD_OPERATIONS,
        ),
        define_real(
            "chat_delete_headless",
            OptionalRealWorkflow::Chats,
            CHAT_DELETE_OPERATIONS,
        ),
        define_real(
            "chats_registry_real_direct",
            OptionalRealWorkflow::Chats,
            CHAT_OPERATIONS,
        ),
        define_real(
            "chats_registry_real_stable_stdio",
            OptionalRealWorkflow::Chats,
            CHAT_OPERATIONS,
        ),
        define_real(
            "chats_registry_real_preview_stdio",
            OptionalRealWorkflow::Chats,
            CHAT_OPERATIONS,
        ),
        define_fast(
            "members_direct",
            OptionalFastWorkflow::Members,
            MEMBER_OPERATIONS,
        ),
        define_real(
            "members_headless",
            OptionalRealWorkflow::Members,
            MEMBER_REAL_OPERATIONS,
        ),
        define_fast(
            "file_content_direct_contract",
            OptionalFastWorkflow::Files,
            FILE_READ_OPERATIONS,
        ),
        define_fast(
            "file_content_stdio_contract",
            OptionalFastWorkflow::Files,
            FILE_READ_OPERATIONS,
        ),
        define_fast(
            "file_upload_direct_contract",
            OptionalFastWorkflow::Files,
            FILE_UPLOAD_OPERATIONS,
        ),
        define_fast(
            "file_upload_stdio_contract",
            OptionalFastWorkflow::Files,
            FILE_UPLOAD_OPERATIONS,
        ),
        define_real(
            "file_content_real_headless",
            OptionalRealWorkflow::Files,
            FILE_OPERATIONS,
        ),
        define_fast(
            "schema_space_direct",
            OptionalFastWorkflow::Schema,
            SPACE_OPERATIONS,
        ),
        define_fast(
            "schema_space_stdio",
            OptionalFastWorkflow::Schema,
            SPACE_OPERATIONS,
        ),
        define_fast(
            "schema_type_direct",
            OptionalFastWorkflow::Schema,
            TYPE_OPERATIONS,
        ),
        define_fast(
            "schema_type_stdio",
            OptionalFastWorkflow::Schema,
            TYPE_OPERATIONS,
        ),
        define_fast(
            "schema_property_direct",
            OptionalFastWorkflow::Schema,
            PROPERTY_OPERATIONS,
        ),
        define_fast(
            "schema_property_stdio",
            OptionalFastWorkflow::Schema,
            PROPERTY_OPERATIONS,
        ),
        define_fast(
            "schema_tag_direct",
            OptionalFastWorkflow::Schema,
            TAG_OPERATIONS,
        ),
        define_fast(
            "schema_tag_stdio",
            OptionalFastWorkflow::Schema,
            TAG_OPERATIONS,
        ),
        define_fast(
            "schema_registry_direct_contract",
            OptionalFastWorkflow::Schema,
            SCHEMA_OPERATIONS,
        ),
        define_fast(
            "schema_registry_stdio_contract",
            OptionalFastWorkflow::Schema,
            SCHEMA_OPERATIONS,
        ),
        define_real(
            "schema_space_headless",
            OptionalRealWorkflow::Schema,
            SPACE_OPERATIONS,
        ),
        define_real(
            "schema_type_headless",
            OptionalRealWorkflow::Schema,
            TYPE_OPERATIONS,
        ),
        define_real(
            "schema_property_headless",
            OptionalRealWorkflow::Schema,
            PROPERTY_OPERATIONS,
        ),
        define_real(
            "schema_tag_headless",
            OptionalRealWorkflow::Schema,
            TAG_OPERATIONS,
        ),
        define_real(
            "schema_registry_real_headless",
            OptionalRealWorkflow::Schema,
            SCHEMA_OPERATIONS,
        ),
        define_fast(
            "collection_member_acceptance_direct",
            OptionalFastWorkflow::ViewsWrite,
            VIEW_OPERATIONS,
        ),
        define_fast(
            "collection_member_acceptance_stdio",
            OptionalFastWorkflow::ViewsWrite,
            VIEW_OPERATIONS,
        ),
        define_real(
            "collection_member_acceptance_headless",
            OptionalRealWorkflow::ViewsWrite,
            VIEW_OPERATIONS,
        ),
    ];

    /// Stable descriptor-owned scenario name.
    pub const fn as_str(self) -> &'static str {
        self.name
    }

    /// Required evidence tier for this scenario.
    pub const fn tier(self) -> OptionalEvidenceTier {
        self.tier
    }

    /// Production descriptor that declares this scenario.
    pub const fn registry(self) -> OptionalRegistry {
        self.registry
    }

    /// Exact executable workflow that runs this scenario.
    pub const fn workflow(self) -> OptionalExecutableWorkflow {
        self.workflow
    }

    /// Returns whether this scenario directly exercises the operation.
    pub fn covers(self, operation: OptionalOperation) -> bool {
        self.operations.contains(&operation)
    }

    fn parse(name: &str, tier: OptionalEvidenceTier) -> Option<Self> {
        Self::EXECUTABLE
            .iter()
            .copied()
            .find(|scenario| scenario.name == name && scenario.tier == tier)
    }
}

const fn define_fast(
    name: &'static str,
    workflow: OptionalFastWorkflow,
    operations: &'static [OptionalOperation],
) -> OptionalScenarioId {
    OptionalScenarioId {
        name,
        tier: OptionalEvidenceTier::Fast,
        registry: workflow.registry(),
        workflow: OptionalExecutableWorkflow::Fast(workflow),
        operations,
    }
}

const fn define_real(
    name: &'static str,
    workflow: OptionalRealWorkflow,
    operations: &'static [OptionalOperation],
) -> OptionalScenarioId {
    OptionalScenarioId {
        name,
        tier: OptionalEvidenceTier::RealHeadless,
        registry: workflow.registry(),
        workflow: OptionalExecutableWorkflow::RealHeadless(workflow),
        operations,
    }
}

const fn optional_fast(name: &'static str) -> OptionalScenarioId {
    match find_optional_scenario(name, OptionalEvidenceTier::Fast) {
        Some(scenario) => scenario,
        None => panic!("unknown fast optional scenario"),
    }
}

const fn optional_real(name: &'static str) -> OptionalScenarioId {
    match find_optional_scenario(name, OptionalEvidenceTier::RealHeadless) {
        Some(scenario) => scenario,
        None => panic!("unknown real-headless optional scenario"),
    }
}

const fn find_optional_scenario(
    name: &str,
    tier: OptionalEvidenceTier,
) -> Option<OptionalScenarioId> {
    let mut index = 0;
    while index < OptionalScenarioId::EXECUTABLE.len() {
        let scenario = OptionalScenarioId::EXECUTABLE[index];
        if scenario.tier as u8 == tier as u8 && optional_scenario_name_eq(scenario.name, name) {
            return Some(scenario);
        }
        index += 1;
    }
    None
}

const fn optional_scenario_name_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// One operation-to-scenario binding at one evidence tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionalOwnership {
    pub operation: OptionalOperation,
    pub scenario: OptionalScenarioId,
}

const fn optional_owner(
    operation: OptionalOperation,
    scenario: OptionalScenarioId,
) -> OptionalOwnership {
    OptionalOwnership {
        operation,
        scenario,
    }
}

/// Exact fast and real-headless ownership for every optional operation.
pub const OPTIONAL_LIVE_OWNERSHIP: &[OptionalOwnership; 62] = &[
    optional_owner(
        OptionalOperation::OptionalToolsetStatus,
        optional_fast("optional_toolset_status_direct_contract"),
    ),
    optional_owner(
        OptionalOperation::OptionalToolsetStatus,
        optional_real("members_headless"),
    ),
    optional_owner(
        OptionalOperation::BodyBlockList,
        optional_fast("body_list_ordered_pages"),
    ),
    optional_owner(
        OptionalOperation::BodyBlockList,
        optional_real("body_blocks_direct_real_headless"),
    ),
    optional_owner(
        OptionalOperation::BodyBlockCreate,
        optional_fast("body_create_idempotent"),
    ),
    optional_owner(
        OptionalOperation::BodyBlockCreate,
        optional_real("body_blocks_direct_real_headless"),
    ),
    optional_owner(
        OptionalOperation::BodyBlockUpdate,
        optional_fast("body_update_one_change"),
    ),
    optional_owner(
        OptionalOperation::BodyBlockUpdate,
        optional_real("body_blocks_direct_real_headless"),
    ),
    optional_owner(
        OptionalOperation::BodyBlockDelete,
        optional_fast("body_delete_confirmed_subtree"),
    ),
    optional_owner(
        OptionalOperation::BodyBlockDelete,
        optional_real("body_blocks_direct_real_headless"),
    ),
    optional_owner(
        OptionalOperation::BodyBlockMove,
        optional_fast("body_move_same_object"),
    ),
    optional_owner(
        OptionalOperation::BodyBlockMove,
        optional_real("body_blocks_direct_real_headless"),
    ),
    optional_owner(
        OptionalOperation::RichPageCreate,
        optional_fast("rich_page_complete"),
    ),
    optional_owner(
        OptionalOperation::RichPageCreate,
        optional_real("body_blocks_direct_real_headless"),
    ),
    optional_owner(
        OptionalOperation::ChatList,
        optional_fast("chats_read_direct"),
    ),
    optional_owner(
        OptionalOperation::ChatList,
        optional_real("chats_read_headless"),
    ),
    optional_owner(
        OptionalOperation::ChatMessageList,
        optional_fast("chats_read_direct"),
    ),
    optional_owner(
        OptionalOperation::ChatMessageList,
        optional_real("chats_read_headless"),
    ),
    optional_owner(
        OptionalOperation::ChatMessageGet,
        optional_fast("chats_read_direct"),
    ),
    optional_owner(
        OptionalOperation::ChatMessageGet,
        optional_real("chats_read_headless"),
    ),
    optional_owner(
        OptionalOperation::ChatMessageSearch,
        optional_fast("chats_read_direct"),
    ),
    optional_owner(
        OptionalOperation::ChatMessageSearch,
        optional_real("chats_read_headless"),
    ),
    optional_owner(
        OptionalOperation::ChatMessageAdd,
        optional_fast("chat_add_direct"),
    ),
    optional_owner(
        OptionalOperation::ChatMessageAdd,
        optional_real("chat_add_headless"),
    ),
    optional_owner(
        OptionalOperation::ChatMessageDelete,
        optional_fast("chat_delete_direct"),
    ),
    optional_owner(
        OptionalOperation::ChatMessageDelete,
        optional_real("chat_delete_headless"),
    ),
    optional_owner(
        OptionalOperation::FileMetadata,
        optional_fast("file_content_direct_contract"),
    ),
    optional_owner(
        OptionalOperation::FileMetadata,
        optional_real("file_content_real_headless"),
    ),
    optional_owner(
        OptionalOperation::FileRead,
        optional_fast("file_content_direct_contract"),
    ),
    optional_owner(
        OptionalOperation::FileRead,
        optional_real("file_content_real_headless"),
    ),
    optional_owner(
        OptionalOperation::FileUpload,
        optional_fast("file_upload_direct_contract"),
    ),
    optional_owner(
        OptionalOperation::FileUpload,
        optional_real("file_content_real_headless"),
    ),
    optional_owner(
        OptionalOperation::FileByteResource,
        optional_fast("file_content_direct_contract"),
    ),
    optional_owner(
        OptionalOperation::FileByteResource,
        optional_real("file_content_real_headless"),
    ),
    optional_owner(
        OptionalOperation::MemberList,
        optional_fast("members_direct"),
    ),
    optional_owner(
        OptionalOperation::MemberList,
        optional_real("members_headless"),
    ),
    optional_owner(
        OptionalOperation::MemberGet,
        optional_fast("members_direct"),
    ),
    optional_owner(
        OptionalOperation::MemberGet,
        optional_real("members_headless"),
    ),
    optional_owner(
        OptionalOperation::SpaceCreate,
        optional_fast("schema_space_direct"),
    ),
    optional_owner(
        OptionalOperation::SpaceCreate,
        optional_real("schema_space_headless"),
    ),
    optional_owner(
        OptionalOperation::SpaceUpdate,
        optional_fast("schema_space_direct"),
    ),
    optional_owner(
        OptionalOperation::SpaceUpdate,
        optional_real("schema_space_headless"),
    ),
    optional_owner(
        OptionalOperation::TypeGet,
        optional_fast("schema_type_direct"),
    ),
    optional_owner(
        OptionalOperation::TypeGet,
        optional_real("schema_type_headless"),
    ),
    optional_owner(
        OptionalOperation::TypeCreate,
        optional_fast("schema_type_direct"),
    ),
    optional_owner(
        OptionalOperation::TypeCreate,
        optional_real("schema_type_headless"),
    ),
    optional_owner(
        OptionalOperation::TypeUpdate,
        optional_fast("schema_type_direct"),
    ),
    optional_owner(
        OptionalOperation::TypeUpdate,
        optional_real("schema_type_headless"),
    ),
    optional_owner(
        OptionalOperation::PropertyCreate,
        optional_fast("schema_property_direct"),
    ),
    optional_owner(
        OptionalOperation::PropertyCreate,
        optional_real("schema_property_headless"),
    ),
    optional_owner(
        OptionalOperation::PropertyUpdate,
        optional_fast("schema_property_direct"),
    ),
    optional_owner(
        OptionalOperation::PropertyUpdate,
        optional_real("schema_property_headless"),
    ),
    optional_owner(
        OptionalOperation::TagCreate,
        optional_fast("schema_tag_direct"),
    ),
    optional_owner(
        OptionalOperation::TagCreate,
        optional_real("schema_tag_headless"),
    ),
    optional_owner(
        OptionalOperation::TagUpdate,
        optional_fast("schema_tag_direct"),
    ),
    optional_owner(
        OptionalOperation::TagUpdate,
        optional_real("schema_tag_headless"),
    ),
    optional_owner(
        OptionalOperation::CollectionMemberList,
        optional_fast("collection_member_acceptance_direct"),
    ),
    optional_owner(
        OptionalOperation::CollectionMemberList,
        optional_real("collection_member_acceptance_headless"),
    ),
    optional_owner(
        OptionalOperation::CollectionMemberAdd,
        optional_fast("collection_member_acceptance_direct"),
    ),
    optional_owner(
        OptionalOperation::CollectionMemberAdd,
        optional_real("collection_member_acceptance_headless"),
    ),
    optional_owner(
        OptionalOperation::CollectionMemberRemove,
        optional_fast("collection_member_acceptance_direct"),
    ),
    optional_owner(
        OptionalOperation::CollectionMemberRemove,
        optional_real("collection_member_acceptance_headless"),
    ),
];

fn parse_tool(name: &str) -> Option<LiveOperation> {
    Some(match name {
        "object_archive" => LiveOperation::ObjectArchive,
        "object_create" => LiveOperation::ObjectCreate,
        "object_edit" => LiveOperation::ObjectEdit,
        "object_get" => LiveOperation::ObjectGet,
        "object_search" => LiveOperation::ObjectSearch,
        "object_update" => LiveOperation::ObjectUpdate,
        "property_list" => LiveOperation::PropertyList,
        "server_status" => LiveOperation::ServerStatus,
        "space_list" => LiveOperation::SpaceList,
        "tag_list" => LiveOperation::TagList,
        "template_list" => LiveOperation::TemplateList,
        "type_list" => LiveOperation::TypeList,
        "view_list" => LiveOperation::ViewList,
        "view_object_list" => LiveOperation::ViewObjectList,
        _ => return None,
    })
}

fn parse_resource(name: &str) -> Option<LiveOperation> {
    Some(match name {
        "resources/list" => LiveOperation::ResourcesList,
        "resources/read" => LiveOperation::ResourcesRead,
        "resources/templates/list" => LiveOperation::ResourcesTemplatesList,
        _ => return None,
    })
}

fn parse_optional_tool(name: &str) -> Option<OptionalOperation> {
    Some(match name {
        "optional_toolset_status" => OptionalOperation::OptionalToolsetStatus,
        "body_block_list" => OptionalOperation::BodyBlockList,
        "body_block_create" => OptionalOperation::BodyBlockCreate,
        "body_block_update" => OptionalOperation::BodyBlockUpdate,
        "body_block_delete" => OptionalOperation::BodyBlockDelete,
        "body_block_move" => OptionalOperation::BodyBlockMove,
        "rich_page_create" => OptionalOperation::RichPageCreate,
        "chat_list" => OptionalOperation::ChatList,
        "chat_message_list" => OptionalOperation::ChatMessageList,
        "chat_message_get" => OptionalOperation::ChatMessageGet,
        "chat_message_search" => OptionalOperation::ChatMessageSearch,
        "chat_message_add" => OptionalOperation::ChatMessageAdd,
        "chat_message_delete" => OptionalOperation::ChatMessageDelete,
        "file_metadata" => OptionalOperation::FileMetadata,
        "file_read" => OptionalOperation::FileRead,
        "file_upload" => OptionalOperation::FileUpload,
        "member_list" => OptionalOperation::MemberList,
        "member_get" => OptionalOperation::MemberGet,
        "space_create" => OptionalOperation::SpaceCreate,
        "space_update" => OptionalOperation::SpaceUpdate,
        "type_get" => OptionalOperation::TypeGet,
        "type_create" => OptionalOperation::TypeCreate,
        "type_update" => OptionalOperation::TypeUpdate,
        "property_create" => OptionalOperation::PropertyCreate,
        "property_update" => OptionalOperation::PropertyUpdate,
        "tag_create" => OptionalOperation::TagCreate,
        "tag_update" => OptionalOperation::TagUpdate,
        "collection_member_list" => OptionalOperation::CollectionMemberList,
        "collection_member_add" => OptionalOperation::CollectionMemberAdd,
        "collection_member_remove" => OptionalOperation::CollectionMemberRemove,
        _ => return None,
    })
}

fn parse_optional_resource(name: &str) -> Option<OptionalOperation> {
    (name == "anytype-file://bytes/{space_id}/{file_id}/{offset}/{length}/{sha256}")
        .then_some(OptionalOperation::FileByteResource)
}

/// Validates exact, unique, executable live ownership for the production catalog.
pub fn validate_live_ownership(
    expected_tools: &[&str],
    expected_resources: &[&str],
) -> Result<(), String> {
    validate_ownership(expected_tools, expected_resources, LIVE_OWNERSHIP)
}

fn validate_ownership(
    expected_tools: &[&str],
    expected_resources: &[&str],
    owners: &[Ownership],
) -> Result<(), String> {
    let mut expected = HashSet::new();
    for name in expected_tools {
        let operation =
            parse_tool(name).ok_or_else(|| format!("unknown advertised tool operation: {name}"))?;
        expected.insert(operation);
    }
    for name in expected_resources {
        let operation = parse_resource(name)
            .ok_or_else(|| format!("unknown advertised resource operation: {name}"))?;
        expected.insert(operation);
    }
    validate_typed_ownership(
        &expected,
        owners.iter().map(|owner| (owner.operation, owner.scenario)),
        |scenario| scenario.is_executable() && ScenarioId::EXECUTABLE.contains(&scenario),
        ScenarioId::as_str,
        "live operation",
        "live scenario",
    )
}

fn validate_typed_ownership<K, S>(
    expected: &HashSet<K>,
    owners: impl IntoIterator<Item = (K, S)>,
    scenario_is_executable: impl Fn(S) -> bool,
    scenario_name: impl Fn(S) -> &'static str,
    operation_label: &str,
    scenario_label: &str,
) -> Result<(), String>
where
    K: Copy + Eq + Hash + Ord + std::fmt::Debug,
    S: Copy,
{
    let mut seen = HashSet::new();
    for (operation, scenario) in owners {
        if !expected.contains(&operation) {
            return Err(format!("unknown {operation_label} owner: {operation:?}"));
        }
        if !seen.insert(operation) {
            return Err(format!("duplicate {operation_label} owner: {operation:?}"));
        }
        if !scenario_is_executable(scenario) {
            return Err(format!(
                "non-executable {scenario_label} owner: {}",
                scenario_name(scenario)
            ));
        }
    }
    let mut missing = expected.difference(&seen).copied().collect::<Vec<_>>();
    missing.sort_unstable();
    if let Some(operation) = missing.first() {
        return Err(format!("missing {operation_label} owner: {operation:?}"));
    }
    Ok(())
}

/// One descriptor-tagged optional scenario declaration.
///
/// Keeping the descriptor identity beside the scenario name prevents a valid
/// identifier from one registry from silently satisfying another registry's
/// inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionalScenarioDeclaration {
    registry: OptionalRegistry,
    name: &'static str,
    tier: OptionalEvidenceTier,
}

impl OptionalScenarioDeclaration {
    /// Declares one fast scenario under its production descriptor.
    #[allow(dead_code)] // Shared support targets do not all enumerate descriptors.
    pub const fn fast(registry: OptionalRegistry, name: &'static str) -> Self {
        Self {
            registry,
            name,
            tier: OptionalEvidenceTier::Fast,
        }
    }

    /// Declares one real-headless scenario under its production descriptor.
    #[allow(dead_code)] // Shared support targets do not all enumerate descriptors.
    pub const fn real_headless(registry: OptionalRegistry, name: &'static str) -> Self {
        Self {
            registry,
            name,
            tier: OptionalEvidenceTier::RealHeadless,
        }
    }
}

fn optional_scenario_inventory(
    declarations: &[OptionalScenarioDeclaration],
) -> Result<HashSet<OptionalScenarioId>, String> {
    let executable_names = OptionalScenarioId::EXECUTABLE
        .iter()
        .map(|scenario| scenario.as_str())
        .collect::<BTreeSet<_>>();
    if executable_names.len() != OptionalScenarioId::EXECUTABLE.len() {
        return Err("duplicate executable optional scenario identifier".to_owned());
    }

    let mut names = HashSet::new();
    let mut scenarios = HashSet::new();
    for declaration in declarations {
        if !names.insert(declaration.name) {
            return Err(format!(
                "duplicate optional scenario identifier: {}",
                declaration.name
            ));
        }
        let tier_name = match declaration.tier {
            OptionalEvidenceTier::Fast => "fast",
            OptionalEvidenceTier::RealHeadless => "real-headless",
        };
        let scenario =
            OptionalScenarioId::parse(declaration.name, declaration.tier).ok_or_else(|| {
                format!(
                    "unknown {tier_name} optional scenario: {}",
                    declaration.name
                )
            })?;
        if scenario.registry() != declaration.registry {
            return Err(format!(
                "optional scenario descriptor mismatch: {} declared by {}, owned by {}",
                scenario.as_str(),
                declaration.registry.as_str(),
                scenario.registry().as_str()
            ));
        }
        if scenario.workflow().tier() != declaration.tier
            || scenario.workflow().registry() != declaration.registry
        {
            return Err(format!(
                "optional scenario workflow mismatch: {}",
                scenario.as_str()
            ));
        }
        scenarios.insert(scenario);
    }

    let executable = OptionalScenarioId::EXECUTABLE
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut missing = executable
        .difference(&scenarios)
        .copied()
        .collect::<Vec<_>>();
    missing.sort_unstable();
    if let Some(scenario) = missing.first() {
        return Err(format!(
            "missing executable optional scenario: {}",
            scenario.as_str()
        ));
    }
    Ok(scenarios)
}

/// Validates exact optional catalog ownership against the linked executable
/// fast and real-headless scenario inventory.
pub fn validate_optional_live_ownership(
    expected_tools: &[&str],
    expected_resource_families: &[&str],
    scenario_declarations: &[OptionalScenarioDeclaration],
) -> Result<(), String> {
    let mut operations = HashSet::new();
    for name in expected_tools {
        let operation = parse_optional_tool(name)
            .ok_or_else(|| format!("unknown advertised optional tool operation: {name}"))?;
        if !operations.insert(operation) {
            return Err(format!(
                "duplicate advertised optional tool operation: {name}"
            ));
        }
    }
    for name in expected_resource_families {
        let operation = parse_optional_resource(name)
            .ok_or_else(|| format!("unknown advertised optional resource family: {name}"))?;
        if !operations.insert(operation) {
            return Err(format!(
                "duplicate advertised optional resource family: {name}"
            ));
        }
    }

    let scenarios = optional_scenario_inventory(scenario_declarations)?;
    let expected = operations
        .iter()
        .flat_map(|operation| {
            [
                (*operation, OptionalEvidenceTier::Fast),
                (*operation, OptionalEvidenceTier::RealHeadless),
            ]
        })
        .collect::<HashSet<_>>();
    validate_optional_typed_ownership(&expected, OPTIONAL_LIVE_OWNERSHIP, &scenarios)
}

fn validate_optional_typed_ownership(
    expected: &HashSet<(OptionalOperation, OptionalEvidenceTier)>,
    owners: &[OptionalOwnership],
    scenarios: &HashSet<OptionalScenarioId>,
) -> Result<(), String> {
    let mut seen = HashSet::new();
    for owner in owners {
        let key = (owner.operation, owner.scenario.tier());
        if !expected.contains(&key) {
            return Err(format!("unknown optional operation/tier owner: {key:?}"));
        }
        if !seen.insert(key) {
            return Err(format!("duplicate optional operation/tier owner: {key:?}"));
        }
        if !scenarios.contains(&owner.scenario) {
            return Err(format!(
                "non-executable optional scenario owner: {}",
                owner.scenario.as_str()
            ));
        }

        let operation_registry = owner.operation.registry();
        let scenario_registry = owner.scenario.registry();
        if operation_registry != scenario_registry {
            return Err(format!(
                "optional owner registry mismatch: {:?} is {}, scenario {} is {}",
                owner.operation,
                operation_registry.as_str(),
                owner.scenario.as_str(),
                scenario_registry.as_str()
            ));
        }

        let expected_workflow = match owner.scenario.tier() {
            OptionalEvidenceTier::Fast => {
                OptionalExecutableWorkflow::Fast(owner.operation.fast_workflow())
            }
            OptionalEvidenceTier::RealHeadless => {
                OptionalExecutableWorkflow::RealHeadless(owner.operation.real_workflow())
            }
        };
        if expected_workflow.registry() != operation_registry {
            return Err(format!(
                "optional operation workflow registry mismatch: {:?}",
                owner.operation
            ));
        }
        if owner.scenario.workflow() != expected_workflow {
            return Err(format!(
                "optional owner workflow mismatch: {:?} uses {}, scenario {} uses {:?}",
                owner.operation,
                operation_registry.as_str(),
                owner.scenario.as_str(),
                owner.scenario.workflow()
            ));
        }
        if !owner.scenario.covers(owner.operation) {
            return Err(format!(
                "optional scenario does not cover operation: {} does not cover {:?}",
                owner.scenario.as_str(),
                owner.operation
            ));
        }
    }

    let mut missing = expected.difference(&seen).copied().collect::<Vec<_>>();
    missing.sort_unstable();
    if let Some(key) = missing.first() {
        return Err(format!("missing optional operation/tier owner: {key:?}"));
    }
    Ok(())
}

fn required_string(value: &Value, pointer: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("missing string at {pointer}"))
}

fn require(condition: bool, message: &str) -> Result<(), String> {
    condition.then_some(()).ok_or_else(|| message.to_owned())
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use std::collections::VecDeque;

    struct ScriptedNoopDriver {
        responses: VecDeque<Value>,
        calls: Vec<(&'static str, Value)>,
    }

    impl ScriptedNoopDriver {
        fn new(exported: &str, repeated: &str) -> Self {
            let hash = Sha256::digest(exported.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let object_get = |body: &str| {
                json!({
                    "object": {"summary": {"id": "object-id", "space_id": "space-id"}},
                    "body": {"text": body, "sha256": hash}
                })
            };
            Self {
                responses: VecDeque::from([
                    object_get(exported),
                    json!({
                        "object": {"id": "object-id", "space_id": "space-id"},
                        "body_sha256": hash
                    }),
                    object_get(repeated),
                ]),
                calls: Vec::new(),
            }
        }
    }

    impl McpDriver for ScriptedNoopDriver {
        fn call_tool<'a>(
            &'a mut self,
            name: &'static str,
            arguments: Value,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
            self.calls.push((name, arguments));
            Box::pin(std::future::ready(self.responses.pop_front().ok_or_else(
                || "scripted Markdown response exhausted".to_owned(),
            )))
        }

        fn call_tool_error<'a>(
            &'a mut self,
            _name: &'static str,
            _arguments: Value,
        ) -> Pin<Box<dyn Future<Output = Result<ToolErrorEvidence, String>> + 'a>> {
            Box::pin(std::future::ready(Err(
                "unexpected scripted error call".to_owned()
            )))
        }

        fn list_tools<'a>(
            &'a mut self,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>> {
            Box::pin(std::future::ready(Err(
                "unexpected scripted list_tools".to_owned()
            )))
        }

        fn list_resources<'a>(
            &'a mut self,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
            Box::pin(std::future::ready(Err(
                "unexpected scripted list_resources".to_owned(),
            )))
        }

        fn list_resource_templates<'a>(
            &'a mut self,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
            Box::pin(std::future::ready(Err(
                "unexpected scripted list_resource_templates".to_owned(),
            )))
        }

        fn read_resource<'a>(
            &'a mut self,
            _uri: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
            Box::pin(std::future::ready(Err(
                "unexpected scripted read_resource".to_owned()
            )))
        }
    }

    // This compile-time assignment ensures callers cannot accidentally regain
    // the large inline dispatcher future that overflowed the live-test worker.
    #[allow(dead_code)]
    fn assert_heap_owned_dispatch<'a, D: McpDriver>(
        driver: &'a mut D,
        ctx: &'a TestContext,
        evidence: &'a mut ScenarioEvidence,
    ) {
        let future: ScenarioFuture<'a> = run_scenario(ScenarioId::Discovery, driver, ctx, evidence);
        std::mem::drop(future);
    }

    const TOOLS: &[&str] = &["server_status", "object_get"];
    const RESOURCES: &[&str] = &["resources/read"];
    const COMPLETE: &[Ownership] = &[
        own(LiveOperation::ServerStatus, ScenarioId::Discovery),
        own(LiveOperation::ObjectGet, ScenarioId::Documents),
        own(LiveOperation::ResourcesRead, ScenarioId::Documents),
    ];

    #[test]
    fn synthetic_missing_operation_fails_deterministically() {
        let error = validate_ownership(TOOLS, RESOURCES, &COMPLETE[..2]).unwrap_err();
        assert_eq!(error, "missing live operation owner: ResourcesRead");
    }

    #[test]
    fn synthetic_missing_optional_tier_fails_deterministically() {
        let expected = HashSet::from([
            (OptionalOperation::MemberList, OptionalEvidenceTier::Fast),
            (
                OptionalOperation::MemberList,
                OptionalEvidenceTier::RealHeadless,
            ),
        ]);
        let owners = [(
            (OptionalOperation::MemberList, OptionalEvidenceTier::Fast),
            optional_fast("members_direct"),
        )];
        let error = validate_typed_ownership(
            &expected,
            owners,
            |_| true,
            OptionalScenarioId::as_str,
            "optional operation/tier",
            "optional scenario",
        )
        .unwrap_err();
        assert_eq!(
            error,
            "missing optional operation/tier owner: (MemberList, RealHeadless)"
        );
    }

    #[test]
    fn optional_scenario_inventory_is_exact_unique_and_typed() {
        let declarations = OptionalScenarioId::EXECUTABLE
            .iter()
            .map(|scenario| OptionalScenarioDeclaration {
                registry: scenario.registry(),
                name: scenario.as_str(),
                tier: scenario.tier(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            optional_scenario_inventory(&declarations)
                .expect("exact typed optional scenario inventory")
                .len(),
            64
        );

        let mut duplicate = declarations.clone();
        duplicate.push(declarations[0]);
        assert!(
            optional_scenario_inventory(&duplicate)
                .unwrap_err()
                .starts_with("duplicate optional scenario identifier")
        );
        assert_eq!(
            optional_scenario_inventory(&declarations[1..]).unwrap_err(),
            format!(
                "missing executable optional scenario: {}",
                declarations[0].name
            )
        );
    }

    #[test]
    fn optional_catalog_parser_covers_the_closed_operation_inventory() {
        let tools = OptionalOperation::ALL
            .into_iter()
            .filter_map(OptionalOperation::tool_name)
            .collect::<Vec<_>>();
        let resources = OptionalOperation::ALL
            .into_iter()
            .filter_map(OptionalOperation::resource_family_name)
            .collect::<Vec<_>>();
        let scenarios = OptionalScenarioId::EXECUTABLE
            .iter()
            .map(|scenario| OptionalScenarioDeclaration {
                registry: scenario.registry(),
                name: scenario.as_str(),
                tier: scenario.tier(),
            })
            .collect::<Vec<_>>();

        assert_eq!(tools.len(), 30);
        assert_eq!(resources.len(), 1);
        validate_optional_live_ownership(&tools, &resources, &scenarios)
            .expect("closed optional catalog parser inventory");
    }

    fn complete_optional_expected() -> HashSet<(OptionalOperation, OptionalEvidenceTier)> {
        OptionalOperation::ALL
            .into_iter()
            .flat_map(|operation| {
                [
                    (operation, OptionalEvidenceTier::Fast),
                    (operation, OptionalEvidenceTier::RealHeadless),
                ]
            })
            .collect()
    }

    fn complete_optional_scenarios() -> HashSet<OptionalScenarioId> {
        OptionalScenarioId::EXECUTABLE.into_iter().collect()
    }

    #[test]
    fn optional_owner_rejects_another_valid_same_tier_scenario() {
        let mut owners = OPTIONAL_LIVE_OWNERSHIP.to_vec();
        let owner = owners
            .iter_mut()
            .find(|owner| {
                owner.operation == OptionalOperation::BodyBlockList
                    && owner.scenario.tier() == OptionalEvidenceTier::Fast
            })
            .expect("body list fast owner");
        owner.scenario = optional_fast("chats_read_direct");

        assert_eq!(
            validate_optional_typed_ownership(
                &complete_optional_expected(),
                &owners,
                &complete_optional_scenarios(),
            )
            .unwrap_err(),
            "optional owner registry mismatch: BodyBlockList is body-blocks, scenario chats_read_direct is chats"
        );
    }

    #[test]
    fn optional_inventory_rejects_swapped_valid_descriptor_scenarios() {
        let mut declarations = OptionalScenarioId::EXECUTABLE
            .iter()
            .map(|scenario| OptionalScenarioDeclaration {
                registry: scenario.registry(),
                name: scenario.as_str(),
                tier: scenario.tier(),
            })
            .collect::<Vec<_>>();
        let chat_index = declarations
            .iter()
            .position(|declaration| declaration.name == "chats_read_direct")
            .expect("chat scenario declaration");
        let member_index = declarations
            .iter()
            .position(|declaration| declaration.name == "members_direct")
            .expect("member scenario declaration");
        let chat_registry = declarations[chat_index].registry;
        declarations[chat_index].registry = declarations[member_index].registry;
        declarations[member_index].registry = chat_registry;

        assert_eq!(
            optional_scenario_inventory(&declarations).unwrap_err(),
            "optional scenario descriptor mismatch: chats_read_direct declared by members, owned by chats"
        );
    }

    #[test]
    fn optional_owner_rejects_correct_broad_workflow_with_wrong_scenario() {
        let mut owners = OPTIONAL_LIVE_OWNERSHIP.to_vec();
        let owner = owners
            .iter_mut()
            .find(|owner| {
                owner.operation == OptionalOperation::SpaceCreate
                    && owner.scenario.tier() == OptionalEvidenceTier::Fast
            })
            .expect("space create fast owner");
        owner.scenario = optional_fast("schema_type_direct");

        assert_eq!(
            validate_optional_typed_ownership(
                &complete_optional_expected(),
                &owners,
                &complete_optional_scenarios(),
            )
            .unwrap_err(),
            "optional scenario does not cover operation: schema_type_direct does not cover SpaceCreate"
        );
    }

    #[test]
    fn optional_ownership_has_exact_two_tiers_for_thirty_one_operations() {
        assert_eq!(OPTIONAL_LIVE_OWNERSHIP.len(), 62);
        let operations = OPTIONAL_LIVE_OWNERSHIP
            .iter()
            .map(|owner| owner.operation)
            .collect::<HashSet<_>>();
        assert_eq!(operations.len(), 31);
        assert!(operations.iter().all(|operation| {
            OPTIONAL_LIVE_OWNERSHIP
                .iter()
                .filter(|owner| owner.operation == *operation)
                .map(|owner| owner.scenario.tier())
                .collect::<HashSet<_>>()
                == HashSet::from([
                    OptionalEvidenceTier::Fast,
                    OptionalEvidenceTier::RealHeadless,
                ])
        }));
    }

    #[test]
    fn scenario_dispatch_storage_is_only_a_fat_pointer() {
        assert_eq!(
            std::mem::size_of::<ScenarioFuture<'static>>(),
            2 * std::mem::size_of::<usize>()
        );
    }

    #[tokio::test]
    async fn markdown_noop_protocol_forwards_exact_export_hash_and_repeats_read() {
        let body = "## Stable\n\nUnicode こんにちは and [link](https://example.com).";
        let mut driver = ScriptedNoopDriver::new(body, body);
        let result = run_markdown_noop_protocol(
            &mut driver,
            MarkdownNoopFixture {
                space_id: "space-id",
                object_id: "object-id",
                exported_body: body,
            },
        )
        .await
        .unwrap();
        assert_eq!(result.before_bytes, body.len());
        assert_eq!(result.after_bytes, body.len());
        assert_eq!(driver.calls.len(), 3);
        assert_eq!(
            driver
                .calls
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>(),
            ["object_get", "object_update", "object_get"]
        );
        assert_eq!(driver.calls[1].1["body_markdown"], body);
        assert_eq!(
            driver.calls[1].1["expected_body_sha256"],
            result.body_sha256
        );
    }

    #[tokio::test]
    async fn markdown_noop_protocol_rejects_lossy_repeated_export() {
        let mut driver = ScriptedNoopDriver::new("before", "after");
        let error = run_markdown_noop_protocol(
            &mut driver,
            MarkdownNoopFixture {
                space_id: "space-id",
                object_id: "object-id",
                exported_body: "before",
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error, "Markdown no-op repeated export byte identity");
    }

    #[test]
    fn duplicate_unknown_and_non_executable_owners_fail() {
        let duplicate = [COMPLETE[0], COMPLETE[0], COMPLETE[1], COMPLETE[2]];
        assert!(
            validate_ownership(TOOLS, RESOURCES, &duplicate)
                .unwrap_err()
                .starts_with("duplicate live operation owner")
        );
        let unknown = [
            COMPLETE[0],
            COMPLETE[1],
            COMPLETE[2],
            own(LiveOperation::ObjectCreate, ScenarioId::Discovery),
        ];
        assert!(
            validate_ownership(TOOLS, RESOURCES, &unknown)
                .unwrap_err()
                .starts_with("unknown live operation owner")
        );
        let non_executable = [
            Ownership {
                operation: LiveOperation::ServerStatus,
                scenario: ScenarioId::SyntheticNonExecutable,
            },
            COMPLETE[1],
            COMPLETE[2],
        ];
        assert!(
            validate_ownership(TOOLS, RESOURCES, &non_executable)
                .unwrap_err()
                .starts_with("non-executable live scenario owner")
        );
    }
}
