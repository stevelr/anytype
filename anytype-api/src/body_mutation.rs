// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Verified, typed mutations for rich document body blocks.
//!
//! A [`BodyEditor`] is always derived from a freshly read
//! [`BodySnapshot`]. Each write is sent at most
//! once and is accepted only after a bounded, fresh `ObjectShow` read proves
//! the exact requested content and position. Callers must reread after an
//! indeterminate transport, timeout, or cancellation outcome; blindly
//! replaying a non-idempotent block write can create duplicates.
//!
//! Bookmark constructors never invoke `BlockBookmarkFetch`. This deliberate
//! policy prevents server-side URL retrieval (and therefore DNS rebinding and
//! private-network SSRF) through this API. They create an unfetched bookmark
//! value whose URL may be rendered as a link by clients.

use anytype_rpc::{
    anytype::rpc::{
        block::{
            create as block_create, list_delete, list_move_to_existing_object, list_set_align,
            list_set_background_color, list_set_vertical_align,
        },
        block_div, block_latex, block_link, block_table, block_text,
    },
    model,
};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use tonic::Request;

use crate::{
    Result,
    body::{
        BlockContent, BlockId, BlockRef, BlockRestrictions, BodyBlock, BodySnapshot, CalloutIcon,
        ColorToken, DividerStyle, EmbedContent, EmbedProcessor, HorizontalAlign, LayoutStyle,
        LinkCardStyle, LinkDescriptionMode, LinkIconSize, MAX_BLOCK_ID_BYTES, MAX_BODY_BLOCKS,
        MAX_EMBED_TEXT_BYTES, MAX_LINK_RELATIONS, MAX_MARKS_PER_TEXT, MAX_TABLE_COLUMNS,
        MAX_TABLE_ROWS, MAX_TEXT_BYTES, MarkKind, TextContent, TextMark, TextStyle, VerticalAlign,
        utf16_len,
    },
    body_rpc::{
        BodyRpcConfig, ResponseLimitKind, acquire_grpc, bounded_body_request, deadline_exhausted,
        observe_first_poll, record_response_limit_rejection,
    },
    client::AnytypeClient,
    error::AnytypeError,
    grpc_util::{GrpcError, ensure_error_ok},
    verify::VerifyConfig,
};

/// Maximum accepted bytes for constructor IDs and relation keys.
const MAX_REFERENCE_BYTES: usize = MAX_BLOCK_ID_BYTES;
/// Maximum accepted bytes for a bookmark URL.
const MAX_BOOKMARK_URL_BYTES: usize = 2_048;

/// Position of a new or moved block relative to a proven snapshot target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertPosition {
    /// Immediately before the target sibling.
    Before,
    /// Immediately after the target sibling.
    After,
    /// First child of the target.
    FirstChild,
    /// Last child of the target.
    LastChild,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PositionExpectation {
    target: BlockId,
    parent: BlockId,
    position: InsertPosition,
}

/// One validated block value accepted by [`BodyEditor::create`].
#[derive(Clone, Debug, PartialEq)]
pub struct NewBlock {
    content: NewBlockContent,
    align: HorizontalAlign,
    vertical_align: VerticalAlign,
    background_color: Option<ColorToken>,
}

#[derive(Clone, Debug, PartialEq)]
enum NewBlockContent {
    Text(TextContent),
    Divider(DividerStyle),
    Bookmark(String),
    Link {
        target_object_id: String,
        card_style: LinkCardStyle,
        icon_size: LinkIconSize,
        description: LinkDescriptionMode,
        relations: Vec<String>,
    },
    Relation(String),
    Embed(EmbedContent),
    TableOfContents,
    Table {
        rows: u32,
        columns: u32,
        with_header_row: bool,
    },
}

impl NewBlock {
    fn text(text: impl Into<String>, style: TextStyle) -> Result<Self> {
        let text = text.into();
        validate_text(&text, &[])?;
        Ok(Self {
            content: NewBlockContent::Text(TextContent {
                text,
                style,
                checked: false,
                color: None,
                icon: None,
                marks: Vec::new(),
            }),
            align: HorizontalAlign::Left,
            vertical_align: VerticalAlign::Top,
            background_color: None,
        })
    }

    /// Creates a paragraph constructor.
    pub fn paragraph(text: impl Into<String>) -> Result<Self> {
        Self::text(text, TextStyle::Paragraph)
    }

    /// Creates a heading constructor for levels 1 through 3.
    pub fn heading(level: u8, text: impl Into<String>) -> Result<Self> {
        let style = match level {
            1 => TextStyle::Header1,
            2 => TextStyle::Header2,
            3 => TextStyle::Header3,
            _ => return validation("heading level must be 1, 2, or 3"),
        };
        Self::text(text, style)
    }

    /// Creates a bulleted-list item constructor.
    pub fn bulleted(text: impl Into<String>) -> Result<Self> {
        Self::text(text, TextStyle::Bulleted)
    }

    /// Creates a numbered-list item constructor.
    pub fn numbered(text: impl Into<String>) -> Result<Self> {
        Self::text(text, TextStyle::Numbered)
    }

    /// Creates a checkbox item constructor.
    pub fn checkbox(text: impl Into<String>, checked: bool) -> Result<Self> {
        let mut block = Self::text(text, TextStyle::Checkbox)?;
        if let NewBlockContent::Text(content) = &mut block.content {
            content.checked = checked;
        }
        Ok(block)
    }

    /// Creates a toggle constructor.
    pub fn toggle(text: impl Into<String>) -> Result<Self> {
        Self::text(text, TextStyle::Toggle)
    }

    /// Creates a callout constructor with an optional emoji or image icon.
    pub fn callout(text: impl Into<String>, icon: Option<CalloutIcon>) -> Result<Self> {
        if let Some(icon) = &icon {
            validate_icon(icon)?;
        }
        let mut block = Self::text(text, TextStyle::Callout)?;
        if let NewBlockContent::Text(content) = &mut block.content {
            content.icon = icon;
        }
        Ok(block)
    }

    /// Creates a quotation constructor.
    pub fn quote(text: impl Into<String>) -> Result<Self> {
        Self::text(text, TextStyle::Quote)
    }

    /// Creates a code-block constructor.
    pub fn code(text: impl Into<String>) -> Result<Self> {
        Self::text(text, TextStyle::Code)
    }

    /// Creates a divider constructor.
    pub fn divider(style: DividerStyle) -> Self {
        Self::non_text(NewBlockContent::Divider(style))
    }

    /// Creates an unfetched bookmark constructor.
    ///
    /// Only absolute HTTP(S) URLs with user-info and fragments absent are
    /// accepted. The URL is stored in the block but is never fetched by this
    /// crate, which is the explicit SSRF-safe v1 networking policy.
    pub fn bookmark(url: impl Into<String>) -> Result<Self> {
        let url = url.into();
        validate_unfetched_url(&url)?;
        Ok(Self::non_text(NewBlockContent::Bookmark(url)))
    }

    /// Creates a link-card constructor.
    pub fn link_card(
        target_object_id: impl Into<String>,
        card_style: LinkCardStyle,
        icon_size: LinkIconSize,
        description: LinkDescriptionMode,
    ) -> Result<Self> {
        let target_object_id = target_object_id.into();
        validate_reference(&target_object_id, "link target object id")?;
        Ok(Self::non_text(NewBlockContent::Link {
            target_object_id,
            card_style,
            icon_size,
            description,
            relations: Vec::new(),
        }))
    }

    /// Sets the bounded relation-key presentation for a link-card constructor.
    pub fn link_relations(mut self, relations: Vec<String>) -> Result<Self> {
        validate_link_relations(&relations)?;
        let NewBlockContent::Link {
            relations: current, ..
        } = &mut self.content
        else {
            return validation("link relations are valid only for link-card blocks");
        };
        *current = relations;
        Ok(self)
    }

    /// Creates a relation-card constructor.
    pub fn relation(key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        validate_reference(&key, "relation key")?;
        Ok(Self::non_text(NewBlockContent::Relation(key)))
    }

    /// Creates a table constructor with bounded dimensions.
    pub fn table(rows: u32, columns: u32, with_header_row: bool) -> Result<Self> {
        if rows == 0 || rows as usize > MAX_TABLE_ROWS {
            return validation(format!("table rows must be within 1..={MAX_TABLE_ROWS}"));
        }
        if columns == 0 || columns as usize > MAX_TABLE_COLUMNS {
            return validation(format!(
                "table columns must be within 1..={MAX_TABLE_COLUMNS}"
            ));
        }
        Ok(Self::non_text(NewBlockContent::Table {
            rows,
            columns,
            with_header_row,
        }))
    }

    /// Creates a LaTeX embed constructor.
    pub fn embed_latex(text: impl Into<String>) -> Result<Self> {
        Self::embed(EmbedProcessor::Latex, text)
    }

    /// Creates a Mermaid embed constructor.
    pub fn embed_mermaid(text: impl Into<String>) -> Result<Self> {
        Self::embed(EmbedProcessor::Mermaid, text)
    }

    /// Creates a `YouTube` embed with a canonical HTTPS watch URL.
    pub fn embed_youtube(url: impl Into<String>) -> Result<Self> {
        let canonical = canonical_youtube_url(&url.into())?;
        Self::embed(EmbedProcessor::Youtube, canonical)
    }

    /// Creates a table-of-contents constructor.
    pub fn table_of_contents() -> Self {
        Self::non_text(NewBlockContent::TableOfContents)
    }

    fn embed(processor: EmbedProcessor, text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        if text.len() > MAX_EMBED_TEXT_BYTES {
            return validation(format!("embed text exceeds {MAX_EMBED_TEXT_BYTES} bytes"));
        }
        Ok(Self::non_text(NewBlockContent::Embed(EmbedContent {
            processor,
            text,
        })))
    }

    fn non_text(content: NewBlockContent) -> Self {
        Self {
            content,
            align: HorizontalAlign::Left,
            vertical_align: VerticalAlign::Top,
            background_color: None,
        }
    }

    /// Replaces inline marks on a text constructor after validating ranges.
    pub fn marks(mut self, marks: Vec<TextMark>) -> Result<Self> {
        let NewBlockContent::Text(content) = &mut self.content else {
            return validation("inline marks are valid only for text blocks");
        };
        validate_text(&content.text, &marks)?;
        content.marks = marks;
        Ok(self)
    }

    /// Sets a text foreground color.
    pub fn text_color(mut self, color: ColorToken) -> Result<Self> {
        let NewBlockContent::Text(content) = &mut self.content else {
            return validation("text color is valid only for text blocks");
        };
        content.color = Some(color);
        Ok(self)
    }

    /// Sets horizontal alignment.
    #[must_use]
    pub fn align(mut self, align: HorizontalAlign) -> Self {
        self.align = align;
        self
    }

    /// Sets vertical alignment.
    #[must_use]
    pub fn vertical_align(mut self, align: VerticalAlign) -> Self {
        self.vertical_align = align;
        self
    }

    /// Sets the block background color.
    #[must_use]
    pub fn background(mut self, color: ColorToken) -> Self {
        self.background_color = Some(color);
        self
    }
}

/// One exact, single-RPC block update.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum BlockChange {
    /// Replaces text and its complete mark list, preserving other text fields.
    Text { text: String, marks: Vec<TextMark> },
    /// Changes a text block's style.
    TextStyle(TextStyle),
    /// Changes a checkbox block's checked state.
    Checked(bool),
    /// Sets or clears a text foreground color.
    TextColor(Option<ColorToken>),
    /// Sets or clears a callout icon.
    CalloutIcon(Option<CalloutIcon>),
    /// Replaces embed text and processor.
    Embed(EmbedContent),
    /// Changes a divider's closed style.
    DividerStyle(DividerStyle),
    /// Replaces a link card's complete appearance.
    LinkAppearance {
        /// Card presentation style.
        card_style: LinkCardStyle,
        /// Icon size.
        icon_size: LinkIconSize,
        /// Description mode.
        description: LinkDescriptionMode,
        /// Bounded relation keys shown by the card.
        relations: Vec<String>,
    },
    /// Changes horizontal alignment.
    HorizontalAlign(HorizontalAlign),
    /// Changes vertical alignment.
    VerticalAlign(VerticalAlign),
    /// Sets or clears block background color.
    Background(Option<ColorToken>),
}

/// A requested body operation for bounded sequential execution.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum BodyOp {
    /// Create a block relative to `target`.
    Create {
        block: NewBlock,
        target: BlockId,
        position: InsertPosition,
    },
    /// Append a block to the body root.
    Append { block: NewBlock },
    /// Apply one exact update.
    Update {
        block_id: BlockId,
        change: BlockChange,
    },
    /// Delete one block.
    Delete { block_id: BlockId },
    /// Move one block relative to another block.
    Move {
        block_id: BlockId,
        target: BlockId,
        position: InsertPosition,
    },
}

/// Verified evidence returned for one completed mutation.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct BlockMutation {
    /// Fully qualified affected block addresses.
    pub affected: Vec<BlockRef>,
    /// Fresh `ObjectShow` evidence proving the requested final state.
    pub snapshot: BodySnapshot,
}

/// Failure evidence for the first operation not proved by a batch.
#[derive(Debug)]
#[non_exhaustive]
pub struct FailedBodyOp {
    /// Zero-based operation index.
    pub index: usize,
    /// Operation that failed or became indeterminate.
    pub operation: BodyOp,
    /// Secret-safe classified API error.
    pub error: AnytypeError,
}

/// Bounded, explicitly non-transactional batch result.
#[derive(Debug)]
#[non_exhaustive]
pub struct BodyBatchOutcome {
    /// Individually verified operations completed before the first failure.
    pub applied: Vec<BlockMutation>,
    /// First failure, if any.
    pub failed: Option<FailedBodyOp>,
    /// Operations not attempted after the first failure.
    pub not_attempted: Vec<BodyOp>,
}

/// A mutation surface pinned to a proven snapshot context.
#[derive(Debug)]
pub struct BodyEditor<'a> {
    client: &'a AnytypeClient,
    snapshot: &'a BodySnapshot,
    verify: VerifyConfig,
    rpc: BodyRpcConfig,
}

impl BodySnapshot {
    /// Starts a verified mutation session bound to this snapshot's object and space.
    #[must_use]
    pub fn edit<'a>(&'a self, client: &'a AnytypeClient) -> BodyEditor<'a> {
        BodyEditor {
            client,
            snapshot: self,
            verify: client
                .config
                .get_verify_config()
                .cloned()
                .unwrap_or_default(),
            rpc: BodyRpcConfig::default(),
        }
    }
}

impl BodyEditor<'_> {
    fn bounded_request<T>(&self, request: T, token: &str) -> Result<Request<T>> {
        bounded_body_request(Request::new(request), token, &self.rpc, self.verify.timeout)
    }

    /// Overrides the finite read-after-write verification policy.
    #[must_use]
    pub fn verify_with(mut self, verify: VerifyConfig) -> Self {
        self.verify = verify;
        self
    }

    /// Returns the verification configuration for downstream contract tests.
    #[cfg(feature = "test-fixtures")]
    #[doc(hidden)]
    #[must_use]
    pub fn fixture_verify_config(&self) -> &VerifyConfig {
        &self.verify
    }

    /// Uses one finite gRPC configuration for acquisition, the write, every
    /// verification show/close pair, and fallback cleanup.
    ///
    /// Pass the same configuration used by the originating body read to share
    /// one absolute deadline and one exact metrics observer.
    #[must_use]
    pub fn rpc_config(mut self, config: BodyRpcConfig) -> Self {
        self.rpc = config;
        self
    }

    /// Creates one block and proves its identity, content, and exact position.
    pub async fn create(
        &self,
        block: NewBlock,
        target: &BlockId,
        position: InsertPosition,
    ) -> Result<BlockMutation> {
        self.create_from(self.snapshot, block, target, position)
            .await
    }

    /// Appends one block to the body root and proves its final state.
    pub async fn append(&self, block: NewBlock) -> Result<BlockMutation> {
        self.create(block, &self.snapshot.root_id, InsertPosition::LastChild)
            .await
    }

    /// Applies one exact update and proves the fresh rich state.
    pub async fn update(&self, id: &BlockId, change: BlockChange) -> Result<BlockMutation> {
        self.update_from(self.snapshot, id, change).await
    }

    /// Deletes one block and proves it is absent from a fresh snapshot.
    pub async fn delete(&self, id: &BlockId) -> Result<BlockMutation> {
        self.delete_from(self.snapshot, id).await
    }

    /// Moves one block and proves its exact fresh parent/sibling relation.
    pub async fn move_block(
        &self,
        id: &BlockId,
        target: &BlockId,
        position: InsertPosition,
    ) -> Result<BlockMutation> {
        self.move_from(self.snapshot, id, target, position).await
    }

    /// Executes at most [`MAX_BODY_BLOCKS`] operations sequentially.
    ///
    /// This is not a transaction. It stops on the first failure and preserves
    /// a verified receipt for every completed prefix operation.
    pub async fn apply_all(&self, operations: Vec<BodyOp>) -> Result<BodyBatchOutcome> {
        if operations.len() > MAX_BODY_BLOCKS {
            return validation(format!("body batch exceeds {MAX_BODY_BLOCKS} operations"));
        }
        let mut current = self.snapshot.clone();
        let mut applied = Vec::with_capacity(operations.len());
        let mut iter = operations.into_iter().enumerate();
        while let Some((index, operation)) = iter.next() {
            let result = match &operation {
                BodyOp::Create {
                    block,
                    target,
                    position,
                } => {
                    self.create_from(&current, block.clone(), target, *position)
                        .await
                }
                BodyOp::Append { block } => {
                    let root = current.root_id.clone();
                    self.create_from(&current, block.clone(), &root, InsertPosition::LastChild)
                        .await
                }
                BodyOp::Update { block_id, change } => {
                    self.update_from(&current, block_id, change.clone()).await
                }
                BodyOp::Delete { block_id } => self.delete_from(&current, block_id).await,
                BodyOp::Move {
                    block_id,
                    target,
                    position,
                } => self.move_from(&current, block_id, target, *position).await,
            };
            match result {
                Ok(receipt) => {
                    current = receipt.snapshot.clone();
                    applied.push(receipt);
                }
                Err(error) => {
                    return Ok(BodyBatchOutcome {
                        applied,
                        failed: Some(FailedBodyOp {
                            index,
                            operation,
                            error,
                        }),
                        not_attempted: iter.map(|(_, op)| op).collect(),
                    });
                }
            }
        }
        Ok(BodyBatchOutcome {
            applied,
            failed: None,
            not_attempted: Vec::new(),
        })
    }

    async fn create_from(
        &self,
        snapshot: &BodySnapshot,
        block: NewBlock,
        target: &BlockId,
        position: InsertPosition,
    ) -> Result<BlockMutation> {
        validate_new_block_for_create(&block)?;
        validate_anchor(snapshot, target, position)?;
        let expectation = position_expectation(snapshot, target, position)?;
        let (wire_target, wire_position) = wire_anchor(snapshot, target, position)?;
        let new_id = match self
            .send_create(block.clone(), &wire_target, wire_position)
            .await
        {
            Ok(id) => id,
            Err(error) => return Err(self.with_observed_evidence(error).await),
        };
        if snapshot.get(&new_id).is_some() {
            return Err(AnytypeError::BodyMutationIndeterminate {
                object_id: snapshot.object_id.clone(),
                block_id: Some(new_id),
                attempts: 0,
                timeout: self.verify.timeout,
                observed: Some(Box::new(snapshot.clone())),
            });
        }
        let expected = block.clone();
        let fresh = self
            .verify_snapshot(Some(&new_id), |fresh| {
                fresh
                    .get(&new_id)
                    .is_some_and(|actual| new_block_matches(&expected, actual))
                    && table_shape_matches(fresh, &new_id, &expected)
                    && position_matches(fresh, &new_id, &expectation)
            })
            .await?;
        Ok(receipt(fresh, vec![new_id]))
    }

    async fn update_from(
        &self,
        snapshot: &BodySnapshot,
        id: &BlockId,
        change: BlockChange,
    ) -> Result<BlockMutation> {
        let block = update_target(snapshot, id)?;
        validate_change(block, &change)?;
        let before = block.clone();
        if let Err(error) = self.send_update(id, &change).await {
            return Err(self.with_observed_evidence(error).await);
        }
        let expected = change.clone();
        let fresh = self
            .verify_snapshot(Some(id), |snapshot| {
                snapshot
                    .get(id)
                    .is_some_and(|block| change_matches(&before, &expected, block))
            })
            .await?;
        Ok(receipt(fresh, vec![id.clone()]))
    }

    async fn delete_from(&self, snapshot: &BodySnapshot, id: &BlockId) -> Result<BlockMutation> {
        if id == &snapshot.root_id {
            return validation("the body root cannot be deleted");
        }
        delete_target(snapshot, id)?;
        if let Err(error) = self.send_delete(id).await {
            return Err(self.with_observed_evidence(error).await);
        }
        let fresh = self
            .verify_snapshot(Some(id), |snapshot| snapshot.get(id).is_none())
            .await?;
        Ok(receipt(fresh, vec![id.clone()]))
    }

    async fn move_from(
        &self,
        snapshot: &BodySnapshot,
        id: &BlockId,
        target: &BlockId,
        position: InsertPosition,
    ) -> Result<BlockMutation> {
        if id == &snapshot.root_id || id == target {
            return validation("a move requires distinct non-root block and target ids");
        }
        let before = move_source(snapshot, id)?.clone();
        validate_anchor(snapshot, target, position)?;
        let expectation = position_expectation(snapshot, target, position)?;
        if is_descendant(snapshot, id, target) {
            return validation("a block cannot be moved into its own subtree");
        }
        let (wire_target, wire_position) = wire_anchor(snapshot, target, position)?;
        if let Err(error) = self.send_move(id, &wire_target, wire_position).await {
            return Err(self.with_observed_evidence(error).await);
        }
        let fresh = self
            .verify_snapshot(Some(id), |fresh| {
                fresh
                    .get(id)
                    .is_some_and(|actual| same_block_state(&before, actual))
                    && position_matches(fresh, id, &expectation)
            })
            .await?;
        Ok(receipt(fresh, vec![id.clone()]))
    }

    async fn send_create(
        &self,
        block: NewBlock,
        target: &BlockId,
        position: model::block::Position,
    ) -> Result<BlockId> {
        let grpc = acquire_grpc(self.client, &self.rpc).await?;
        let mut commands = self.rpc.mutation_commands(&grpc);
        let response = if let NewBlockContent::Table {
            rows,
            columns,
            with_header_row,
        } = block.content
        {
            let request = block_table::create::Request {
                context_id: self.snapshot.object_id.clone(),
                target_id: target.to_string(),
                position: position as i32,
                rows,
                columns,
                with_header_row,
            };
            let request = self.bounded_request(request, grpc.token())?;
            let response = poll_tonic_write_once(
                &self.verify,
                &self.rpc,
                &self.snapshot.object_id,
                None,
                commands.block_table_create(request),
            )
            .await?;
            mutation_response_ok(
                response.error.as_ref(),
                "table create",
                &self.snapshot.object_id,
                None,
                self.verify.timeout,
            )?;
            response.block_id
        } else {
            let request = block_create::Request {
                context_id: self.snapshot.object_id.clone(),
                target_id: target.to_string(),
                block: Some(block_to_proto(block)),
                position: position as i32,
            };
            let request = self.bounded_request(request, grpc.token())?;
            let response = poll_tonic_write_once(
                &self.verify,
                &self.rpc,
                &self.snapshot.object_id,
                None,
                commands.block_create(request),
            )
            .await?;
            mutation_response_ok(
                response.error.as_ref(),
                "block create",
                &self.snapshot.object_id,
                None,
                self.verify.timeout,
            )?;
            response.block_id
        };
        BlockId::try_from(response)
            .map_err(|_| indeterminate(&self.snapshot.object_id, None, self.verify.timeout))
    }

    async fn send_update(&self, id: &BlockId, change: &BlockChange) -> Result<()> {
        let grpc = acquire_grpc(self.client, &self.rpc).await?;
        let mut commands = self.rpc.mutation_commands(&grpc);
        let context_id = self.snapshot.object_id.clone();
        let block_id = id.to_string();
        let token = grpc.token();
        macro_rules! dispatch {
            ($future:expr, $action:literal) => {{
                let response = poll_tonic_write_once(
                    &self.verify,
                    &self.rpc,
                    &self.snapshot.object_id,
                    Some(id),
                    $future,
                )
                .await?;
                mutation_response_ok(
                    response.error.as_ref(),
                    $action,
                    &self.snapshot.object_id,
                    Some(id),
                    self.verify.timeout,
                )
            }};
        }
        match change {
            BlockChange::Text { text, marks } => {
                let request = block_text::set_text::Request {
                    context_id,
                    block_id,
                    text: text.clone(),
                    marks: Some(marks_to_proto(marks)),
                    selected_text_range: None,
                };
                let request = self.bounded_request(request, token)?;
                dispatch!(commands.block_text_set_text(request), "block text update")
            }
            BlockChange::TextStyle(style) => {
                let request = block_text::set_style::Request {
                    context_id,
                    block_id,
                    style: text_style_proto(*style),
                };
                let request = self.bounded_request(request, token)?;
                dispatch!(commands.block_text_set_style(request), "block style update")
            }
            BlockChange::Checked(checked) => {
                let request = block_text::set_checked::Request {
                    context_id,
                    block_id,
                    checked: *checked,
                };
                let request = self.bounded_request(request, token)?;
                dispatch!(commands.block_text_set_checked(request), "checkbox update")
            }
            BlockChange::TextColor(color) => {
                let request = block_text::set_color::Request {
                    context_id,
                    block_id,
                    color: color.as_ref().map_or_else(String::new, ToString::to_string),
                };
                let request = self.bounded_request(request, token)?;
                dispatch!(commands.block_text_set_color(request), "text color update")
            }
            BlockChange::CalloutIcon(icon) => {
                let (icon_image, icon_emoji) = icon_parts(icon.as_ref());
                let request = block_text::set_icon::Request {
                    context_id,
                    block_id,
                    icon_image,
                    icon_emoji,
                };
                let request = self.bounded_request(request, token)?;
                dispatch!(commands.block_text_set_icon(request), "callout icon update")
            }
            BlockChange::Embed(embed) => {
                let request = block_latex::set_text::Request {
                    context_id,
                    block_id,
                    text: embed.text.clone(),
                    processor: embed_processor_proto(embed.processor),
                };
                let request = self.bounded_request(request, token)?;
                dispatch!(commands.block_latex_set_text(request), "embed update")
            }
            BlockChange::DividerStyle(style) => {
                let request = block_div::list_set_style::Request {
                    context_id,
                    block_ids: vec![block_id],
                    style: divider_style_proto(*style),
                };
                let request = self.bounded_request(request, token)?;
                dispatch!(
                    commands.block_div_list_set_style(request),
                    "divider style update"
                )
            }
            BlockChange::LinkAppearance {
                card_style,
                icon_size,
                description,
                relations,
            } => {
                let request = block_link::list_set_appearance::Request {
                    context_id,
                    block_ids: vec![block_id],
                    icon_size: link_icon_size_proto(*icon_size),
                    card_style: link_card_style_proto(*card_style),
                    description: link_description_proto(*description),
                    relations: relations.clone(),
                };
                let request = self.bounded_request(request, token)?;
                dispatch!(
                    commands.block_link_list_set_appearance(request),
                    "link appearance update"
                )
            }
            BlockChange::HorizontalAlign(align) => {
                let request = list_set_align::Request {
                    context_id,
                    block_ids: vec![block_id],
                    align: horizontal_align_proto(*align),
                };
                let request = self.bounded_request(request, token)?;
                dispatch!(
                    commands.block_list_set_align(request),
                    "horizontal alignment update"
                )
            }
            BlockChange::VerticalAlign(vertical_align) => {
                let request = list_set_vertical_align::Request {
                    context_id,
                    block_ids: vec![block_id],
                    vertical_align: vertical_align_proto(*vertical_align),
                };
                let request = self.bounded_request(request, token)?;
                dispatch!(
                    commands.block_list_set_vertical_align(request),
                    "vertical alignment update"
                )
            }
            BlockChange::Background(color) => {
                let request = list_set_background_color::Request {
                    context_id,
                    block_ids: vec![block_id],
                    color: color.as_ref().map_or_else(String::new, ToString::to_string),
                };
                let request = self.bounded_request(request, token)?;
                dispatch!(
                    commands.block_list_set_background_color(request),
                    "background update"
                )
            }
        }
    }

    async fn send_delete(&self, id: &BlockId) -> Result<()> {
        let grpc = acquire_grpc(self.client, &self.rpc).await?;
        let mut commands = self.rpc.mutation_commands(&grpc);
        let request = list_delete::Request {
            context_id: self.snapshot.object_id.clone(),
            block_ids: vec![id.to_string()],
        };
        let request = self.bounded_request(request, grpc.token())?;
        let response = poll_tonic_write_once(
            &self.verify,
            &self.rpc,
            &self.snapshot.object_id,
            Some(id),
            commands.block_list_delete(request),
        )
        .await?;
        mutation_response_ok(
            response.error.as_ref(),
            "block delete",
            &self.snapshot.object_id,
            Some(id),
            self.verify.timeout,
        )
    }

    async fn send_move(
        &self,
        id: &BlockId,
        target: &BlockId,
        position: model::block::Position,
    ) -> Result<()> {
        let grpc = acquire_grpc(self.client, &self.rpc).await?;
        let mut commands = self.rpc.mutation_commands(&grpc);
        let request = list_move_to_existing_object::Request {
            context_id: self.snapshot.object_id.clone(),
            block_ids: vec![id.to_string()],
            target_context_id: self.snapshot.object_id.clone(),
            drop_target_id: target.to_string(),
            position: position as i32,
        };
        let request = self.bounded_request(request, grpc.token())?;
        let response = poll_tonic_write_once(
            &self.verify,
            &self.rpc,
            &self.snapshot.object_id,
            Some(id),
            commands.block_list_move_to_existing_object(request),
        )
        .await?;
        mutation_response_ok(
            response.error.as_ref(),
            "block move",
            &self.snapshot.object_id,
            Some(id),
            self.verify.timeout,
        )
    }

    async fn verify_snapshot(
        &self,
        id: Option<&BlockId>,
        ready: impl FnMut(&BodySnapshot) -> bool,
    ) -> Result<BodySnapshot> {
        verify_snapshot_with(
            &self.verify,
            &self.rpc,
            &self.snapshot.object_id,
            id,
            || {
                self.client
                    .blocks()
                    .body(&self.snapshot.space_id, &self.snapshot.object_id)
                    .rpc_config(self.rpc.clone())
                    .fetch()
            },
            ready,
        )
        .await
    }

    async fn with_observed_evidence(&self, error: AnytypeError) -> AnytypeError {
        let AnytypeError::BodyMutationIndeterminate {
            object_id,
            block_id,
            attempts,
            timeout,
            observed: None,
        } = error
        else {
            return error;
        };
        let Some(timeout) = self.rpc.timeout_for(self.verify.timeout) else {
            return AnytypeError::BodyMutationIndeterminate {
                object_id,
                block_id,
                attempts,
                timeout,
                observed: None,
            };
        };
        let observed = tokio::time::timeout(
            timeout,
            self.client
                .blocks()
                .body(&self.snapshot.space_id, &self.snapshot.object_id)
                .rpc_config(self.rpc.clone())
                .fetch(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .map(Box::new);
        AnytypeError::BodyMutationIndeterminate {
            object_id,
            block_id,
            attempts,
            timeout,
            observed,
        }
    }
}

async fn verify_snapshot_with<Fetch, Future, Ready>(
    verify: &VerifyConfig,
    rpc: &BodyRpcConfig,
    object_id: &str,
    block_id: Option<&BlockId>,
    mut fetch: Fetch,
    mut ready: Ready,
) -> Result<BodySnapshot>
where
    Fetch: FnMut() -> Future,
    Future: std::future::Future<Output = Result<BodySnapshot>>,
    Ready: FnMut(&BodySnapshot) -> bool,
{
    let start = Instant::now();
    let max_attempts = verify.effective_max_attempts();
    let mut attempts = 0usize;
    let mut delay = verify.initial_delay;
    let mut observed = None;
    while attempts < max_attempts {
        let Some(verify_remaining) = verify.timeout.checked_sub(start.elapsed()) else {
            break;
        };
        let Some(deadline_remaining) = rpc.remaining() else {
            break;
        };
        let remaining = verify_remaining.min(deadline_remaining);
        if remaining.is_zero() {
            break;
        }
        if !delay.is_zero()
            && tokio::time::timeout(remaining, tokio::time::sleep(delay))
                .await
                .is_err()
        {
            break;
        }
        attempts += 1;
        let Some(verify_remaining) = verify.timeout.checked_sub(start.elapsed()) else {
            break;
        };
        let Some(deadline_remaining) = rpc.remaining() else {
            break;
        };
        let remaining = verify_remaining.min(deadline_remaining);
        if let Ok(Ok(snapshot)) = tokio::time::timeout(remaining, fetch()).await {
            if ready(&snapshot) {
                return Ok(snapshot);
            }
            observed = Some(Box::new(snapshot));
        }
        delay = delay.saturating_mul(2).min(verify.max_delay);
    }
    Err(AnytypeError::BodyMutationIndeterminate {
        object_id: object_id.to_owned(),
        block_id: block_id.cloned(),
        attempts,
        timeout: verify.timeout,
        observed,
    })
}

async fn poll_tonic_write_once<T, Future>(
    verify: &VerifyConfig,
    rpc: &BodyRpcConfig,
    object_id: &str,
    block_id: Option<&BlockId>,
    future: Future,
) -> Result<T>
where
    Future: std::future::Future<Output = std::result::Result<tonic::Response<T>, tonic::Status>>,
{
    if verify.timeout.is_zero() {
        return validation("body verification timeout must be nonzero");
    }
    let timeout = rpc
        .timeout_for(verify.timeout)
        .ok_or_else(deadline_exhausted)?;
    let response = tokio::time::timeout(
        timeout,
        observe_first_poll(future, || rpc.metrics().record_write_poll()),
    )
    .await
    .map_err(|_| indeterminate(object_id, block_id, verify.timeout))?
    .map_err(|status| {
        let _ = record_response_limit_rejection(
            rpc,
            &status,
            rpc.non_show_response_limit(),
            ResponseLimitKind::Mutation,
        );
        indeterminate(object_id, block_id, verify.timeout)
    })?;
    Ok(response.into_inner())
}

fn mutation_response_ok<T: GrpcError>(
    error: Option<&T>,
    action: &'static str,
    object_id: &str,
    block_id: Option<&BlockId>,
    timeout: std::time::Duration,
) -> Result<()> {
    let code = error.map_or(0, GrpcError::code);
    match ensure_error_ok(error, action) {
        Ok(()) => Ok(()),
        Err(_) if code == 2 => validation(format!("{action} rejected invalid input")),
        Err(_) => Err(indeterminate(object_id, block_id, timeout)),
    }
}

fn indeterminate(
    object_id: &str,
    block_id: Option<&BlockId>,
    timeout: std::time::Duration,
) -> AnytypeError {
    AnytypeError::BodyMutationIndeterminate {
        object_id: object_id.to_owned(),
        block_id: block_id.cloned(),
        attempts: 0,
        timeout,
        observed: None,
    }
}

fn receipt(snapshot: BodySnapshot, ids: Vec<BlockId>) -> BlockMutation {
    let affected = ids
        .into_iter()
        .map(|block_id| BlockRef {
            space_id: snapshot.space_id.clone(),
            object_id: snapshot.object_id.clone(),
            block_id,
        })
        .collect();
    BlockMutation { affected, snapshot }
}

fn validation<T>(message: impl Into<String>) -> Result<T> {
    Err(AnytypeError::Validation {
        message: message.into(),
    })
}

fn validate_reference(value: &str, name: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_REFERENCE_BYTES || value.chars().any(char::is_control)
    {
        return validation(format!(
            "{name} must be nonempty, bounded, control-free text"
        ));
    }
    Ok(())
}

fn validate_link_relations(relations: &[String]) -> Result<()> {
    if relations.len() > MAX_LINK_RELATIONS {
        return validation(format!(
            "link appearance exceeds {MAX_LINK_RELATIONS} relation keys"
        ));
    }
    for relation in relations {
        validate_reference(relation, "link relation key")?;
    }
    Ok(())
}

fn validate_icon(icon: &CalloutIcon) -> Result<()> {
    match icon {
        CalloutIcon::Emoji(value) => {
            if value.is_empty() || value.len() > 64 || value.chars().any(char::is_control) {
                return validation(
                    "callout emoji must be nonempty, at most 64 bytes, and control-free",
                );
            }
        }
        CalloutIcon::Image(value) => validate_reference(value, "callout image id")?,
    }
    Ok(())
}

fn validate_text(text: &str, marks: &[TextMark]) -> Result<()> {
    if text.len() > MAX_TEXT_BYTES {
        return validation(format!("text exceeds {MAX_TEXT_BYTES} bytes"));
    }
    if marks.len() > MAX_MARKS_PER_TEXT {
        return validation(format!("text has more than {MAX_MARKS_PER_TEXT} marks"));
    }
    let length = utf16_len(text);
    for mark in marks {
        if mark.range.start > mark.range.end
            || mark.range.start > length
            || mark.range.end > length
            || mark.range.to_byte_range(text).is_none()
        {
            return validation("text mark range is outside the UTF-16 text length");
        }
        match &mark.kind {
            MarkKind::Link { url } => {
                if url.is_empty()
                    || url.len() > MAX_BOOKMARK_URL_BYTES
                    || url.chars().any(char::is_control)
                {
                    return validation("mark link must be nonempty, bounded, control-free text");
                }
            }
            MarkKind::Mention { object_id } | MarkKind::Object { object_id } => {
                validate_reference(object_id, "mark object id")?
            }
            MarkKind::Emoji { emoji }
                if emoji.is_empty() || emoji.len() > 64 || emoji.chars().any(char::is_control) =>
            {
                return validation(
                    "mark emoji must be nonempty, at most 64 bytes, and control-free",
                );
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_unfetched_url(value: &str) -> Result<()> {
    if value.len() > MAX_BOOKMARK_URL_BYTES || value.chars().any(char::is_control) {
        return validation("bookmark URL must be bounded and control-free");
    }
    let parsed = url::Url::parse(value).map_err(|_| AnytypeError::Validation {
        message: "bookmark URL must be absolute HTTP(S)".to_owned(),
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return validation(
            "bookmark URL must be absolute HTTP(S), with no credentials or fragment",
        );
    }
    Ok(())
}

fn canonical_youtube_url(value: &str) -> Result<String> {
    if value.len() > MAX_BOOKMARK_URL_BYTES || value.chars().any(char::is_control) {
        return validation("YouTube URL must be bounded and control-free");
    }
    let parsed = url::Url::parse(value).map_err(|_| AnytypeError::Validation {
        message: "YouTube URL is invalid".to_owned(),
    })?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.fragment().is_some()
    {
        return validation("YouTube URL must use HTTPS without credentials, port, or fragment");
    }
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let video_id = if matches!(host.as_str(), "youtube.com" | "www.youtube.com")
        && parsed.path() == "/watch"
    {
        let mut pairs = parsed.query_pairs();
        match (pairs.next(), pairs.next()) {
            (Some((key, value)), None) if key == "v" => Some(value.into_owned()),
            _ => None,
        }
    } else if host == "youtu.be" && parsed.query().is_none() {
        let mut segments = parsed.path_segments();
        match segments.as_mut().and_then(Iterator::next) {
            Some(segment)
                if !segment.is_empty()
                    && segments
                        .as_mut()
                        .is_some_and(|remaining| remaining.next().is_none()) =>
            {
                Some(segment.to_owned())
            }
            _ => None,
        }
    } else {
        None
    };
    let Some(video_id) = video_id else {
        return validation("YouTube URL host or shape is not allowed");
    };
    if !(6..=20).contains(&video_id.len())
        || !video_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return validation("YouTube video id is invalid");
    }
    Ok(format!("https://www.youtube.com/watch?v={video_id}"))
}

fn typed_block<'a>(snapshot: &'a BodySnapshot, id: &BlockId) -> Result<&'a BodyBlock> {
    let block = snapshot.get(id).ok_or_else(|| AnytypeError::NotFound {
        obj_type: "body block".to_owned(),
        key: id.to_string(),
    })?;
    if structurally_read_only(block) {
        return validation("body block is structurally read-only");
    }
    Ok(block)
}

fn structurally_read_only(block: &BodyBlock) -> bool {
    matches!(
        &block.content,
        BlockContent::Unsupported(_)
            | BlockContent::Layout(_)
            | BlockContent::Table
            | BlockContent::TableRow { .. }
            | BlockContent::TableColumn
            | BlockContent::FeaturedRelations
            | BlockContent::File(_)
            | BlockContent::Text(TextContent {
                style: TextStyle::Title | TextStyle::Description | TextStyle::Header4,
                ..
            })
    )
}

fn update_target<'a>(snapshot: &'a BodySnapshot, id: &BlockId) -> Result<&'a BodyBlock> {
    let block = typed_block(snapshot, id)?;
    if block.restrictions.edit {
        return validation("body block is edit-restricted");
    }
    Ok(block)
}

fn delete_target<'a>(snapshot: &'a BodySnapshot, id: &BlockId) -> Result<&'a BodyBlock> {
    let block = typed_block(snapshot, id)?;
    if block.restrictions.remove {
        return validation("body block is remove-restricted");
    }
    Ok(block)
}

fn move_source<'a>(snapshot: &'a BodySnapshot, id: &BlockId) -> Result<&'a BodyBlock> {
    let block = typed_block(snapshot, id)?;
    if block.restrictions.drag {
        return validation("body block is drag-restricted");
    }
    Ok(block)
}

fn validate_anchor(
    snapshot: &BodySnapshot,
    target: &BlockId,
    position: InsertPosition,
) -> Result<()> {
    let block = snapshot.get(target).ok_or_else(|| AnytypeError::NotFound {
        obj_type: "body block target".to_owned(),
        key: target.to_string(),
    })?;
    if block.restrictions.drop_on || (target != &snapshot.root_id && structurally_read_only(block))
    {
        return validation("body block target is restricted or structurally read-only");
    }
    if matches!(position, InsertPosition::Before | InsertPosition::After)
        && target == &snapshot.root_id
    {
        return validation("the body root cannot be used as a sibling target");
    }
    if matches!(position, InsertPosition::Before | InsertPosition::After) {
        let parent = parent_of(snapshot, target).ok_or_else(|| AnytypeError::Validation {
            message: "body block sibling target has no proven parent".to_owned(),
        })?;
        if parent.restrictions.drop_on
            || (parent.id != snapshot.root_id && structurally_read_only(parent))
        {
            return validation("body block target parent is structurally read-only");
        }
    }
    if position == InsertPosition::FirstChild
        && let Some(first) = snapshot.children(target).first()
    {
        let anchor = snapshot
            .get(first)
            .ok_or_else(|| AnytypeError::Validation {
                message: "body block first-child anchor is missing".to_owned(),
            })?;
        if anchor.restrictions.drop_on || structurally_read_only(anchor) {
            return validation("body block first-child anchor is structurally read-only");
        }
    }
    Ok(())
}

fn validate_new_block_for_create(block: &NewBlock) -> Result<()> {
    if matches!(block.content, NewBlockContent::Table { .. })
        && (block.align != HorizontalAlign::Left
            || block.vertical_align != VerticalAlign::Top
            || block.background_color.is_some())
    {
        return validation(
            "table creation accepts only default alignment/background; update after verification",
        );
    }
    Ok(())
}

fn parent_of<'a>(snapshot: &'a BodySnapshot, id: &BlockId) -> Option<&'a BodyBlock> {
    snapshot
        .iter()
        .find(|block| block.children.iter().any(|child| child == id))
}

fn wire_anchor(
    snapshot: &BodySnapshot,
    target: &BlockId,
    position: InsertPosition,
) -> Result<(BlockId, model::block::Position)> {
    match position {
        InsertPosition::Before => Ok((target.clone(), model::block::Position::Top)),
        InsertPosition::After => Ok((target.clone(), model::block::Position::Bottom)),
        InsertPosition::FirstChild => snapshot.children(target).first().cloned().map_or_else(
            || Ok((target.clone(), model::block::Position::Inner)),
            |first| Ok((first, model::block::Position::Top)),
        ),
        InsertPosition::LastChild => Ok((target.clone(), model::block::Position::Inner)),
    }
}

fn position_matches(
    snapshot: &BodySnapshot,
    id: &BlockId,
    expectation: &PositionExpectation,
) -> bool {
    let Some(parent) = snapshot.get(&expectation.parent) else {
        return false;
    };
    match expectation.position {
        InsertPosition::Before | InsertPosition::After => {
            if parent_of(snapshot, &expectation.target).map(|block| &block.id)
                != Some(&expectation.parent)
            {
                return false;
            }
            let Some(target_index) = parent
                .children
                .iter()
                .position(|child| child == &expectation.target)
            else {
                return false;
            };
            let expected = if expectation.position == InsertPosition::Before {
                target_index.checked_sub(1)
            } else {
                target_index.checked_add(1)
            };
            expected
                .and_then(|index| parent.children.get(index))
                .is_some_and(|child| child == id)
        }
        InsertPosition::FirstChild => parent.children.first().is_some_and(|child| child == id),
        InsertPosition::LastChild => parent.children.last().is_some_and(|child| child == id),
    }
}

fn position_expectation(
    snapshot: &BodySnapshot,
    target: &BlockId,
    position: InsertPosition,
) -> Result<PositionExpectation> {
    let parent = match position {
        InsertPosition::Before | InsertPosition::After => parent_of(snapshot, target)
            .map(|block| block.id.clone())
            .ok_or_else(|| AnytypeError::Validation {
                message: "body block sibling target has no proven parent".to_owned(),
            })?,
        InsertPosition::FirstChild | InsertPosition::LastChild => target.clone(),
    };
    Ok(PositionExpectation {
        target: target.clone(),
        parent,
        position,
    })
}

fn is_descendant(snapshot: &BodySnapshot, ancestor: &BlockId, possible_child: &BlockId) -> bool {
    let mut stack = snapshot.children(ancestor).to_vec();
    while let Some(id) = stack.pop() {
        if &id == possible_child {
            return true;
        }
        stack.extend_from_slice(snapshot.children(&id));
    }
    false
}

fn validate_change(block: &BodyBlock, change: &BlockChange) -> Result<()> {
    match change {
        BlockChange::Text { text, marks } => {
            if !matches!(block.content, BlockContent::Text(_)) {
                return validation("text update requires a text block");
            }
            validate_text(text, marks)
        }
        BlockChange::TextStyle(style) => {
            if !matches!(block.content, BlockContent::Text(_)) {
                return validation("style update requires a text block");
            }
            if matches!(
                style,
                TextStyle::Header4 | TextStyle::Title | TextStyle::Description
            ) {
                return validation("read-only text style cannot be written");
            }
            Ok(())
        }
        BlockChange::Checked(_) => {
            if matches!(&block.content, BlockContent::Text(text) if text.style == TextStyle::Checkbox)
            {
                Ok(())
            } else {
                validation("checked update requires a checkbox block")
            }
        }
        BlockChange::TextColor(_) => {
            if matches!(block.content, BlockContent::Text(_)) {
                Ok(())
            } else {
                validation("text color update requires a text block")
            }
        }
        BlockChange::CalloutIcon(icon) => {
            if !matches!(&block.content, BlockContent::Text(text) if text.style == TextStyle::Callout)
            {
                return validation("icon update requires a callout block");
            }
            if let Some(icon) = icon {
                validate_icon(icon)?;
            }
            Ok(())
        }
        BlockChange::Embed(embed) => {
            if !matches!(block.content, BlockContent::Embed(_)) {
                return validation("embed update requires an embed block");
            }
            if embed.text.len() > MAX_EMBED_TEXT_BYTES {
                return validation(format!("embed text exceeds {MAX_EMBED_TEXT_BYTES} bytes"));
            }
            if embed.processor == EmbedProcessor::Youtube
                && canonical_youtube_url(&embed.text)? != embed.text
            {
                return validation("YouTube embed updates require the canonical watch URL");
            }
            Ok(())
        }
        BlockChange::DividerStyle(_) => {
            if matches!(block.content, BlockContent::Divider(_)) {
                Ok(())
            } else {
                validation("divider style update requires a divider block")
            }
        }
        BlockChange::LinkAppearance { relations, .. } => {
            if !matches!(block.content, BlockContent::Link(_)) {
                return validation("link appearance update requires a link-card block");
            }
            validate_link_relations(relations)
        }
        BlockChange::HorizontalAlign(_)
        | BlockChange::VerticalAlign(_)
        | BlockChange::Background(_) => Ok(()),
    }
}

fn change_matches(before: &BodyBlock, change: &BlockChange, block: &BodyBlock) -> bool {
    let mut expected = before.clone();
    match change {
        BlockChange::Text { text, marks } => {
            let BlockContent::Text(content) = &mut expected.content else {
                return false;
            };
            content.text.clone_from(text);
            content.marks.clone_from(marks);
        }
        BlockChange::TextStyle(style) => {
            let BlockContent::Text(content) = &mut expected.content else {
                return false;
            };
            content.style = *style;
        }
        BlockChange::Checked(value) => {
            let BlockContent::Text(content) = &mut expected.content else {
                return false;
            };
            content.checked = *value;
        }
        BlockChange::TextColor(color) => {
            let BlockContent::Text(content) = &mut expected.content else {
                return false;
            };
            content.color.clone_from(color);
        }
        BlockChange::CalloutIcon(icon) => {
            let BlockContent::Text(content) = &mut expected.content else {
                return false;
            };
            content.icon.clone_from(icon);
        }
        BlockChange::Embed(embed) => expected.content = BlockContent::Embed(embed.clone()),
        BlockChange::DividerStyle(style) => {
            expected.content = BlockContent::Divider(*style);
        }
        BlockChange::LinkAppearance {
            card_style,
            icon_size,
            description,
            relations,
        } => {
            let BlockContent::Link(link) = &mut expected.content else {
                return false;
            };
            link.card_style = *card_style;
            link.icon_size = *icon_size;
            link.description = *description;
            link.relations.clone_from(relations);
        }
        BlockChange::HorizontalAlign(align) => expected.align = *align,
        BlockChange::VerticalAlign(align) => expected.vertical_align = *align,
        BlockChange::Background(color) => expected.background_color.clone_from(color),
    }
    &expected == block
}

fn same_block_state(before: &BodyBlock, actual: &BodyBlock) -> bool {
    before == actual
}

fn new_block_matches(expected: &NewBlock, actual: &BodyBlock) -> bool {
    let content = match (&expected.content, &actual.content) {
        (NewBlockContent::Text(expected), BlockContent::Text(actual)) => expected == actual,
        (NewBlockContent::Divider(expected), BlockContent::Divider(actual)) => expected == actual,
        (NewBlockContent::Bookmark(expected), BlockContent::Bookmark(actual)) => {
            &actual.url == expected
                && actual.target_object_id.is_none()
                && actual.state == crate::body::BookmarkState::Empty
        }
        (
            NewBlockContent::Link {
                target_object_id,
                card_style,
                icon_size,
                description,
                relations,
            },
            BlockContent::Link(actual),
        ) => {
            &actual.target_object_id == target_object_id
                && &actual.card_style == card_style
                && &actual.icon_size == icon_size
                && &actual.description == description
                && &actual.relations == relations
        }
        (NewBlockContent::Relation(expected), BlockContent::Relation(actual)) => {
            &actual.key == expected
        }
        (NewBlockContent::Embed(expected), BlockContent::Embed(actual)) => expected == actual,
        (NewBlockContent::TableOfContents, BlockContent::TableOfContents)
        | (NewBlockContent::Table { .. }, BlockContent::Table) => true,
        _ => false,
    };
    content
        && actual.align == expected.align
        && actual.vertical_align == expected.vertical_align
        && actual.background_color == expected.background_color
}

fn table_shape_matches(snapshot: &BodySnapshot, table_id: &BlockId, expected: &NewBlock) -> bool {
    let NewBlockContent::Table {
        rows,
        columns,
        with_header_row,
    } = &expected.content
    else {
        return true;
    };
    let Some(table) = snapshot.get(table_id) else {
        return false;
    };
    if table.content != BlockContent::Table {
        return false;
    }
    let [columns_region_id, rows_region_id] = table.children.as_slice() else {
        return false;
    };
    let (Some(columns_region), Some(rows_region)) = (
        snapshot.get(columns_region_id),
        snapshot.get(rows_region_id),
    ) else {
        return false;
    };
    if columns_region.content != BlockContent::Layout(LayoutStyle::TableColumns)
        || rows_region.content != BlockContent::Layout(LayoutStyle::TableRows)
        || columns_region.children.len() != *columns as usize
        || rows_region.children.len() != *rows as usize
    {
        return false;
    }
    let (Ok(row_count), Ok(column_count)) = (usize::try_from(*rows), usize::try_from(*columns))
    else {
        return false;
    };
    let Some(expected_subtree_count) = row_count
        .checked_mul(column_count)
        .and_then(|cells| cells.checked_add(row_count))
        .and_then(|value| value.checked_add(column_count))
        .and_then(|value| value.checked_add(3))
    else {
        return false;
    };
    let columns_match = columns_region.children.iter().all(|id| {
        snapshot.get(id).is_some_and(|block| {
            block.content == BlockContent::TableColumn && block.children.is_empty()
        })
    });
    let rows_match = rows_region.children.iter().enumerate().all(|(index, id)| {
        snapshot.get(id).is_some_and(|block| {
            block.content
                == (BlockContent::TableRow {
                    is_header: *with_header_row && index == 0,
                })
                && block.children.len() == *columns as usize
                && block.children.iter().all(|cell_id| {
                    snapshot
                        .get(cell_id)
                        .is_some_and(canonical_empty_table_cell)
                })
        })
    });
    let descendants = snapshot
        .iter()
        .filter(|block| block.id == *table_id || is_descendant(snapshot, table_id, &block.id))
        .collect::<Vec<_>>();
    let no_misplaced_structure = descendants.iter().all(|block| match block.content {
        BlockContent::TableColumn => columns_region.children.contains(&block.id),
        BlockContent::TableRow { .. } => rows_region.children.contains(&block.id),
        BlockContent::Layout(LayoutStyle::TableColumns) => block.id == *columns_region_id,
        BlockContent::Layout(LayoutStyle::TableRows) => block.id == *rows_region_id,
        BlockContent::Table => block.id == *table_id,
        BlockContent::Text(_) => rows_region.children.iter().any(|row_id| {
            snapshot
                .get(row_id)
                .is_some_and(|row| row.children.contains(&block.id))
        }),
        _ => false,
    });
    descendants.len() == expected_subtree_count
        && columns_match
        && rows_match
        && no_misplaced_structure
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
        && block.background_color.is_none()
        && block.restrictions == BlockRestrictions::default()
}

fn block_to_proto(block: NewBlock) -> model::Block {
    use model::block::{ContentValue, content};
    let content_value = match block.content {
        NewBlockContent::Text(text) => ContentValue::Text(content::Text {
            text: text.text,
            style: text_style_proto(text.style),
            marks: Some(marks_to_proto(&text.marks)),
            checked: text.checked,
            color: text
                .color
                .map_or_else(String::new, |color| color.to_string()),
            icon_emoji: match &text.icon {
                Some(CalloutIcon::Emoji(value)) => value.clone(),
                _ => String::new(),
            },
            icon_image: match &text.icon {
                Some(CalloutIcon::Image(value)) => value.clone(),
                _ => String::new(),
            },
        }),
        NewBlockContent::Divider(style) => ContentValue::Div(content::Div {
            style: divider_style_proto(style),
        }),
        NewBlockContent::Bookmark(url) => ContentValue::Bookmark(content::Bookmark {
            url,
            state: content::bookmark::State::Empty as i32,
            ..Default::default()
        }),
        NewBlockContent::Link {
            target_object_id,
            card_style,
            icon_size,
            description,
            relations,
        } => ContentValue::Link(content::Link {
            target_block_id: target_object_id,
            card_style: link_card_style_proto(card_style),
            icon_size: link_icon_size_proto(icon_size),
            description: link_description_proto(description),
            relations,
            ..Default::default()
        }),
        NewBlockContent::Relation(key) => ContentValue::Relation(content::Relation { key }),
        NewBlockContent::Embed(embed) => ContentValue::Latex(content::Latex {
            text: embed.text,
            processor: embed_processor_proto(embed.processor),
        }),
        NewBlockContent::TableOfContents => {
            ContentValue::TableOfContents(content::TableOfContents {})
        }
        NewBlockContent::Table { .. } => ContentValue::Table(content::Table {}),
    };
    model::Block {
        id: String::new(),
        fields: None,
        restrictions: None,
        children_ids: Vec::new(),
        background_color: block
            .background_color
            .map_or_else(String::new, |color| color.to_string()),
        align: horizontal_align_proto(block.align),
        vertical_align: vertical_align_proto(block.vertical_align),
        content_value: Some(content_value),
    }
}

fn marks_to_proto(marks: &[TextMark]) -> model::block::content::text::Marks {
    model::block::content::text::Marks {
        marks: marks
            .iter()
            .map(|mark| {
                let (kind, param) = match &mark.kind {
                    MarkKind::Bold => {
                        (model::block::content::text::mark::Type::Bold, String::new())
                    }
                    MarkKind::Italic => (
                        model::block::content::text::mark::Type::Italic,
                        String::new(),
                    ),
                    MarkKind::Strikethrough => (
                        model::block::content::text::mark::Type::Strikethrough,
                        String::new(),
                    ),
                    MarkKind::Underline => (
                        model::block::content::text::mark::Type::Underscored,
                        String::new(),
                    ),
                    MarkKind::Code => (
                        model::block::content::text::mark::Type::Keyboard,
                        String::new(),
                    ),
                    MarkKind::Link { url } => {
                        (model::block::content::text::mark::Type::Link, url.clone())
                    }
                    MarkKind::TextColor { color } => (
                        model::block::content::text::mark::Type::TextColor,
                        color.to_string(),
                    ),
                    MarkKind::BackgroundColor { color } => (
                        model::block::content::text::mark::Type::BackgroundColor,
                        color.to_string(),
                    ),
                    MarkKind::Mention { object_id } => (
                        model::block::content::text::mark::Type::Mention,
                        object_id.clone(),
                    ),
                    MarkKind::Emoji { emoji } => (
                        model::block::content::text::mark::Type::Emoji,
                        emoji.clone(),
                    ),
                    MarkKind::Object { object_id } => (
                        model::block::content::text::mark::Type::Object,
                        object_id.clone(),
                    ),
                };
                model::block::content::text::Mark {
                    range: Some(model::Range {
                        from: mark.range.start as i32,
                        to: mark.range.end as i32,
                    }),
                    r#type: kind as i32,
                    param,
                }
            })
            .collect(),
    }
}

fn icon_parts(icon: Option<&CalloutIcon>) -> (String, String) {
    match icon {
        Some(CalloutIcon::Image(value)) => (value.clone(), String::new()),
        Some(CalloutIcon::Emoji(value)) => (String::new(), value.clone()),
        None => (String::new(), String::new()),
    }
}

fn text_style_proto(value: TextStyle) -> i32 {
    use model::block::content::text::Style;
    (match value {
        TextStyle::Paragraph => Style::Paragraph,
        TextStyle::Header1 => Style::Header1,
        TextStyle::Header2 => Style::Header2,
        TextStyle::Header3 => Style::Header3,
        TextStyle::Header4 => Style::Header4,
        TextStyle::Quote => Style::Quote,
        TextStyle::Code => Style::Code,
        TextStyle::Title => Style::Title,
        TextStyle::Description => Style::Description,
        TextStyle::Checkbox => Style::Checkbox,
        TextStyle::Bulleted => Style::Marked,
        TextStyle::Numbered => Style::Numbered,
        TextStyle::Toggle => Style::Toggle,
        TextStyle::Callout => Style::Callout,
        TextStyle::ToggleHeader1 => Style::ToggleHeader1,
        TextStyle::ToggleHeader2 => Style::ToggleHeader2,
        TextStyle::ToggleHeader3 => Style::ToggleHeader3,
    }) as i32
}
fn divider_style_proto(value: DividerStyle) -> i32 {
    use model::block::content::div::Style;
    (match value {
        DividerStyle::Line => Style::Line,
        DividerStyle::Dots => Style::Dots,
    }) as i32
}
fn embed_processor_proto(value: EmbedProcessor) -> i32 {
    use model::block::content::latex::Processor;
    (match value {
        EmbedProcessor::Latex => Processor::Latex,
        EmbedProcessor::Mermaid => Processor::Mermaid,
        EmbedProcessor::Youtube => Processor::Youtube,
    }) as i32
}
fn horizontal_align_proto(value: HorizontalAlign) -> i32 {
    (match value {
        HorizontalAlign::Left => model::block::Align::Left,
        HorizontalAlign::Center => model::block::Align::Center,
        HorizontalAlign::Right => model::block::Align::Right,
        HorizontalAlign::Justify => model::block::Align::Justify,
    }) as i32
}
fn vertical_align_proto(value: VerticalAlign) -> i32 {
    (match value {
        VerticalAlign::Top => model::block::VerticalAlign::Top,
        VerticalAlign::Middle => model::block::VerticalAlign::Middle,
        VerticalAlign::Bottom => model::block::VerticalAlign::Bottom,
    }) as i32
}
fn link_card_style_proto(value: LinkCardStyle) -> i32 {
    use model::block::content::link::CardStyle;
    (match value {
        LinkCardStyle::Text => CardStyle::Text,
        LinkCardStyle::Card => CardStyle::Card,
        LinkCardStyle::Inline => CardStyle::Inline,
    }) as i32
}
fn link_icon_size_proto(value: LinkIconSize) -> i32 {
    use model::block::content::link::IconSize;
    (match value {
        LinkIconSize::None => IconSize::SizeNone,
        LinkIconSize::Small => IconSize::SizeSmall,
        LinkIconSize::Medium => IconSize::SizeMedium,
    }) as i32
}
fn link_description_proto(value: LinkDescriptionMode) -> i32 {
    use model::block::content::link::Description;
    (match value {
        LinkDescriptionMode::None => Description::None,
        LinkDescriptionMode::Added => Description::Added,
        LinkDescriptionMode::Content => Description::Content,
    }) as i32
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::body::{BodyLimits, MAX_TEXT_BYTES, TextRange, snapshot_from_view};

    fn block(
        id: &str,
        children: &[&str],
        content_value: model::block::ContentValue,
    ) -> model::Block {
        model::Block {
            id: id.to_owned(),
            children_ids: children.iter().map(|child| (*child).to_owned()).collect(),
            content_value: Some(content_value),
            ..Default::default()
        }
    }

    fn text_block(id: &str, text: &str) -> model::Block {
        block(
            id,
            &[],
            model::block::ContentValue::Text(model::block::content::Text {
                text: text.to_owned(),
                ..Default::default()
            }),
        )
    }

    fn snapshot() -> Result<BodySnapshot> {
        let view = model::ObjectView {
            root_id: "root".to_owned(),
            blocks: vec![
                block(
                    "root",
                    &["a", "b", "opaque"],
                    model::block::ContentValue::Smartblock(model::block::content::Smartblock {}),
                ),
                text_block("a", "alpha"),
                text_block("b", "beta"),
                block(
                    "opaque",
                    &[],
                    model::block::ContentValue::Widget(model::block::content::Widget::default()),
                ),
            ],
            ..Default::default()
        };
        snapshot_from_view("space", "object", &view, &BodyLimits::default())
    }

    fn nested_snapshot(target_parent: &str, include_affected: bool) -> Result<BodySnapshot> {
        let p1_children: &[&str] = if target_parent == "p1" {
            if include_affected {
                &["affected", "target"]
            } else {
                &["target"]
            }
        } else {
            &[]
        };
        let p2_children: &[&str] = if target_parent == "p2" {
            if include_affected {
                &["affected", "target"]
            } else {
                &["target"]
            }
        } else {
            &[]
        };
        let mut blocks = vec![
            block(
                "root",
                &["p1", "p2"],
                model::block::ContentValue::Smartblock(model::block::content::Smartblock {}),
            ),
            block(
                "p1",
                p1_children,
                model::block::ContentValue::Text(model::block::content::Text::default()),
            ),
            block(
                "p2",
                p2_children,
                model::block::ContentValue::Text(model::block::content::Text::default()),
            ),
            text_block("target", "target"),
        ];
        if include_affected {
            blocks.push(text_block("affected", "affected"));
        }
        snapshot_from_view(
            "space",
            "object",
            &model::ObjectView {
                root_id: "root".to_owned(),
                blocks,
                ..Default::default()
            },
            &BodyLimits::default(),
        )
    }

    fn target_snapshot(
        content_value: model::block::ContentValue,
        restrictions: model::block::Restrictions,
    ) -> Result<(BodySnapshot, BlockId)> {
        let mut target = block("target", &[], content_value);
        target.restrictions = Some(restrictions);
        let snapshot = snapshot_from_view(
            "space",
            "object",
            &model::ObjectView {
                root_id: "root".to_owned(),
                blocks: vec![
                    block(
                        "root",
                        &["target"],
                        model::block::ContentValue::Smartblock(
                            model::block::content::Smartblock {},
                        ),
                    ),
                    target,
                ],
                ..Default::default()
            },
            &BodyLimits::default(),
        )?;
        let id = BlockId::try_from("target".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        Ok((snapshot, id))
    }

    fn table_snapshot(
        table_children: &[&str],
        column_children: &[&str],
        row_children: &[&str],
        columns: &[&str],
        rows: &[(&str, bool)],
        row_cell_children: &[(&str, &[&str])],
        extra: Vec<model::Block>,
    ) -> Result<BodySnapshot> {
        let mut blocks = vec![
            block(
                "root",
                &["table"],
                model::block::ContentValue::Smartblock(model::block::content::Smartblock {}),
            ),
            block(
                "table",
                table_children,
                model::block::ContentValue::Table(model::block::content::Table {}),
            ),
            block(
                "columns",
                column_children,
                model::block::ContentValue::Layout(model::block::content::Layout {
                    style: model::block::content::layout::Style::TableColumns as i32,
                }),
            ),
            block(
                "rows",
                row_children,
                model::block::ContentValue::Layout(model::block::content::Layout {
                    style: model::block::content::layout::Style::TableRows as i32,
                }),
            ),
        ];
        blocks.extend(columns.iter().map(|id| {
            block(
                id,
                &[],
                model::block::ContentValue::TableColumn(model::block::content::TableColumn {}),
            )
        }));
        blocks.extend(rows.iter().map(|(id, is_header)| {
            let children = row_cell_children
                .iter()
                .find_map(|(row_id, children)| (*row_id == *id).then_some(*children))
                .unwrap_or_default();
            block(
                id,
                children,
                model::block::ContentValue::TableRow(model::block::content::TableRow {
                    is_header: *is_header,
                }),
            )
        }));
        blocks.extend(extra);
        snapshot_from_view(
            "space",
            "object",
            &model::ObjectView {
                root_id: "root".to_owned(),
                blocks,
                ..Default::default()
            },
            &BodyLimits::default(),
        )
    }

    fn canonical_table_cells() -> Vec<model::Block> {
        ["r1c1", "r1c2", "r2c1", "r2c2"]
            .into_iter()
            .map(|id| text_block(id, ""))
            .collect()
    }

    fn parent_anchor_snapshot(
        parent_content: model::block::ContentValue,
    ) -> Result<(BodySnapshot, BlockId)> {
        let snapshot = snapshot_from_view(
            "space",
            "object",
            &model::ObjectView {
                root_id: "root".to_owned(),
                blocks: vec![
                    block(
                        "root",
                        &["parent"],
                        model::block::ContentValue::Smartblock(
                            model::block::content::Smartblock {},
                        ),
                    ),
                    block("parent", &["target"], parent_content),
                    text_block("target", "target"),
                ],
                ..Default::default()
            },
            &BodyLimits::default(),
        )?;
        let id = BlockId::try_from("target".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        Ok((snapshot, id))
    }

    fn first_child_anchor_snapshot(
        first_content: model::block::ContentValue,
    ) -> Result<(BodySnapshot, BlockId)> {
        let snapshot = snapshot_from_view(
            "space",
            "object",
            &model::ObjectView {
                root_id: "root".to_owned(),
                blocks: vec![
                    block(
                        "root",
                        &["target"],
                        model::block::ContentValue::Smartblock(
                            model::block::content::Smartblock {},
                        ),
                    ),
                    block(
                        "target",
                        &["first"],
                        model::block::ContentValue::Text(model::block::content::Text::default()),
                    ),
                    block("first", &[], first_content),
                ],
                ..Default::default()
            },
            &BodyLimits::default(),
        )?;
        let id = BlockId::try_from("target".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        Ok((snapshot, id))
    }

    #[test]
    fn typed_text_constructors_cover_writable_styles() -> Result<()> {
        use model::block::content::text::Style;
        let constructors = [
            (NewBlock::paragraph("p")?, Style::Paragraph),
            (NewBlock::heading(1, "h1")?, Style::Header1),
            (NewBlock::heading(2, "h2")?, Style::Header2),
            (NewBlock::heading(3, "h3")?, Style::Header3),
            (NewBlock::bulleted("b")?, Style::Marked),
            (NewBlock::numbered("n")?, Style::Numbered),
            (NewBlock::checkbox("c", true)?, Style::Checkbox),
            (NewBlock::toggle("t")?, Style::Toggle),
            (
                NewBlock::callout("c", Some(CalloutIcon::Emoji("!".to_owned())))?,
                Style::Callout,
            ),
            (NewBlock::quote("q")?, Style::Quote),
            (NewBlock::code("code")?, Style::Code),
        ];
        for (constructor, expected) in constructors {
            let Some(model::block::ContentValue::Text(text)) =
                block_to_proto(constructor).content_value
            else {
                return validation("text constructor emitted a non-text proto");
            };
            assert_eq!(text.style, expected as i32);
        }
        assert!(NewBlock::heading(4, "bad").is_err());
        Ok(())
    }

    #[test]
    fn typed_non_text_constructors_cover_v1_families() -> Result<()> {
        let constructors = vec![
            block_to_proto(NewBlock::divider(DividerStyle::Dots)),
            block_to_proto(NewBlock::bookmark("https://example.com/card")?),
            block_to_proto(
                NewBlock::link_card(
                    "target-id",
                    LinkCardStyle::Card,
                    LinkIconSize::Medium,
                    LinkDescriptionMode::Content,
                )?
                .link_relations(vec!["name".to_owned()])?,
            ),
            block_to_proto(NewBlock::relation("relation-key")?),
            block_to_proto(NewBlock::table(2, 3, true)?),
            block_to_proto(NewBlock::embed_latex("x^2")?),
            block_to_proto(NewBlock::embed_mermaid("graph TD; A-->B")?),
            block_to_proto(NewBlock::embed_youtube("https://youtu.be/dQw4w9WgXcQ")?),
            block_to_proto(NewBlock::table_of_contents()),
        ];
        use model::block::ContentValue;
        assert!(matches!(
            constructors[0].content_value,
            Some(ContentValue::Div(_))
        ));
        assert!(matches!(
            constructors[1].content_value,
            Some(ContentValue::Bookmark(_))
        ));
        assert!(matches!(
            &constructors[2].content_value,
            Some(ContentValue::Link(link)) if link.target_block_id == "target-id"
                && link.relations == ["name"]
        ));
        assert!(matches!(
            constructors[3].content_value,
            Some(ContentValue::Relation(_))
        ));
        assert!(matches!(
            constructors[4].content_value,
            Some(ContentValue::Table(_))
        ));
        for proto in &constructors[5..8] {
            assert!(matches!(proto.content_value, Some(ContentValue::Latex(_))));
        }
        assert!(matches!(
            constructors[8].content_value,
            Some(ContentValue::TableOfContents(_))
        ));
        Ok(())
    }

    #[test]
    fn table_receipt_requires_canonical_direct_region_topology() -> Result<()> {
        const ROW_CELLS: &[(&str, &[&str])] =
            &[("r1", &["r1c1", "r1c2"]), ("r2", &["r2c1", "r2c2"])];
        let expected = NewBlock::table(2, 2, true)?;
        let table_id = BlockId::try_from("table".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        let canonical = table_snapshot(
            &["columns", "rows"],
            &["c1", "c2"],
            &["r1", "r2"],
            &["c1", "c2"],
            &[("r1", true), ("r2", false)],
            ROW_CELLS,
            canonical_table_cells(),
        )?;
        assert!(table_shape_matches(&canonical, &table_id, &expected));

        let missing_cell = table_snapshot(
            &["columns", "rows"],
            &["c1", "c2"],
            &["r1", "r2"],
            &["c1", "c2"],
            &[("r1", true), ("r2", false)],
            &[("r1", &["r1c1", "r1c2"]), ("r2", &["r2c1"])],
            canonical_table_cells()
                .into_iter()
                .filter(|block| block.id != "r2c2")
                .collect(),
        )?;
        assert!(!table_shape_matches(&missing_cell, &table_id, &expected));

        let mut extra_cells = canonical_table_cells();
        extra_cells.push(text_block("r2c3", ""));
        let extra_cell = table_snapshot(
            &["columns", "rows"],
            &["c1", "c2"],
            &["r1", "r2"],
            &["c1", "c2"],
            &[("r1", true), ("r2", false)],
            &[("r1", &["r1c1", "r1c2"]), ("r2", &["r2c1", "r2c2", "r2c3"])],
            extra_cells,
        )?;
        assert!(!table_shape_matches(&extra_cell, &table_id, &expected));

        let cell_variant = |replacement: model::Block| -> Result<BodySnapshot> {
            let mut cells = canonical_table_cells();
            cells.retain(|block| block.id != "r1c1");
            cells.push(replacement);
            table_snapshot(
                &["columns", "rows"],
                &["c1", "c2"],
                &["r1", "r2"],
                &["c1", "c2"],
                &[("r1", true), ("r2", false)],
                ROW_CELLS,
                cells,
            )
        };
        let wrong_cell_type = cell_variant(block(
            "r1c1",
            &[],
            model::block::ContentValue::TableColumn(model::block::content::TableColumn {}),
        ))?;
        let nonempty_cell = cell_variant(text_block("r1c1", "not empty"))?;
        let mut presented = text_block("r1c1", "");
        presented.align = model::block::Align::Center as i32;
        let wrong_cell_presentation = cell_variant(presented)?;
        for invalid in [wrong_cell_type, nonempty_cell, wrong_cell_presentation] {
            assert!(!table_shape_matches(&invalid, &table_id, &expected));
        }
        let mut nested_cells = canonical_table_cells();
        nested_cells.retain(|block| block.id != "r1c1");
        nested_cells.push(block(
            "r1c1",
            &["nested"],
            model::block::ContentValue::Text(model::block::content::Text::default()),
        ));
        nested_cells.push(text_block("nested", ""));
        let cell_with_child = table_snapshot(
            &["columns", "rows"],
            &["c1", "c2"],
            &["r1", "r2"],
            &["c1", "c2"],
            &[("r1", true), ("r2", false)],
            ROW_CELLS,
            nested_cells,
        )?;
        assert!(!table_shape_matches(&cell_with_child, &table_id, &expected));

        let reversed_regions = table_snapshot(
            &["rows", "columns"],
            &["c1", "c2"],
            &["r1", "r2"],
            &["c1", "c2"],
            &[("r1", true), ("r2", false)],
            ROW_CELLS,
            canonical_table_cells(),
        )?;
        assert!(!table_shape_matches(
            &reversed_regions,
            &table_id,
            &expected
        ));

        let swapped_members = table_snapshot(
            &["columns", "rows"],
            &["r1", "r2"],
            &["c1", "c2"],
            &["c1", "c2"],
            &[("r1", true), ("r2", false)],
            ROW_CELLS,
            canonical_table_cells(),
        )?;
        assert!(!table_shape_matches(&swapped_members, &table_id, &expected));

        let misplaced_header = table_snapshot(
            &["columns", "rows"],
            &["c1", "c2"],
            &["r1", "r2"],
            &["c1", "c2"],
            &[("r1", false), ("r2", true)],
            ROW_CELLS,
            canonical_table_cells(),
        )?;
        assert!(!table_shape_matches(
            &misplaced_header,
            &table_id,
            &expected
        ));

        let nested_row = table_snapshot(
            &["columns", "rows"],
            &["c1", "c2"],
            &["wrapper", "r2"],
            &["c1", "c2"],
            &[("r1", true), ("r2", false)],
            ROW_CELLS,
            {
                let mut extra = canonical_table_cells();
                extra.push(block(
                    "wrapper",
                    &["r1"],
                    model::block::ContentValue::Text(model::block::content::Text {
                        text: "wrapper".to_owned(),
                        ..Default::default()
                    }),
                ));
                extra
            },
        )?;
        assert!(!table_shape_matches(&nested_row, &table_id, &expected));

        let extra_nested_row = table_snapshot(
            &["columns", "rows"],
            &["c1", "c2"],
            &["r1", "r2"],
            &["c1", "c2"],
            &[("r1", true), ("r2", false), ("r3", false)],
            &[("r2", &["wrapper"])],
            vec![block(
                "wrapper",
                &["r3"],
                model::block::ContentValue::Text(model::block::content::Text::default()),
            )],
        )?;
        assert!(!table_shape_matches(
            &extra_nested_row,
            &table_id,
            &expected
        ));

        let wrong_no_header = NewBlock::table(2, 2, false)?;
        assert!(!table_shape_matches(
            &canonical,
            &table_id,
            &wrong_no_header
        ));
        let canonical_no_header = table_snapshot(
            &["columns", "rows"],
            &["c1", "c2"],
            &["r1", "r2"],
            &["c1", "c2"],
            &[("r1", false), ("r2", false)],
            ROW_CELLS,
            canonical_table_cells(),
        )?;
        assert!(table_shape_matches(
            &canonical_no_header,
            &table_id,
            &wrong_no_header
        ));
        Ok(())
    }

    #[test]
    fn every_mark_kind_maps_to_exact_proto_type_and_parameter() -> Result<()> {
        use model::block::content::text::mark::Type;
        let purple = ColorToken::new("purple")?;
        let cases = [
            (MarkKind::Bold, Type::Bold, ""),
            (MarkKind::Italic, Type::Italic, ""),
            (MarkKind::Strikethrough, Type::Strikethrough, ""),
            (MarkKind::Underline, Type::Underscored, ""),
            (MarkKind::Code, Type::Keyboard, ""),
            (
                MarkKind::Link {
                    url: "https://example.com".to_owned(),
                },
                Type::Link,
                "https://example.com",
            ),
            (
                MarkKind::TextColor {
                    color: purple.clone(),
                },
                Type::TextColor,
                "purple",
            ),
            (
                MarkKind::BackgroundColor { color: purple },
                Type::BackgroundColor,
                "purple",
            ),
            (
                MarkKind::Mention {
                    object_id: "mention".to_owned(),
                },
                Type::Mention,
                "mention",
            ),
            (
                MarkKind::Emoji {
                    emoji: "🙂".to_owned(),
                },
                Type::Emoji,
                "🙂",
            ),
            (
                MarkKind::Object {
                    object_id: "object".to_owned(),
                },
                Type::Object,
                "object",
            ),
        ];
        for (kind, expected_type, expected_param) in cases {
            let proto = marks_to_proto(&[TextMark {
                range: TextRange { start: 1, end: 2 },
                kind,
            }]);
            let Some(mark) = proto.marks.first() else {
                return validation("mark conversion returned no mark");
            };
            assert_eq!(mark.r#type, expected_type as i32);
            assert_eq!(mark.param, expected_param);
            assert_eq!(
                mark.range.as_ref().map(|range| (range.from, range.to)),
                Some((1, 2))
            );
        }
        Ok(())
    }

    #[test]
    fn marks_and_constructor_limits_fail_closed() -> Result<()> {
        let mark = TextMark {
            range: TextRange { start: 0, end: 2 },
            kind: MarkKind::Bold,
        };
        assert!(NewBlock::paragraph("a")?.marks(vec![mark]).is_err());
        assert!(NewBlock::paragraph("x".repeat(MAX_TEXT_BYTES + 1)).is_err());
        assert!(NewBlock::table(0, 1, false).is_err());
        assert!(NewBlock::table(1, MAX_TABLE_COLUMNS as u32 + 1, false).is_err());
        let nondefault_table = NewBlock::table(1, 1, false)?.align(HorizontalAlign::Center);
        assert!(validate_new_block_for_create(&nondefault_table).is_err());

        let text = "a\u{10348}b";
        for range in [
            TextRange { start: 2, end: 3 },
            TextRange { start: 1, end: 2 },
            TextRange { start: 3, end: 1 },
            TextRange {
                start: 0,
                end: u32::MAX,
            },
            TextRange {
                start: u32::MAX,
                end: u32::MAX,
            },
        ] {
            assert!(
                NewBlock::paragraph(text)?
                    .marks(vec![TextMark {
                        range,
                        kind: MarkKind::Bold,
                    }])
                    .is_err()
            );
        }
        for range in [
            TextRange { start: 0, end: 0 },
            TextRange { start: 4, end: 4 },
            TextRange { start: 1, end: 3 },
        ] {
            assert!(
                NewBlock::paragraph(text)?
                    .marks(vec![TextMark {
                        range,
                        kind: MarkKind::Bold,
                    }])
                    .is_ok()
            );
        }
        Ok(())
    }

    #[test]
    fn mutation_emoji_values_enforce_exact_utf8_byte_bounds() -> Result<()> {
        for emoji in ["x".to_owned(), "🙂".repeat(16)] {
            assert!(NewBlock::callout("text", Some(CalloutIcon::Emoji(emoji.clone()))).is_ok());
            assert!(
                NewBlock::paragraph("x")?
                    .marks(vec![TextMark {
                        range: TextRange { start: 0, end: 1 },
                        kind: MarkKind::Emoji { emoji },
                    }])
                    .is_ok()
            );
        }
        for emoji in [String::new(), "\n".to_owned(), "a".repeat(65)] {
            assert!(NewBlock::callout("text", Some(CalloutIcon::Emoji(emoji.clone()))).is_err());
            assert!(
                NewBlock::paragraph("x")?
                    .marks(vec![TextMark {
                        range: TextRange { start: 0, end: 1 },
                        kind: MarkKind::Emoji { emoji },
                    }])
                    .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn bookmark_policy_never_accepts_local_or_credential_forms() {
        assert!(NewBlock::bookmark("https://example.com/path").is_ok());
        assert!(NewBlock::bookmark("file:///etc/passwd").is_err());
        assert!(NewBlock::bookmark("http://user:pass@example.com/").is_err());
        assert!(NewBlock::bookmark("https://example.com/#secret").is_err());
    }

    #[test]
    fn youtube_urls_are_canonical_and_allowlisted() -> Result<()> {
        let short = NewBlock::embed_youtube("https://youtu.be/dQw4w9WgXcQ")?;
        let NewBlockContent::Embed(embed) = short.content else {
            return validation("expected embed");
        };
        assert_eq!(embed.text, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert!(NewBlock::embed_youtube("http://youtube.com/watch?v=dQw4w9WgXcQ").is_err());
        assert!(NewBlock::embed_youtube("https://youtube.example/watch?v=dQw4w9WgXcQ").is_err());
        assert!(
            NewBlock::embed_youtube("https://youtube.com/watch?v=dQw4w9WgXcQ&feature=test")
                .is_err()
        );
        assert!(NewBlock::embed_youtube("https://youtu.be/dQw4w9WgXcQ/extra").is_err());
        Ok(())
    }

    #[test]
    fn proto_conversion_preserves_rich_text_state() -> Result<()> {
        let color = ColorToken::new("purple")?;
        let block = NewBlock::checkbox("hello", true)?
            .marks(vec![TextMark {
                range: TextRange { start: 0, end: 5 },
                kind: MarkKind::Bold,
            }])?
            .text_color(color.clone())?
            .align(HorizontalAlign::Center)
            .background(color);
        let proto = block_to_proto(block);
        assert_eq!(proto.align, model::block::Align::Center as i32);
        assert_eq!(proto.background_color, "purple");
        let Some(model::block::ContentValue::Text(text)) = proto.content_value else {
            return validation("expected text proto");
        };
        assert!(text.checked);
        assert_eq!(text.marks.map_or(0, |marks| marks.marks.len()), 1);
        Ok(())
    }

    #[test]
    fn batch_is_statically_bounded() {
        assert_eq!(MAX_BODY_BLOCKS, 8_192);
    }

    #[test]
    fn verification_policy_is_finite() {
        let verify = VerifyConfig {
            timeout: std::time::Duration::from_millis(1),
            initial_delay: std::time::Duration::ZERO,
            max_delay: std::time::Duration::ZERO,
            max_attempts: usize::MAX,
        };
        assert_eq!(
            verify.effective_max_attempts(),
            crate::verify::MAX_VERIFY_ATTEMPTS
        );
    }

    fn fault_verify_config() -> VerifyConfig {
        VerifyConfig {
            timeout: std::time::Duration::from_millis(20),
            initial_delay: std::time::Duration::ZERO,
            max_delay: std::time::Duration::ZERO,
            max_attempts: 2,
        }
    }

    fn fault_rpc_config() -> BodyRpcConfig {
        BodyRpcConfig::for_timeout(std::time::Duration::from_secs(1))
            .rpc_timeout(std::time::Duration::from_millis(20))
    }

    #[tokio::test]
    async fn mutation_rpc_dispatches_exactly_once() -> Result<()> {
        let dispatches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&dispatches);
        let rpc = fault_rpc_config();
        let value =
            poll_tonic_write_once(&fault_verify_config(), &rpc, "object", None, async move {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(tonic::Response::new(7_u8))
            })
            .await?;
        assert_eq!(value, 7);
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
        assert_eq!(rpc.metrics().snapshot().write_polls, 1);
        Ok(())
    }

    #[tokio::test]
    async fn mutation_rpc_timeout_after_dispatch_is_indeterminate() {
        let dispatches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&dispatches);
        let error = poll_tonic_write_once::<(), _>(
            &fault_verify_config(),
            &fault_rpc_config(),
            "object",
            None,
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                std::future::pending().await
            },
        )
        .await
        .expect_err("pending mutation must time out");
        assert!(matches!(
            error,
            AnytypeError::BodyMutationIndeterminate {
                attempts: 0,
                observed: None,
                ..
            }
        ));
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exhausted_absolute_deadline_proves_no_write_poll() {
        let dispatches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&dispatches);
        let rpc = BodyRpcConfig::new(tokio::time::Instant::now());
        let error = poll_tonic_write_once::<(), _>(
            &fault_verify_config(),
            &rpc,
            "object",
            None,
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(tonic::Response::new(()))
            },
        )
        .await
        .expect_err("expired deadline must fail before write polling");
        assert!(matches!(
            error,
            AnytypeError::BodyRpcLifecycle {
                kind: crate::body_rpc::BodyRpcLifecycleErrorKind::AbsoluteDeadlineExhausted
            }
        ));
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        assert_eq!(rpc.metrics().snapshot().write_polls, 0);
    }

    #[tokio::test]
    async fn zero_local_budget_proves_no_write_poll() {
        let dispatches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&dispatches);
        let rpc = fault_rpc_config();
        let verify = VerifyConfig {
            timeout: std::time::Duration::ZERO,
            ..fault_verify_config()
        };
        let error = poll_tonic_write_once::<(), _>(&verify, &rpc, "object", None, async move {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(tonic::Response::new(()))
        })
        .await
        .expect_err("zero local budget must fail before write polling");
        assert!(matches!(error, AnytypeError::Validation { .. }));
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        assert_eq!(rpc.metrics().snapshot().write_polls, 0);
    }

    #[tokio::test]
    async fn mutation_decoder_overrun_is_indeterminate_and_counted() {
        let rpc = fault_rpc_config();
        let limit = rpc.non_show_response_limit();
        let status = tonic::Status::out_of_range(format!(
            "Error, decoded message length too large: found {} bytes, the limit is: {limit} bytes",
            limit + 1
        ));
        let error = poll_tonic_write_once::<(), _>(
            &fault_verify_config(),
            &rpc,
            "object",
            None,
            async move { Err(status) },
        )
        .await
        .expect_err("oversized mutation response must be indeterminate");
        assert!(matches!(
            error,
            AnytypeError::BodyMutationIndeterminate { .. }
        ));
        let metrics = rpc.metrics().snapshot();
        assert_eq!(metrics.write_polls, 1);
        assert_eq!(metrics.non_show_limit_rejections, 1);
        assert_eq!(metrics.mutation_limit_rejections, 1);
    }

    #[tokio::test]
    async fn tonic_connection_cancellation_and_shutdown_are_indeterminate_once() {
        for status in [
            tonic::Status::unavailable("SECRET_CONNECTION"),
            tonic::Status::cancelled("SECRET_CANCELLATION"),
            tonic::Status::aborted("SECRET_SHUTDOWN"),
            tonic::Status::resource_exhausted("SECRET_OVERSIZED"),
        ] {
            let dispatches = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&dispatches);
            let error = poll_tonic_write_once::<(), _>(
                &fault_verify_config(),
                &fault_rpc_config(),
                "object",
                None,
                async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    Err(status)
                },
            )
            .await
            .expect_err("ambiguous tonic status must be indeterminate");
            assert!(matches!(
                error,
                AnytypeError::BodyMutationIndeterminate { .. }
            ));
            assert_eq!(dispatches.load(Ordering::SeqCst), 1);
            assert!(!format!("{error:?}").contains("SECRET"));
        }
    }

    #[tokio::test]
    async fn dropping_polled_mutation_future_never_replays() {
        let dispatches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&dispatches);
        let entered = Arc::new(tokio::sync::Notify::new());
        let signal = Arc::clone(&entered);
        let notified = entered.notified();
        let task = tokio::spawn(async move {
            poll_tonic_write_once::<(), _>(
                &fault_verify_config(),
                &fault_rpc_config(),
                "object",
                None,
                async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    signal.notify_one();
                    std::future::pending().await
                },
            )
            .await
        });
        notified.await;
        task.abort();
        let _ = task.await;
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn response_errors_use_fixed_typed_classification() {
        let unknown = block_create::response::Error {
            code: 1,
            description: "SECRET_UPSTREAM".to_owned(),
        };
        let error = mutation_response_ok(
            Some(&unknown),
            "block create",
            "object",
            None,
            std::time::Duration::from_secs(1),
        )
        .expect_err("unknown response is indeterminate");
        assert!(matches!(
            error,
            AnytypeError::BodyMutationIndeterminate { .. }
        ));
        assert!(!format!("{error:?}").contains("SECRET"));

        let rejected = block_create::response::Error {
            code: 2,
            description: "SECRET_BAD_INPUT".to_owned(),
        };
        let error = mutation_response_ok(
            Some(&rejected),
            "block create",
            "object",
            None,
            std::time::Duration::from_secs(1),
        )
        .expect_err("bad input is definitive validation");
        assert!(matches!(error, AnytypeError::Validation { .. }));
        assert!(!format!("{error:?}").contains("SECRET"));
    }

    #[tokio::test]
    async fn verification_exhaustion_preserves_last_complete_snapshot() -> Result<()> {
        let stale = snapshot()?;
        let fetches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&fetches);
        let stale_for_fetch = stale.clone();
        let error = verify_snapshot_with(
            &fault_verify_config(),
            &fault_rpc_config(),
            "object",
            None,
            move || {
                let attempt = counted.fetch_add(1, Ordering::SeqCst);
                let result = if attempt == 0 {
                    Ok(stale_for_fetch.clone())
                } else {
                    Err(AnytypeError::Other {
                        message: "SECRET_TRANSIENT".to_owned(),
                    })
                };
                async move { result }
            },
            |_| false,
        )
        .await
        .expect_err("stale evidence must exhaust verification");
        let AnytypeError::BodyMutationIndeterminate {
            attempts, observed, ..
        } = error
        else {
            return validation("expected indeterminate verification result");
        };
        assert_eq!(attempts, 2);
        assert_eq!(
            observed.as_deref().map(|snapshot| &snapshot.root_id),
            Some(&stale.root_id)
        );
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[test]
    fn stale_and_opaque_targets_fail_before_io() -> Result<()> {
        let snapshot = snapshot()?;
        let missing = BlockId::try_from("missing".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        let opaque = BlockId::try_from("opaque".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        assert!(matches!(
            validate_anchor(&snapshot, &missing, InsertPosition::After),
            Err(AnytypeError::NotFound { .. })
        ));
        assert!(validate_anchor(&snapshot, &opaque, InsertPosition::After).is_err());
        assert!(typed_block(&snapshot, &opaque).is_err());
        Ok(())
    }

    #[test]
    fn system_and_structural_blocks_are_locally_read_only() -> Result<()> {
        use model::block::{ContentValue, content};
        let title = content::Text {
            style: content::text::Style::Title as i32,
            ..Default::default()
        };
        let description = content::Text {
            style: content::text::Style::Description as i32,
            ..Default::default()
        };
        let header4 = content::Text {
            style: content::text::Style::Header4 as i32,
            ..Default::default()
        };
        let cases = vec![
            ContentValue::Text(title),
            ContentValue::Text(description),
            ContentValue::Text(header4),
            ContentValue::File(content::File::default()),
            ContentValue::Layout(content::Layout::default()),
            ContentValue::FeaturedRelations(content::FeaturedRelations::default()),
            ContentValue::Table(content::Table::default()),
            ContentValue::TableRow(content::TableRow::default()),
            ContentValue::TableColumn(content::TableColumn::default()),
            ContentValue::Widget(content::Widget::default()),
        ];
        for content in cases {
            let (snapshot, id) = target_snapshot(content, model::block::Restrictions::default())?;
            assert!(typed_block(&snapshot, &id).is_err());
            assert!(update_target(&snapshot, &id).is_err());
            assert!(delete_target(&snapshot, &id).is_err());
            assert!(move_source(&snapshot, &id).is_err());
        }
        Ok(())
    }

    #[test]
    fn every_mutation_restriction_is_rejected_locally() -> Result<()> {
        use model::block::{ContentValue, Restrictions, content};
        let ordinary = || ContentValue::Text(content::Text::default());
        let (snapshot, id) = target_snapshot(
            ordinary(),
            Restrictions {
                edit: true,
                ..Default::default()
            },
        )?;
        assert!(update_target(&snapshot, &id).is_err());
        let (snapshot, id) = target_snapshot(
            ordinary(),
            Restrictions {
                remove: true,
                ..Default::default()
            },
        )?;
        assert!(delete_target(&snapshot, &id).is_err());
        let (snapshot, id) = target_snapshot(
            ordinary(),
            Restrictions {
                drag: true,
                ..Default::default()
            },
        )?;
        assert!(move_source(&snapshot, &id).is_err());
        let (snapshot, id) = target_snapshot(
            ordinary(),
            Restrictions {
                drop_on: true,
                ..Default::default()
            },
        )?;
        assert!(validate_anchor(&snapshot, &id, InsertPosition::LastChild).is_err());
        Ok(())
    }

    #[test]
    fn every_structural_read_only_kind_is_rejected_in_all_anchor_roles() -> Result<()> {
        use model::block::{ContentValue, content};
        let text = |style| {
            ContentValue::Text(content::Text {
                style: style as i32,
                ..Default::default()
            })
        };
        let file = ContentValue::File(content::File {
            hash: String::new(),
            target_object_id: "file-object".to_owned(),
            r#type: content::file::Type::Image as i32,
            mime: "image/png".to_owned(),
            size: 1,
            added_at: 0,
            state: content::file::State::Done as i32,
            style: content::file::Style::Embed as i32,
            name: "image.png".to_owned(),
        });
        let cases = vec![
            text(content::text::Style::Title),
            text(content::text::Style::Description),
            text(content::text::Style::Header4),
            ContentValue::FeaturedRelations(content::FeaturedRelations::default()),
            file,
            ContentValue::Layout(content::Layout::default()),
            ContentValue::Table(content::Table::default()),
            ContentValue::TableRow(content::TableRow::default()),
            ContentValue::TableColumn(content::TableColumn::default()),
            ContentValue::Widget(content::Widget::default()),
        ];
        for content in cases {
            let (snapshot, target) =
                target_snapshot(content.clone(), model::block::Restrictions::default())?;
            assert!(validate_anchor(&snapshot, &target, InsertPosition::LastChild).is_err());

            let (snapshot, target) = parent_anchor_snapshot(content.clone())?;
            assert!(validate_anchor(&snapshot, &target, InsertPosition::Before).is_err());

            let (snapshot, target) = first_child_anchor_snapshot(content)?;
            assert!(validate_anchor(&snapshot, &target, InsertPosition::FirstChild).is_err());
        }
        Ok(())
    }

    #[test]
    fn exact_position_evidence_rejects_stale_order() -> Result<()> {
        let snapshot = snapshot()?;
        let first_id = BlockId::try_from("a".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        let second_id = BlockId::try_from("b".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        let before = position_expectation(&snapshot, &second_id, InsertPosition::Before)?;
        let after = position_expectation(&snapshot, &second_id, InsertPosition::After)?;
        let before_first = position_expectation(&snapshot, &first_id, InsertPosition::Before)?;
        assert!(position_matches(&snapshot, &first_id, &before));
        assert!(!position_matches(&snapshot, &first_id, &after));
        assert!(!position_matches(&snapshot, &second_id, &before_first));
        Ok(())
    }

    #[test]
    fn create_receipt_rejects_concurrently_moved_anchor_parent() -> Result<()> {
        let initial = nested_snapshot("p1", false)?;
        let moved = nested_snapshot("p2", true)?;
        let target = BlockId::try_from("target".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        let affected = BlockId::try_from("affected".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        let expectation = position_expectation(&initial, &target, InsertPosition::Before)?;
        assert!(!position_matches(&moved, &affected, &expectation));
        Ok(())
    }

    #[test]
    fn move_receipt_rejects_concurrently_moved_anchor_parent() -> Result<()> {
        let initial = nested_snapshot("p1", false)?;
        let moved = nested_snapshot("p2", true)?;
        let target = BlockId::try_from("target".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        let affected = BlockId::try_from("affected".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        let expectation = position_expectation(&initial, &target, InsertPosition::Before)?;
        assert!(!position_matches(&moved, &affected, &expectation));
        Ok(())
    }

    #[test]
    fn update_evidence_requires_unchanged_rich_state() -> Result<()> {
        let snapshot = snapshot()?;
        let first_id = BlockId::try_from("a".to_owned())
            .map_err(|message| AnytypeError::Validation { message })?;
        let before = snapshot
            .get(&first_id)
            .ok_or_else(|| AnytypeError::Validation {
                message: "missing fixture block".to_owned(),
            })?;
        let mut exact = before.clone();
        if let BlockContent::Text(text) = &mut exact.content {
            text.text = "changed".to_owned();
        }
        let change = BlockChange::Text {
            text: "changed".to_owned(),
            marks: Vec::new(),
        };
        assert!(change_matches(before, &change, &exact));
        exact.align = HorizontalAlign::Center;
        assert!(!change_matches(before, &change, &exact));
        Ok(())
    }
}
