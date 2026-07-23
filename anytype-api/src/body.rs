// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Typed, bounded, fail-closed body-block reads.
//!
//! This module exposes the rich body of an Anytype object (paragraphs,
//! headings, lists, callouts, tables, bookmarks, embeds) as a validated
//! [`BodySnapshot`] tree with exact block identities and exact child order,
//! read through the gRPC `ObjectShow` view.
//!
//! Design contract:
//!
//! - No `anytype_rpc` protobuf type appears in any public signature; all
//!   conversions are crate-private.
//! - Content kinds, text styles, and marks are closed enums covering the v1
//!   rich-content families. Anything else reads as an explicit
//!   [`BlockContent::Unsupported`] marker with a content-free summary; nothing
//!   is coerced or silently dropped.
//! - Malformed graphs, out-of-range structural enum values, and oversized
//!   inputs fail the whole read with a typed
//!   [`BodyGraph`](crate::error::AnytypeError::BodyGraph) error; a partial or
//!   truncated snapshot is never returned.
//!
//! # Example
//!
//! ```rust,no_run
//! # use anytype::prelude::*;
//! # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
//! let snapshot = client.blocks().body("space_id", "object_id").fetch().await?;
//! for block in snapshot.iter() {
//!     println!("{} -> {:?}", block.id, block.content);
//! }
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::fmt;

use anytype_rpc::model;
use prost::Message as _;
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    body_rpc::{BodyRpcConfig, fetch_object_view},
    client::AnytypeClient,
    error::AnytypeError,
};

// ============================================================================
// Hard limit ceilings
// ============================================================================

/// Hard ceiling on the number of blocks accepted in one body read.
pub const MAX_BODY_BLOCKS: usize = 8_192;
/// Hard ceiling on tree depth (the root block sits at depth 1).
pub const MAX_BODY_DEPTH: usize = 64;
/// Hard ceiling on the number of direct children of one block.
pub const MAX_BLOCK_CHILDREN: usize = 2_048;
/// Hard ceiling on the UTF-8 byte length of one text block.
pub const MAX_TEXT_BYTES: usize = 262_144;
/// Hard ceiling on the number of marks carried by one text block.
pub const MAX_MARKS_PER_TEXT: usize = 1_024;
/// Hard ceiling on the number of rows in one table.
pub const MAX_TABLE_ROWS: usize = 512;
/// Hard ceiling on the number of columns in one table.
pub const MAX_TABLE_COLUMNS: usize = 64;
/// Hard ceiling on the byte length of one block ID.
pub const MAX_BLOCK_ID_BYTES: usize = 256;
/// Hard ceiling on the UTF-8 byte length of one embed (LaTeX/Mermaid) source.
pub const MAX_EMBED_TEXT_BYTES: usize = 65_536;
/// Hard ceiling on the byte length of a color token.
pub const MAX_COLOR_TOKEN_BYTES: usize = 32;
/// Hard ceiling on relation keys shown by one link-card block.
pub const MAX_LINK_RELATIONS: usize = 64;

/// Per-request bounds for a body read.
///
/// Every field defaults to its hard ceiling. Callers may lower any bound;
/// values above a ceiling clamp back down to it — limits can only tighten,
/// never widen (following the [`ValidationLimits`](crate::validation::ValidationLimits)
/// precedent). A read that exceeds any effective bound fails whole with
/// [`BodyGraphErrorKind::Oversized`]; it is never truncated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyLimits {
    /// Maximum number of blocks in the returned view.
    pub max_blocks: usize,
    /// Maximum tree depth (root at depth 1).
    pub max_depth: usize,
    /// Maximum direct children per block.
    pub max_children: usize,
    /// Maximum UTF-8 bytes of one text block.
    pub max_text_bytes: usize,
    /// Maximum marks per text block.
    pub max_marks_per_text: usize,
    /// Maximum rows per table.
    pub max_table_rows: usize,
    /// Maximum columns per table.
    pub max_table_columns: usize,
    /// Maximum bytes of one block ID.
    pub max_block_id_bytes: usize,
    /// Maximum UTF-8 bytes of one embed source.
    pub max_embed_text_bytes: usize,
}

impl Default for BodyLimits {
    fn default() -> Self {
        Self {
            max_blocks: MAX_BODY_BLOCKS,
            max_depth: MAX_BODY_DEPTH,
            max_children: MAX_BLOCK_CHILDREN,
            max_text_bytes: MAX_TEXT_BYTES,
            max_marks_per_text: MAX_MARKS_PER_TEXT,
            max_table_rows: MAX_TABLE_ROWS,
            max_table_columns: MAX_TABLE_COLUMNS,
            max_block_id_bytes: MAX_BLOCK_ID_BYTES,
            max_embed_text_bytes: MAX_EMBED_TEXT_BYTES,
        }
    }
}

impl BodyLimits {
    /// Returns these limits with every bound clamped to its hard ceiling.
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            max_blocks: self.max_blocks.min(MAX_BODY_BLOCKS),
            max_depth: self.max_depth.min(MAX_BODY_DEPTH),
            max_children: self.max_children.min(MAX_BLOCK_CHILDREN),
            max_text_bytes: self.max_text_bytes.min(MAX_TEXT_BYTES),
            max_marks_per_text: self.max_marks_per_text.min(MAX_MARKS_PER_TEXT),
            max_table_rows: self.max_table_rows.min(MAX_TABLE_ROWS),
            max_table_columns: self.max_table_columns.min(MAX_TABLE_COLUMNS),
            max_block_id_bytes: self.max_block_id_bytes.min(MAX_BLOCK_ID_BYTES),
            max_embed_text_bytes: self.max_embed_text_bytes.min(MAX_EMBED_TEXT_BYTES),
        }
    }
}

// ============================================================================
// Identifiers, colors, references
// ============================================================================

/// Opaque, validated block identifier.
///
/// Always non-empty and at most [`MAX_BLOCK_ID_BYTES`] bytes. A `BlockId` is
/// only ever minted from a validated read (or deserialized under the same
/// checks), so a snapshot index lookup with a `BlockId` cannot be handed
/// arbitrary unbounded input.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BlockId(String);

impl BlockId {
    /// Returns the raw identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for BlockId {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value.is_empty() {
            return Err("block id must not be empty".to_owned());
        }
        if value.len() > MAX_BLOCK_ID_BYTES {
            return Err(format!("block id exceeds {MAX_BLOCK_ID_BYTES} bytes"));
        }
        Ok(Self(value))
    }
}

impl From<BlockId> for String {
    fn from(id: BlockId) -> Self {
        id.0
    }
}

/// Fully qualified block address: the only serialized form in which a block
/// ID travels to other layers, so an ID never loses its owning
/// `(space, object)` context.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockRef {
    /// Space the object lives in.
    pub space_id: String,
    /// Object the block belongs to.
    pub object_id: String,
    /// Block identity within that object.
    pub block_id: BlockId,
}

/// Validated color token (block backgrounds, text colors, mark colors).
///
/// The wire type is an open string because the server palette is data, not
/// schema. A token is accepted when it is non-empty, at most
/// [`MAX_COLOR_TOKEN_BYTES`] bytes, and consists of ASCII graphic characters
/// with no uppercase letters. Unknown-but-well-formed tokens read verbatim;
/// malformed tokens fail the read as malformed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ColorToken(String);

/// Documented Anytype palette tokens, plus the `default` sentinel.
pub const COLOR_TOKEN_PALETTE: &[&str] = &[
    "default", "grey", "yellow", "orange", "red", "pink", "purple", "blue", "ice", "teal", "lime",
];

impl ColorToken {
    /// Validates and wraps a color token.
    ///
    /// # Errors
    ///
    /// Returns [`AnytypeError::Validation`] when the token is empty,
    /// oversized, or not lowercase ASCII graphic text.
    pub fn new(token: impl Into<String>) -> Result<Self> {
        let token = token.into();
        Self::try_from(token).map_err(|message| AnytypeError::Validation { message })
    }

    /// Returns the raw token string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ColorToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for ColorToken {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value.is_empty() {
            return Err("color token must not be empty".to_owned());
        }
        if value.len() > MAX_COLOR_TOKEN_BYTES {
            return Err(format!("color token exceeds {MAX_COLOR_TOKEN_BYTES} bytes"));
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !byte.is_ascii_uppercase())
        {
            return Err("color token must be lowercase ASCII graphic text".to_owned());
        }
        Ok(Self(value))
    }
}

impl From<ColorToken> for String {
    fn from(token: ColorToken) -> Self {
        token.0
    }
}

// ============================================================================
// Structural enums
// ============================================================================

/// Horizontal block alignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HorizontalAlign {
    /// Left-aligned (server default).
    #[default]
    Left,
    /// Centered.
    Center,
    /// Right-aligned.
    Right,
    /// Justified.
    Justify,
}

impl HorizontalAlign {
    fn from_proto(raw: i32) -> Option<Self> {
        match model::block::Align::try_from(raw).ok()? {
            model::block::Align::Left => Some(Self::Left),
            model::block::Align::Center => Some(Self::Center),
            model::block::Align::Right => Some(Self::Right),
            model::block::Align::Justify => Some(Self::Justify),
        }
    }
}

/// Vertical block alignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerticalAlign {
    /// Top-aligned (server default).
    #[default]
    Top,
    /// Middle-aligned.
    Middle,
    /// Bottom-aligned.
    Bottom,
}

impl VerticalAlign {
    fn from_proto(raw: i32) -> Option<Self> {
        match model::block::VerticalAlign::try_from(raw).ok()? {
            model::block::VerticalAlign::Top => Some(Self::Top),
            model::block::VerticalAlign::Middle => Some(Self::Middle),
            model::block::VerticalAlign::Bottom => Some(Self::Bottom),
        }
    }
}

/// Per-block server restrictions, preserved verbatim.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BlockRestrictions {
    /// Reading this block is restricted.
    pub read: bool,
    /// Editing this block is restricted.
    pub edit: bool,
    /// Removing this block is restricted.
    pub remove: bool,
    /// Dragging this block is restricted.
    pub drag: bool,
    /// Dropping onto this block is restricted.
    pub drop_on: bool,
}

// ============================================================================
// Text content
// ============================================================================

/// Closed set of text block styles readable in v1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextStyle {
    /// Plain paragraph.
    Paragraph,
    /// Heading level 1.
    Header1,
    /// Heading level 2.
    Header2,
    /// Heading level 3.
    Header3,
    /// Heading level 4 (deprecated upstream; readable only).
    Header4,
    /// Quotation.
    Quote,
    /// Code block.
    Code,
    /// Object title (system singleton; readable only).
    Title,
    /// Object description (system singleton; readable only).
    Description,
    /// Checkbox item.
    Checkbox,
    /// Bulleted list item (proto `Marked`).
    Bulleted,
    /// Numbered list item.
    Numbered,
    /// Toggle.
    Toggle,
    /// Callout.
    Callout,
    /// Toggle heading level 1.
    ToggleHeader1,
    /// Toggle heading level 2.
    ToggleHeader2,
    /// Toggle heading level 3.
    ToggleHeader3,
}

impl TextStyle {
    fn from_proto(raw: i32) -> Option<Self> {
        use model::block::content::text::Style;
        match Style::try_from(raw).ok()? {
            Style::Paragraph => Some(Self::Paragraph),
            Style::Header1 => Some(Self::Header1),
            Style::Header2 => Some(Self::Header2),
            Style::Header3 => Some(Self::Header3),
            Style::Header4 => Some(Self::Header4),
            Style::Quote => Some(Self::Quote),
            Style::Code => Some(Self::Code),
            Style::Title => Some(Self::Title),
            Style::Checkbox => Some(Self::Checkbox),
            Style::Marked => Some(Self::Bulleted),
            Style::Numbered => Some(Self::Numbered),
            Style::Toggle => Some(Self::Toggle),
            Style::Description => Some(Self::Description),
            Style::Callout => Some(Self::Callout),
            Style::ToggleHeader1 => Some(Self::ToggleHeader1),
            Style::ToggleHeader2 => Some(Self::ToggleHeader2),
            Style::ToggleHeader3 => Some(Self::ToggleHeader3),
        }
    }
}

/// Half-open `[start, end)` range in UTF-16 code units, the protocol's
/// "symbol" unit for marks (matching the chat highlight-range precedent).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextRange {
    /// Inclusive start, in UTF-16 code units.
    pub start: u32,
    /// Exclusive end, in UTF-16 code units.
    pub end: u32,
}

impl TextRange {
    /// Converts a Rust byte range over `text` into a UTF-16 code-unit range.
    ///
    /// Returns `None` when either byte offset is out of bounds or does not
    /// fall on a `char` boundary, so callers never do UTF-16 math by hand.
    #[must_use]
    pub fn from_byte_range(text: &str, bytes: std::ops::Range<usize>) -> Option<Self> {
        if bytes.start > bytes.end {
            return None;
        }
        let start = utf16_offset_at_byte(text, bytes.start)?;
        let end = utf16_offset_at_byte(text, bytes.end)?;
        Some(Self { start, end })
    }

    /// Converts this UTF-16 code-unit range into a Rust byte range over
    /// `text`.
    ///
    /// Returns `None` when the range is inverted, out of bounds, or a bound
    /// splits a surrogate pair.
    #[must_use]
    pub fn to_byte_range(self, text: &str) -> Option<std::ops::Range<usize>> {
        if self.start > self.end {
            return None;
        }
        let start = byte_offset_at_utf16(text, self.start)?;
        let end = byte_offset_at_utf16(text, self.end)?;
        Some(start..end)
    }
}

/// Returns the UTF-16 code-unit length of `text`.
#[must_use]
pub fn utf16_len(text: &str) -> u32 {
    let mut total: u32 = 0;
    for character in text.chars() {
        total = total.saturating_add(character.len_utf16() as u32);
    }
    total
}

fn utf16_offset_at_byte(text: &str, byte_offset: usize) -> Option<u32> {
    if byte_offset > text.len() || !text.is_char_boundary(byte_offset) {
        return None;
    }
    Some(utf16_len(&text[..byte_offset]))
}

fn byte_offset_at_utf16(text: &str, utf16_offset: u32) -> Option<usize> {
    let mut units: u32 = 0;
    for (byte_index, character) in text.char_indices() {
        if units == utf16_offset {
            return Some(byte_index);
        }
        if units > utf16_offset {
            // The requested offset splits a surrogate pair.
            return None;
        }
        units += character.len_utf16() as u32;
    }
    (units == utf16_offset).then_some(text.len())
}

/// Closed set of inline mark kinds readable in v1.
///
/// An unknown mark type is mutation-critical (`BlockTextSetText` replaces the
/// whole mark list), so a text block carrying one reads as
/// [`BlockContent::Unsupported`] instead of a `TextContent` with holes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MarkKind {
    /// Bold.
    Bold,
    /// Italic.
    Italic,
    /// Strikethrough.
    Strikethrough,
    /// Underline (proto `Underscored`).
    Underline,
    /// Inline code (proto `Keyboard`).
    Code,
    /// Hyperlink.
    Link {
        /// Link target URL, preserved verbatim.
        url: String,
    },
    /// Foreground text color.
    TextColor {
        /// Validated color token.
        color: ColorToken,
    },
    /// Background text color.
    BackgroundColor {
        /// Validated color token.
        color: ColorToken,
    },
    /// Mention of another object.
    Mention {
        /// Mentioned object ID.
        object_id: String,
    },
    /// Inline emoji substitution.
    Emoji {
        /// Emoji string.
        emoji: String,
    },
    /// Inline object link.
    Object {
        /// Linked object ID.
        object_id: String,
    },
}

/// One inline mark: a validated range plus a typed kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TextMark {
    /// UTF-16 code-unit range the mark covers, validated against the text.
    pub range: TextRange,
    /// Typed mark kind.
    pub kind: MarkKind,
}

impl TextMark {
    /// Creates an inline mark. Its range is validated against the enclosing
    /// text when the mark is used in a constructor or mutation.
    #[must_use]
    pub fn new(range: TextRange, kind: MarkKind) -> Self {
        Self { range, kind }
    }
}

/// Callout icon attached to a `Callout`-styled text block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "content", rename_all = "snake_case")]
pub enum CalloutIcon {
    /// Emoji icon.
    Emoji(String),
    /// Image icon referencing a file object ID. When the server sends both an
    /// emoji and an image, the image wins (upstream UI contract).
    Image(String),
}

/// Typed text block content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct TextContent {
    /// Plain text, at most [`BodyLimits::max_text_bytes`] bytes.
    pub text: String,
    /// Block style.
    pub style: TextStyle,
    /// Checkbox state, preserved verbatim for every style.
    pub checked: bool,
    /// Validated text color; `None` when the server sends an empty string.
    pub color: Option<ColorToken>,
    /// Callout icon, when present.
    pub icon: Option<CalloutIcon>,
    /// Inline marks, at most [`BodyLimits::max_marks_per_text`].
    pub marks: Vec<TextMark>,
}

// ============================================================================
// Non-text content
// ============================================================================

/// Structural layout style (server-managed; readable only).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutStyle {
    /// Horizontal row container.
    Row,
    /// Vertical column container.
    Column,
    /// Divider container.
    Div,
    /// Object header region.
    Header,
    /// Table rows region.
    TableRows,
    /// Table columns region.
    TableColumns,
}

impl LayoutStyle {
    fn from_proto(raw: i32) -> Option<Self> {
        use model::block::content::layout::Style;
        match Style::try_from(raw).ok()? {
            Style::Row => Some(Self::Row),
            Style::Column => Some(Self::Column),
            Style::Div => Some(Self::Div),
            Style::Header => Some(Self::Header),
            Style::TableRows => Some(Self::TableRows),
            Style::TableColumns => Some(Self::TableColumns),
        }
    }
}

/// Divider style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DividerStyle {
    /// Thin line.
    Line,
    /// Dotted line.
    Dots,
}

impl DividerStyle {
    fn from_proto(raw: i32) -> Option<Self> {
        use model::block::content::div::Style;
        match Style::try_from(raw).ok()? {
            Style::Line => Some(Self::Line),
            Style::Dots => Some(Self::Dots),
        }
    }
}

/// Bookmark fetch state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookmarkState {
    /// No URL set yet.
    Empty,
    /// The server is fetching the preview.
    Fetching,
    /// Preview fetched.
    Done,
    /// Fetch failed.
    Error,
}

impl BookmarkState {
    fn from_proto(raw: i32) -> Option<Self> {
        use model::block::content::bookmark::State;
        match State::try_from(raw).ok()? {
            State::Empty => Some(Self::Empty),
            State::Fetching => Some(Self::Fetching),
            State::Done => Some(Self::Done),
            State::Error => Some(Self::Error),
        }
    }
}

/// Typed bookmark block content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct BookmarkContent {
    /// Bookmarked URL, preserved verbatim.
    pub url: String,
    /// Backing bookmark object, when the server has created one.
    pub target_object_id: Option<String>,
    /// Fetch state.
    pub state: BookmarkState,
}

/// Link card presentation style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkCardStyle {
    /// Inline text link.
    Text,
    /// Card presentation.
    Card,
    /// Inline embed presentation.
    Inline,
}

/// Link card icon size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkIconSize {
    /// No icon.
    None,
    /// Small icon.
    Small,
    /// Medium icon.
    Medium,
}

/// Link card description mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkDescriptionMode {
    /// No description.
    None,
    /// Manually added description.
    Added,
    /// Description derived from content.
    Content,
}

/// Typed link card content referencing another object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct LinkCard {
    /// Target object ID.
    pub target_object_id: String,
    /// Card presentation style.
    pub card_style: LinkCardStyle,
    /// Icon size.
    pub icon_size: LinkIconSize,
    /// Description mode.
    pub description: LinkDescriptionMode,
    /// Bounded relation keys displayed by the card, in exact server order.
    pub relations: Vec<String>,
}

/// Typed relation view content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct RelationView {
    /// Relation key displayed by the block.
    pub key: String,
}

/// Closed set of embed processors readable as typed content in v1.
///
/// The remaining processors (`Chart`, `Vimeo`, `Soundcloud`, ...) read as
/// [`BlockContent::Unsupported`]: their payloads are URLs or scripts whose
/// safety and normalization have not been reviewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbedProcessor {
    /// LaTeX source.
    Latex,
    /// Mermaid diagram source.
    Mermaid,
    /// `YouTube` video URL.
    Youtube,
}

/// Typed embed content (proto `Latex` content).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct EmbedContent {
    /// Embed processor.
    pub processor: EmbedProcessor,
    /// Embed source text, at most [`BodyLimits::max_embed_text_bytes`] bytes.
    pub text: String,
}

impl EmbedContent {
    /// Creates a bounded embed value for an update.
    ///
    /// # Errors
    ///
    /// Returns [`AnytypeError::Validation`]
    /// when `text` exceeds [`BodyLimits::max_embed_text_bytes`]. Processor-
    /// specific mutation policy is checked by the body editor.
    pub fn new(processor: EmbedProcessor, text: impl Into<String>) -> crate::Result<Self> {
        let text = text.into();
        if text.len() > MAX_EMBED_TEXT_BYTES {
            return Err(crate::error::AnytypeError::Validation {
                message: format!("embed text exceeds {MAX_EMBED_TEXT_BYTES} bytes"),
            });
        }
        Ok(Self { processor, text })
    }
}

/// File kind shown by a file block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileBlockKind {
    /// Not determined yet.
    None,
    /// Generic file.
    File,
    /// Image.
    Image,
    /// Video.
    Video,
    /// Audio.
    Audio,
    /// PDF document.
    Pdf,
}

/// Upload state of a file block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileBlockState {
    /// Empty placeholder awaiting a file.
    Empty,
    /// Upload in progress.
    Uploading,
    /// File available.
    Done,
    /// Upload failed.
    Error,
}

/// Presentation style of a file block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileBlockStyle {
    /// Automatic (embed for media kinds).
    Auto,
    /// Link presentation.
    Link,
    /// Embed presentation.
    Embed,
}

/// Typed file block view (read-only in v1; file placement stays in the files
/// surface).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct FileView {
    /// Backing file object ID.
    pub target_object_id: String,
    /// File kind.
    pub kind: FileBlockKind,
    /// MIME type reported by the server.
    pub mime: String,
    /// File size in bytes as reported by the server.
    pub size: i64,
    /// Upload state.
    pub state: FileBlockState,
    /// Presentation style.
    pub style: FileBlockStyle,
}

/// Content-free summary of an unsupported block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct OpaqueSummary {
    /// Number of direct children.
    pub child_count: usize,
    /// Approximate encoded size of the raw block, in bytes.
    pub approx_bytes: usize,
}

/// The single escape hatch for content the typed layer does not model.
///
/// The summary is content-free: it never carries block text, URLs, file
/// names, or protobuf bytes. Opaque blocks keep their exact ID, children,
/// alignment, and background so tree shape, order, and traversal stay
/// complete and correct. They are read-only anchors for the mutation layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct OpaqueContent {
    /// Stable discriminant: the proto oneof variant name in snake case
    /// (e.g. `"dataview"`, `"widget"`), `"unknown"` for unrecognized tags,
    /// or a qualified reason such as `"text_unknown_mark"`.
    pub kind: String,
    /// Bounded, content-free structural summary.
    pub summary: OpaqueSummary,
}

/// Closed, typed block content.
///
/// The enum is `#[non_exhaustive]`: future crate revisions may add variants.
/// Anything the layer cannot model reads as [`BlockContent::Unsupported`].
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", content = "content", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlockContent {
    /// Text in any supported style.
    Text(TextContent),
    /// Structural layout container (server-managed).
    Layout(LayoutStyle),
    /// Divider.
    Divider(DividerStyle),
    /// Web bookmark.
    Bookmark(BookmarkContent),
    /// Link card to another object.
    Link(LinkCard),
    /// Inline relation view.
    Relation(RelationView),
    /// Featured-relations system block (singleton).
    FeaturedRelations,
    /// LaTeX/Mermaid/YouTube embed.
    Embed(EmbedContent),
    /// Table of contents.
    TableOfContents,
    /// Table root.
    Table,
    /// Table row.
    TableRow {
        /// Whether this row is a header row.
        is_header: bool,
    },
    /// Table column.
    TableColumn,
    /// File block view.
    File(FileView),
    /// Content the typed layer does not model, read fail-closed.
    Unsupported(OpaqueContent),
}

// ============================================================================
// Blocks and snapshots
// ============================================================================

/// One block with its exact identity, typed content, and ordered children.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct BodyBlock {
    /// Exact server-assigned block ID.
    pub id: BlockId,
    /// Typed content.
    pub content: BlockContent,
    /// Ordered child IDs, exactly as returned by the server.
    pub children: Vec<BlockId>,
    /// Horizontal alignment.
    pub align: HorizontalAlign,
    /// Vertical alignment.
    pub vertical_align: VerticalAlign,
    /// Validated background color; `None` when the server sends an empty
    /// string.
    pub background_color: Option<ColorToken>,
    /// Server-reported per-block restrictions.
    pub restrictions: BlockRestrictions,
}

/// A validated, bounded, ordered body tree read from one object at one
/// moment.
///
/// The snapshot preserves exactly what the server returned after validation:
/// no reordering, no merging, no synthetic blocks. Blocks are stored in
/// depth-first document order. `Deserialize` is deliberately not implemented,
/// so a snapshot can never be forged from JSON and passed back as read
/// evidence.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct BodySnapshot {
    /// Space the object was read from.
    pub space_id: String,
    /// Object the body belongs to.
    pub object_id: String,
    /// Root block ID.
    pub root_id: BlockId,
    /// Blocks in depth-first document order; the root is first.
    blocks: Vec<BodyBlock>,
    /// Index from block ID to arena position.
    #[serde(skip)]
    index: HashMap<BlockId, usize>,
}

impl BodySnapshot {
    /// Returns the root block.
    #[must_use]
    pub fn root(&self) -> &BodyBlock {
        &self.blocks[0]
    }

    /// Returns the block with the given ID, if present.
    #[must_use]
    pub fn get(&self, id: &BlockId) -> Option<&BodyBlock> {
        self.index.get(id).map(|&position| &self.blocks[position])
    }

    /// Returns the ordered children of the given block, in exact server
    /// order. Unknown IDs yield an empty slice.
    #[must_use]
    pub fn children(&self, id: &BlockId) -> &[BlockId] {
        self.get(id).map_or(&[], |block| block.children.as_slice())
    }

    /// Iterates all blocks in depth-first document order, root first.
    pub fn iter(&self) -> impl Iterator<Item = &BodyBlock> {
        self.blocks.iter()
    }

    /// Returns the number of blocks in the snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Returns whether the snapshot contains no blocks. A validated snapshot
    /// always contains at least the root, so this is normally `false`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Returns the fully qualified address of the given block, if present.
    #[must_use]
    pub fn block_ref(&self, id: &BlockId) -> Option<BlockRef> {
        self.get(id).map(|block| BlockRef {
            space_id: self.space_id.clone(),
            object_id: self.object_id.clone(),
            block_id: block.id.clone(),
        })
    }
}

/// Narrow typed body fixtures for downstream contract tests.
///
/// The module is absent from ordinary builds so production callers cannot
/// forge a [`BodySnapshot`] and pass it off as server evidence.
#[cfg(feature = "test-fixtures")]
#[doc(hidden)]
pub mod test_fixtures {
    use super::*;
    use model::block::{ContentValue, content};

    /// Deliberate table-shape defect for downstream verifier tests.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum TableFixtureDefect {
        /// Exact two-by-two canonical table.
        None,
        /// One row omits a cell.
        MissingCell,
        /// One row contains an extra cell.
        ExtraCell,
        /// One cell has a structural content type.
        WrongCellType,
        /// One cell contains text.
        NonemptyCell,
        /// One cell uses non-default presentation.
        WrongCellPresentation,
        /// One header cell omits its required grey background.
        WrongCellBackground,
        /// One cell has a child.
        CellWithChild,
        /// Table regions appear in the wrong order.
        ReversedRegions,
    }

    /// Builds a valid body graph containing one two-by-two table, optionally
    /// with one deliberate semantic shape defect.
    #[must_use]
    pub fn table_snapshot(defect: TableFixtureDefect) -> Option<BodySnapshot> {
        table_snapshot_with_header(true, defect)
    }

    /// Builds a valid sparse two-by-two table with or without a header row,
    /// optionally with one deliberate semantic shape defect.
    #[must_use]
    pub fn table_snapshot_with_header(
        header_row: bool,
        defect: TableFixtureDefect,
    ) -> Option<BodySnapshot> {
        let block = |id: &str, children: &[&str], content_value| model::Block {
            id: id.to_owned(),
            fields: None,
            restrictions: None,
            children_ids: children.iter().map(|child| (*child).to_owned()).collect(),
            background_color: String::new(),
            align: 0,
            vertical_align: 0,
            content_value: Some(content_value),
        };
        let text = |id: &str, value: &str| {
            block(
                id,
                &[],
                ContentValue::Text(content::Text {
                    text: value.to_owned(),
                    ..Default::default()
                }),
            )
        };
        let header_cell = |id: &str, value: &str| {
            let mut cell = text(id, value);
            cell.background_color = "grey".to_owned();
            cell
        };
        let table_children = if defect == TableFixtureDefect::ReversedRegions {
            ["rows", "columns"]
        } else {
            ["columns", "rows"]
        };
        let row_one_cells: &[&str] = if !header_row {
            &[]
        } else if defect == TableFixtureDefect::MissingCell {
            &["r1c1"]
        } else {
            &["r1c1", "r1c2"]
        };
        let row_two_cells: &[&str] = if defect == TableFixtureDefect::ExtraCell {
            &["r2c1"]
        } else {
            &[]
        };
        let cell_one_children: &[&str] = if defect == TableFixtureDefect::CellWithChild {
            &["nested"]
        } else {
            &[]
        };
        let mut blocks = vec![
            block(
                "root",
                &["table"],
                ContentValue::Smartblock(content::Smartblock {}),
            ),
            block(
                "table",
                &table_children,
                ContentValue::Table(content::Table {}),
            ),
            block(
                "columns",
                &["c1", "c2"],
                ContentValue::Layout(content::Layout {
                    style: content::layout::Style::TableColumns as i32,
                }),
            ),
            block(
                "c1",
                &[],
                ContentValue::TableColumn(content::TableColumn {}),
            ),
            block(
                "c2",
                &[],
                ContentValue::TableColumn(content::TableColumn {}),
            ),
            block(
                "rows",
                &["r1", "r2"],
                ContentValue::Layout(content::Layout {
                    style: content::layout::Style::TableRows as i32,
                }),
            ),
            block(
                "r1",
                row_one_cells,
                ContentValue::TableRow(content::TableRow {
                    is_header: header_row,
                }),
            ),
            block(
                "r2",
                row_two_cells,
                ContentValue::TableRow(content::TableRow { is_header: false }),
            ),
        ];
        if header_row {
            blocks.push(block(
                "r1c1",
                cell_one_children,
                if defect == TableFixtureDefect::WrongCellType {
                    ContentValue::TableColumn(content::TableColumn {})
                } else {
                    ContentValue::Text(content::Text {
                        text: if defect == TableFixtureDefect::NonemptyCell {
                            "not empty".to_owned()
                        } else {
                            String::new()
                        },
                        ..Default::default()
                    })
                },
            ));
            if let Some(cell) = blocks.iter_mut().find(|block| block.id == "r1c1") {
                cell.background_color = "grey".to_owned();
            }
            if defect != TableFixtureDefect::MissingCell {
                blocks.push(header_cell("r1c2", ""));
            }
        }
        if defect == TableFixtureDefect::ExtraCell {
            blocks.push(text("r2c1", ""));
        }
        if defect == TableFixtureDefect::CellWithChild {
            blocks.push(text("nested", ""));
        }
        if defect == TableFixtureDefect::WrongCellPresentation
            && let Some(cell) = blocks.iter_mut().find(|block| block.id == "r1c1")
        {
            cell.align = model::block::Align::Center as i32;
        }
        if defect == TableFixtureDefect::WrongCellBackground
            && let Some(cell) = blocks.iter_mut().find(|block| block.id == "r1c1")
        {
            cell.background_color.clear();
        }
        let view = model::ObjectView {
            root_id: "root".to_owned(),
            blocks,
            ..Default::default()
        };
        snapshot_from_view(
            "fixture-space",
            "fixture-object",
            &view,
            &BodyLimits::default(),
        )
        .ok()
    }

    /// Builds one valid depth-first text tree with exactly `block_count`
    /// blocks. `restricted_index` addresses the resulting depth-first block
    /// order and marks only that block read-restricted.
    ///
    /// This fixture intentionally supports only the two production-cap
    /// boundary sizes and small restriction tests.
    #[must_use]
    pub fn bounded_text_snapshot(
        block_count: usize,
        restricted_index: Option<usize>,
    ) -> Option<BodySnapshot> {
        if block_count == 0 || block_count > 2_049 {
            return None;
        }
        let container_count = block_count.saturating_sub(1).div_ceil(513);
        let leaf_count = block_count.saturating_sub(1 + container_count);
        let mut container_sizes = vec![0usize; container_count];
        for index in 0..leaf_count {
            if let Some(size) = container_sizes.get_mut(index % container_count.max(1)) {
                *size = size.saturating_add(1);
            }
        }
        let container_ids = (0..container_count)
            .map(|index| format!("fixture-layout-{index}"))
            .collect::<Vec<_>>();
        let block = |id: String, children_ids: Vec<String>, content_value| model::Block {
            id,
            fields: None,
            restrictions: None,
            children_ids,
            background_color: String::new(),
            align: 0,
            vertical_align: 0,
            content_value,
        };
        let mut blocks = Vec::with_capacity(block_count);
        blocks.push(block(
            "fixture-root".to_owned(),
            container_ids.clone(),
            Some(ContentValue::Smartblock(content::Smartblock {})),
        ));
        let mut leaf_index = 0usize;
        for (container_index, (container_id, size)) in
            container_ids.into_iter().zip(container_sizes).enumerate()
        {
            let child_ids = (0..size)
                .map(|offset| format!("fixture-text-{}", leaf_index.saturating_add(offset)))
                .collect::<Vec<_>>();
            blocks.push(block(
                container_id,
                child_ids.clone(),
                Some(ContentValue::Smartblock(content::Smartblock {})),
            ));
            for (offset, id) in child_ids.into_iter().enumerate() {
                blocks.push(block(
                    id,
                    Vec::new(),
                    Some(ContentValue::Text(content::Text {
                        text: format!("fixture {container_index} {offset}"),
                        style: 0,
                        marks: None,
                        checked: false,
                        color: String::new(),
                        icon_emoji: String::new(),
                        icon_image: String::new(),
                    })),
                ));
            }
            leaf_index = leaf_index.saturating_add(size);
        }
        let view = model::ObjectView {
            root_id: "fixture-root".to_owned(),
            blocks,
            ..Default::default()
        };
        let limits = BodyLimits {
            max_blocks: block_count,
            max_children: 512,
            ..BodyLimits::default()
        }
        .clamped();
        let mut snapshot =
            snapshot_from_view("fixture-space", "fixture-object", &view, &limits).ok()?;
        if let Some(block) = restricted_index.and_then(|index| snapshot.blocks.get_mut(index)) {
            block.restrictions.read = true;
        }
        Some(snapshot)
    }
}

// ============================================================================
// Graph validation errors
// ============================================================================

/// Closed classification of body graph validation failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BodyGraphErrorKind {
    /// `root_id` is empty or not present in the block list.
    MissingRoot,
    /// Two blocks share an ID.
    DuplicateBlock,
    /// A `children_ids` entry references an ID not in the list.
    DanglingChild,
    /// A block is referenced as a child by more than one parent, or is the
    /// root and has a parent.
    SharedChild,
    /// The parent/child relation is not acyclic.
    Cycle,
    /// A returned block is unreachable from the root.
    Orphaned,
    /// Empty or oversized ID, invalid structural enum value, malformed color
    /// token, or invalid mark range.
    MalformedBlock,
    /// A bound in the effective [`BodyLimits`] is exceeded.
    Oversized,
}

impl fmt::Display for BodyGraphErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::MissingRoot => "missing_root",
            Self::DuplicateBlock => "duplicate_block",
            Self::DanglingChild => "dangling_child",
            Self::SharedChild => "shared_child",
            Self::Cycle => "cycle",
            Self::Orphaned => "orphaned",
            Self::MalformedBlock => "malformed_block",
            Self::Oversized => "oversized",
        };
        formatter.write_str(name)
    }
}

/// Internal validation violation carrying an ID-and-kind-only detail string.
struct Violation {
    kind: BodyGraphErrorKind,
    detail: String,
}

impl Violation {
    fn new(kind: BodyGraphErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

fn graph_error(object_id: &str, violation: Violation) -> AnytypeError {
    AnytypeError::BodyGraph {
        object_id: object_id.to_owned(),
        kind: violation.kind,
        detail: violation.detail,
    }
}

// ============================================================================
// Client surface
// ============================================================================

/// Typed body-block operations (gRPC-backed).
#[derive(Debug)]
pub struct BlocksClient<'a> {
    client: &'a AnytypeClient,
}

impl AnytypeClient {
    /// Typed body-block operations (gRPC-backed).
    #[must_use]
    pub fn blocks(&self) -> BlocksClient<'_> {
        BlocksClient { client: self }
    }
}

impl<'a> BlocksClient<'a> {
    /// Builds a bounded body read for one object.
    pub fn body(
        &self,
        space_id: impl Into<String>,
        object_id: impl Into<String>,
    ) -> BodyRequest<'a> {
        BodyRequest {
            client: self.client,
            space_id: space_id.into(),
            object_id: object_id.into(),
            limits: BodyLimits::default(),
            rpc: None,
        }
    }
}

/// Builder for a bounded body read over gRPC `ObjectShow`.
#[derive(Debug)]
pub struct BodyRequest<'a> {
    client: &'a AnytypeClient,
    space_id: String,
    object_id: String,
    limits: BodyLimits,
    rpc: Option<BodyRpcConfig>,
}

impl BodyRequest<'_> {
    /// Tightens limits below the hard ceilings; values above a ceiling clamp
    /// back down to it.
    #[must_use]
    pub fn limits(mut self, limits: BodyLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Uses one finite gRPC configuration for acquisition, show, and close.
    ///
    /// Cloning and reusing the same configuration for a subsequent
    /// [`BodyEditor`](crate::body_mutation::BodyEditor) shares its absolute
    /// deadline and payload-free counters across the complete operation.
    #[must_use]
    pub fn rpc_config(mut self, config: BodyRpcConfig) -> Self {
        self.rpc = Some(config);
        self
    }

    /// Executes `ObjectShow`, validates the returned graph, and returns the
    /// snapshot. Every possibly accepted show owns a bounded close; a cleanup
    /// failure takes precedence over the show or application response.
    ///
    /// # Errors
    ///
    /// Returns [`AnytypeError::BodyGraph`] when the returned graph is
    /// duplicate, cyclic, orphaned, oversized, or malformed, and the ordinary
    /// transport/authentication errors otherwise.
    pub async fn fetch(self) -> Result<BodySnapshot> {
        let limits = self.limits.clamped();
        let rpc = self.rpc.unwrap_or_default();
        let view = fetch_object_view(self.client, &self.space_id, &self.object_id, &rpc).await?;
        snapshot_from_view(&self.space_id, &self.object_id, &view, &limits)
    }
}

// ============================================================================
// Conversion and validation
// ============================================================================

/// Validates an `ObjectShow` view and converts it into a [`BodySnapshot`].
///
/// `limits` must already be clamped. On any violation the whole read fails; a
/// partial tree is never returned.
pub(crate) fn snapshot_from_view(
    space_id: &str,
    object_id: &str,
    view: &model::ObjectView,
    limits: &BodyLimits,
) -> Result<BodySnapshot> {
    validate_and_convert(space_id, object_id, view, limits)
        .map_err(|violation| graph_error(object_id, violation))
}

fn validate_and_convert(
    space_id: &str,
    object_id: &str,
    view: &model::ObjectView,
    limits: &BodyLimits,
) -> std::result::Result<BodySnapshot, Violation> {
    let blocks = &view.blocks;

    if blocks.len() > limits.max_blocks {
        return Err(Violation::new(
            BodyGraphErrorKind::Oversized,
            format!(
                "view returned {} blocks, limit {}",
                blocks.len(),
                limits.max_blocks
            ),
        ));
    }
    if view.root_id.is_empty() {
        return Err(Violation::new(
            BodyGraphErrorKind::MissingRoot,
            "root_id is empty",
        ));
    }

    // Index all blocks, rejecting malformed and duplicate IDs.
    let mut index: HashMap<&str, usize> = HashMap::with_capacity(blocks.len());
    for (position, block) in blocks.iter().enumerate() {
        if block.id.is_empty() {
            return Err(Violation::new(
                BodyGraphErrorKind::MalformedBlock,
                format!("block at position {position} has an empty id"),
            ));
        }
        if block.id.len() > limits.max_block_id_bytes {
            return Err(Violation::new(
                BodyGraphErrorKind::MalformedBlock,
                format!(
                    "block at position {position} has an id longer than {} bytes",
                    limits.max_block_id_bytes
                ),
            ));
        }
        if index.insert(block.id.as_str(), position).is_some() {
            return Err(Violation::new(
                BodyGraphErrorKind::DuplicateBlock,
                format!("duplicate block id {}", block.id),
            ));
        }
    }

    let Some(&root_position) = index.get(view.root_id.as_str()) else {
        return Err(Violation::new(
            BodyGraphErrorKind::MissingRoot,
            format!("root {} is not in the block list", view.root_id),
        ));
    };

    // Validate edges: bounded fanout, no dangling references, single parent,
    // parentless root.
    let mut in_degree: Vec<usize> = vec![0; blocks.len()];
    for block in blocks {
        if block.children_ids.len() > limits.max_children {
            return Err(Violation::new(
                BodyGraphErrorKind::Oversized,
                format!(
                    "block {} has {} children, limit {}",
                    block.id,
                    block.children_ids.len(),
                    limits.max_children
                ),
            ));
        }
        for child in &block.children_ids {
            let Some(&child_position) = index.get(child.as_str()) else {
                return Err(Violation::new(
                    BodyGraphErrorKind::DanglingChild,
                    format!("block {} references unknown child {child}", block.id),
                ));
            };
            if child_position == root_position {
                return Err(Violation::new(
                    BodyGraphErrorKind::SharedChild,
                    format!("root {child} is referenced as a child of {}", block.id),
                ));
            }
            in_degree[child_position] += 1;
            if in_degree[child_position] > 1 {
                return Err(Violation::new(
                    BodyGraphErrorKind::SharedChild,
                    format!("block {child} has more than one parent"),
                ));
            }
        }
    }

    detect_cycles(blocks, &index)?;

    // Reachability and depth from the root. After the single-parent and
    // acyclic checks the reachable component is a tree.
    let mut visited = vec![false; blocks.len()];
    let mut stack: Vec<(usize, usize)> = vec![(root_position, 1)];
    let mut reached = 0_usize;
    while let Some((position, depth)) = stack.pop() {
        if depth > limits.max_depth {
            return Err(Violation::new(
                BodyGraphErrorKind::Oversized,
                format!(
                    "block {} sits at depth {depth}, limit {}",
                    blocks[position].id, limits.max_depth
                ),
            ));
        }
        visited[position] = true;
        reached += 1;
        for child in &blocks[position].children_ids {
            let child_position = index[child.as_str()];
            stack.push((child_position, depth + 1));
        }
    }
    if reached != blocks.len() {
        let orphan = blocks
            .iter()
            .enumerate()
            .find(|(position, _)| !visited[*position])
            .map(|(_, block)| block.id.clone())
            .unwrap_or_default();
        return Err(Violation::new(
            BodyGraphErrorKind::Orphaned,
            format!("block {orphan} is unreachable from the root"),
        ));
    }

    // Table fanout bounds.
    for block in blocks {
        let mut rows = 0_usize;
        let mut columns = 0_usize;
        for child in &block.children_ids {
            match blocks[index[child.as_str()]].content_value {
                Some(model::block::ContentValue::TableRow(_)) => rows += 1,
                Some(model::block::ContentValue::TableColumn(_)) => columns += 1,
                _ => {}
            }
        }
        if rows > limits.max_table_rows {
            return Err(Violation::new(
                BodyGraphErrorKind::Oversized,
                format!(
                    "block {} has {rows} table rows, limit {}",
                    block.id, limits.max_table_rows
                ),
            ));
        }
        if columns > limits.max_table_columns {
            return Err(Violation::new(
                BodyGraphErrorKind::Oversized,
                format!(
                    "block {} has {columns} table columns, limit {}",
                    block.id, limits.max_table_columns
                ),
            ));
        }
    }

    // Convert every block, then assemble the arena in depth-first document
    // order (children pushed in reverse for pre-order traversal).
    let mut converted: Vec<Option<BodyBlock>> = blocks
        .iter()
        .map(|block| convert_block(block, limits))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(Some)
        .collect();

    let mut arena: Vec<BodyBlock> = Vec::with_capacity(blocks.len());
    let mut arena_index: HashMap<BlockId, usize> = HashMap::with_capacity(blocks.len());
    let mut order: Vec<usize> = vec![root_position];
    while let Some(position) = order.pop() {
        for child in blocks[position].children_ids.iter().rev() {
            order.push(index[child.as_str()]);
        }
        let block = converted[position]
            .take()
            .expect("tree validation guarantees single visitation");
        arena_index.insert(block.id.clone(), arena.len());
        arena.push(block);
    }

    let root_id = arena[0].id.clone();
    Ok(BodySnapshot {
        space_id: space_id.to_owned(),
        object_id: object_id.to_owned(),
        root_id,
        blocks: arena,
        index: arena_index,
    })
}

/// Iterative white/grey/black depth-first search over the full edge set.
fn detect_cycles(
    blocks: &[model::Block],
    index: &HashMap<&str, usize>,
) -> std::result::Result<(), Violation> {
    const WHITE: u8 = 0;
    const GREY: u8 = 1;
    const BLACK: u8 = 2;
    let mut color = vec![WHITE; blocks.len()];
    for start in 0..blocks.len() {
        if color[start] != WHITE {
            continue;
        }
        // Stack entries: (node, next child offset).
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        color[start] = GREY;
        while let Some(&(node, next_child)) = stack.last() {
            let children = &blocks[node].children_ids;
            if next_child < children.len() {
                stack.last_mut().expect("stack is non-empty").1 += 1;
                let child = index[children[next_child].as_str()];
                match color[child] {
                    WHITE => {
                        color[child] = GREY;
                        stack.push((child, 0));
                    }
                    GREY => {
                        return Err(Violation::new(
                            BodyGraphErrorKind::Cycle,
                            format!("block {} participates in a cycle", blocks[child].id),
                        ));
                    }
                    _ => {}
                }
            } else {
                color[node] = BLACK;
                stack.pop();
            }
        }
    }
    Ok(())
}

fn convert_block(
    block: &model::Block,
    limits: &BodyLimits,
) -> std::result::Result<BodyBlock, Violation> {
    let id = BlockId(block.id.clone());
    let align = HorizontalAlign::from_proto(block.align).ok_or_else(|| {
        Violation::new(
            BodyGraphErrorKind::MalformedBlock,
            format!("block {} has invalid align value {}", block.id, block.align),
        )
    })?;
    let vertical_align = VerticalAlign::from_proto(block.vertical_align).ok_or_else(|| {
        Violation::new(
            BodyGraphErrorKind::MalformedBlock,
            format!(
                "block {} has invalid vertical align value {}",
                block.id, block.vertical_align
            ),
        )
    })?;
    let background_color = convert_color(&block.background_color, &block.id, "background color")?;
    let restrictions = block
        .restrictions
        .map(|restrictions| BlockRestrictions {
            read: restrictions.read,
            edit: restrictions.edit,
            remove: restrictions.remove,
            drag: restrictions.drag,
            drop_on: restrictions.drop_on,
        })
        .unwrap_or_default();
    let children = block
        .children_ids
        .iter()
        .map(|child| BlockId(child.clone()))
        .collect();
    let content = convert_content(block, limits)?;
    Ok(BodyBlock {
        id,
        content,
        children,
        align,
        vertical_align,
        background_color,
        restrictions,
    })
}

/// Converts a wire color string: empty means absent, well-formed tokens read
/// verbatim, and malformed tokens fail the read.
fn convert_color(
    raw: &str,
    block_id: &str,
    what: &str,
) -> std::result::Result<Option<ColorToken>, Violation> {
    if raw.is_empty() {
        return Ok(None);
    }
    ColorToken::try_from(raw.to_owned()).map(Some).map_err(|_| {
        Violation::new(
            BodyGraphErrorKind::MalformedBlock,
            format!("block {block_id} has a malformed {what} token"),
        )
    })
}

fn opaque(block: &model::Block, kind: impl Into<String>) -> BlockContent {
    BlockContent::Unsupported(OpaqueContent {
        kind: kind.into(),
        summary: OpaqueSummary {
            child_count: block.children_ids.len(),
            approx_bytes: block.encoded_len(),
        },
    })
}

fn convert_content(
    block: &model::Block,
    limits: &BodyLimits,
) -> std::result::Result<BlockContent, Violation> {
    use model::block::ContentValue;
    let Some(content) = &block.content_value else {
        return Ok(opaque(block, "unknown"));
    };
    match content {
        ContentValue::Text(text) => convert_text(block, text, limits),
        ContentValue::Layout(layout) => Ok(LayoutStyle::from_proto(layout.style).map_or_else(
            || opaque(block, "layout_unknown_style"),
            BlockContent::Layout,
        )),
        ContentValue::Div(div) => Ok(DividerStyle::from_proto(div.style)
            .map_or_else(|| opaque(block, "div_unknown_style"), BlockContent::Divider)),
        ContentValue::Bookmark(bookmark) => Ok(BookmarkState::from_proto(bookmark.state)
            .map_or_else(
                || opaque(block, "bookmark_unknown_state"),
                |state| {
                    BlockContent::Bookmark(BookmarkContent {
                        url: bookmark.url.clone(),
                        target_object_id: (!bookmark.target_object_id.is_empty())
                            .then(|| bookmark.target_object_id.clone()),
                        state,
                    })
                },
            )),
        ContentValue::Link(link) => convert_link(block, link),
        ContentValue::Relation(relation) => Ok(BlockContent::Relation(RelationView {
            key: relation.key.clone(),
        })),
        ContentValue::FeaturedRelations(_) => Ok(BlockContent::FeaturedRelations),
        ContentValue::Latex(latex) => convert_embed(block, latex, limits),
        ContentValue::TableOfContents(_) => Ok(BlockContent::TableOfContents),
        ContentValue::Table(_) => Ok(BlockContent::Table),
        ContentValue::TableRow(row) => Ok(BlockContent::TableRow {
            is_header: row.is_header,
        }),
        ContentValue::TableColumn(_) => Ok(BlockContent::TableColumn),
        ContentValue::File(file) => Ok(convert_file(block, file)),
        ContentValue::Smartblock(_) => Ok(opaque(block, "smartblock")),
        ContentValue::Dataview(_) => Ok(opaque(block, "dataview")),
        ContentValue::Widget(_) => Ok(opaque(block, "widget")),
        ContentValue::Chat(_) => Ok(opaque(block, "chat")),
        ContentValue::Icon(_) => Ok(opaque(block, "icon")),
    }
}

fn convert_link(
    block: &model::Block,
    link: &model::block::content::Link,
) -> std::result::Result<BlockContent, Violation> {
    use model::block::content::link::{CardStyle, Description, IconSize};
    let card_style = match CardStyle::try_from(link.card_style) {
        Ok(CardStyle::Text) => LinkCardStyle::Text,
        Ok(CardStyle::Card) => LinkCardStyle::Card,
        Ok(CardStyle::Inline) => LinkCardStyle::Inline,
        Err(_) => return Ok(opaque(block, "link_unknown_appearance")),
    };
    let icon_size = match IconSize::try_from(link.icon_size) {
        Ok(IconSize::SizeNone) => LinkIconSize::None,
        Ok(IconSize::SizeSmall) => LinkIconSize::Small,
        Ok(IconSize::SizeMedium) => LinkIconSize::Medium,
        Err(_) => return Ok(opaque(block, "link_unknown_appearance")),
    };
    let description = match Description::try_from(link.description) {
        Ok(Description::None) => LinkDescriptionMode::None,
        Ok(Description::Added) => LinkDescriptionMode::Added,
        Ok(Description::Content) => LinkDescriptionMode::Content,
        Err(_) => return Ok(opaque(block, "link_unknown_appearance")),
    };
    if link.relations.len() > MAX_LINK_RELATIONS {
        return Err(Violation::new(
            BodyGraphErrorKind::Oversized,
            format!("block {} has too many link relations", block.id),
        ));
    }
    if link.relations.iter().any(|key| {
        key.is_empty() || key.len() > MAX_BLOCK_ID_BYTES || key.chars().any(char::is_control)
    }) {
        return Err(Violation::new(
            BodyGraphErrorKind::MalformedBlock,
            format!("block {} has a malformed link relation", block.id),
        ));
    }
    Ok(BlockContent::Link(LinkCard {
        target_object_id: link.target_block_id.clone(),
        card_style,
        icon_size,
        description,
        relations: link.relations.clone(),
    }))
}

fn convert_file(block: &model::Block, file: &model::block::content::File) -> BlockContent {
    use model::block::content::file::{State, Style, Type};
    let kind = match Type::try_from(file.r#type) {
        Ok(Type::None) => FileBlockKind::None,
        Ok(Type::File) => FileBlockKind::File,
        Ok(Type::Image) => FileBlockKind::Image,
        Ok(Type::Video) => FileBlockKind::Video,
        Ok(Type::Audio) => FileBlockKind::Audio,
        Ok(Type::Pdf) => FileBlockKind::Pdf,
        Err(_) => return opaque(block, "file_unknown_type"),
    };
    let state = match State::try_from(file.state) {
        Ok(State::Empty) => FileBlockState::Empty,
        Ok(State::Uploading) => FileBlockState::Uploading,
        Ok(State::Done) => FileBlockState::Done,
        Ok(State::Error) => FileBlockState::Error,
        Err(_) => return opaque(block, "file_unknown_state"),
    };
    let style = match Style::try_from(file.style) {
        Ok(Style::Auto) => FileBlockStyle::Auto,
        Ok(Style::Link) => FileBlockStyle::Link,
        Ok(Style::Embed) => FileBlockStyle::Embed,
        Err(_) => return opaque(block, "file_unknown_style"),
    };
    BlockContent::File(FileView {
        target_object_id: file.target_object_id.clone(),
        kind,
        mime: file.mime.clone(),
        size: file.size,
        state,
        style,
    })
}

fn convert_embed(
    block: &model::Block,
    latex: &model::block::content::Latex,
    limits: &BodyLimits,
) -> std::result::Result<BlockContent, Violation> {
    use model::block::content::latex::Processor;
    let processor = match Processor::try_from(latex.processor) {
        Ok(Processor::Latex) => EmbedProcessor::Latex,
        Ok(Processor::Mermaid) => EmbedProcessor::Mermaid,
        Ok(Processor::Youtube) => EmbedProcessor::Youtube,
        Ok(_) => return Ok(opaque(block, "latex_unsupported_processor")),
        Err(_) => return Ok(opaque(block, "latex_unknown_processor")),
    };
    if latex.text.len() > limits.max_embed_text_bytes {
        return Err(Violation::new(
            BodyGraphErrorKind::Oversized,
            format!(
                "block {} embed text exceeds {} bytes",
                block.id, limits.max_embed_text_bytes
            ),
        ));
    }
    Ok(BlockContent::Embed(EmbedContent {
        processor,
        text: latex.text.clone(),
    }))
}

fn convert_text(
    block: &model::Block,
    text: &model::block::content::Text,
    limits: &BodyLimits,
) -> std::result::Result<BlockContent, Violation> {
    let Some(style) = TextStyle::from_proto(text.style) else {
        return Ok(opaque(block, "text_unknown_style"));
    };

    let raw_marks: &[model::block::content::text::Mark] = text
        .marks
        .as_ref()
        .map(|marks| marks.marks.as_slice())
        .unwrap_or_default();
    // An unknown mark type is mutation-critical: fail closed to opaque before
    // validating any other detail of the block.
    let mut kinds = Vec::with_capacity(raw_marks.len());
    for mark in raw_marks {
        match convert_mark_kind(mark, &block.id)? {
            Some(kind) => kinds.push(kind),
            None => return Ok(opaque(block, "text_unknown_mark")),
        }
    }

    if text.text.len() > limits.max_text_bytes {
        return Err(Violation::new(
            BodyGraphErrorKind::Oversized,
            format!(
                "block {} text exceeds {} bytes",
                block.id, limits.max_text_bytes
            ),
        ));
    }
    if raw_marks.len() > limits.max_marks_per_text {
        return Err(Violation::new(
            BodyGraphErrorKind::Oversized,
            format!(
                "block {} has {} marks, limit {}",
                block.id,
                raw_marks.len(),
                limits.max_marks_per_text
            ),
        ));
    }

    let text_utf16_len = utf16_len(&text.text);
    let mut marks = Vec::with_capacity(raw_marks.len());
    for (mark, kind) in raw_marks.iter().zip(kinds) {
        let range =
            validate_mark_range(mark.range.as_ref(), &text.text, text_utf16_len, &block.id)?;
        marks.push(TextMark { range, kind });
    }

    let color = convert_color(&text.color, &block.id, "text color")?;
    let icon = if !text.icon_image.is_empty() {
        Some(CalloutIcon::Image(text.icon_image.clone()))
    } else if !text.icon_emoji.is_empty() {
        validate_emoji_value(&text.icon_emoji).map_err(|()| {
            Violation::new(
                BodyGraphErrorKind::MalformedBlock,
                format!("block {} has a malformed callout emoji", block.id),
            )
        })?;
        Some(CalloutIcon::Emoji(text.icon_emoji.clone()))
    } else {
        None
    };

    Ok(BlockContent::Text(TextContent {
        text: text.text.clone(),
        style,
        checked: text.checked,
        color,
        icon,
        marks,
    }))
}

/// Converts a mark's type and parameter. Returns `Ok(None)` for an unknown
/// mark type (the caller reads the whole block as opaque) and a violation for
/// a malformed parameter on a known type.
fn convert_mark_kind(
    mark: &model::block::content::text::Mark,
    block_id: &str,
) -> std::result::Result<Option<MarkKind>, Violation> {
    use model::block::content::text::mark::Type;
    let malformed_param = || {
        Violation::new(
            BodyGraphErrorKind::MalformedBlock,
            format!("block {block_id} has a mark with a malformed parameter"),
        )
    };
    let nonempty_param = || {
        if mark.param.is_empty() {
            Err(malformed_param())
        } else {
            Ok(mark.param.clone())
        }
    };
    let Ok(mark_type) = Type::try_from(mark.r#type) else {
        return Ok(None);
    };
    let kind = match mark_type {
        Type::Bold => MarkKind::Bold,
        Type::Italic => MarkKind::Italic,
        Type::Strikethrough => MarkKind::Strikethrough,
        Type::Underscored => MarkKind::Underline,
        Type::Keyboard => MarkKind::Code,
        Type::Link => MarkKind::Link {
            url: nonempty_param()?,
        },
        Type::TextColor => MarkKind::TextColor {
            color: ColorToken::try_from(mark.param.clone()).map_err(|_| malformed_param())?,
        },
        Type::BackgroundColor => MarkKind::BackgroundColor {
            color: ColorToken::try_from(mark.param.clone()).map_err(|_| malformed_param())?,
        },
        Type::Mention => MarkKind::Mention {
            object_id: nonempty_param()?,
        },
        Type::Emoji => {
            validate_emoji_value(&mark.param).map_err(|()| malformed_param())?;
            MarkKind::Emoji {
                emoji: mark.param.clone(),
            }
        }
        Type::Object => MarkKind::Object {
            object_id: nonempty_param()?,
        },
    };
    Ok(Some(kind))
}

fn validate_mark_range(
    range: Option<&model::Range>,
    text: &str,
    text_utf16_len: u32,
    block_id: &str,
) -> std::result::Result<TextRange, Violation> {
    let invalid = |detail: &str| {
        Violation::new(
            BodyGraphErrorKind::MalformedBlock,
            format!("block {block_id} has a mark with {detail}"),
        )
    };
    let range = range.ok_or_else(|| invalid("a missing range"))?;
    let start = u32::try_from(range.from).map_err(|_| invalid("a negative range start"))?;
    let end = u32::try_from(range.to).map_err(|_| invalid("a negative range end"))?;
    if start > end {
        return Err(invalid("an inverted range"));
    }
    if start > text_utf16_len || end > text_utf16_len {
        return Err(invalid("a range past the end of the text"));
    }
    let range = TextRange { start, end };
    if range.to_byte_range(text).is_none() {
        return Err(invalid("an endpoint that splits a Unicode scalar"));
    }
    Ok(range)
}

fn validate_emoji_value(value: &str) -> std::result::Result<(), ()> {
    if value.is_empty() || value.len() > 64 || value.chars().any(char::is_control) {
        return Err(());
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use model::block::{ContentValue, content};

    const SPACE: &str = "space-1";
    const OBJECT: &str = "obj-1";

    fn base_block(id: &str, children: &[&str], value: Option<ContentValue>) -> model::Block {
        model::Block {
            id: id.to_string(),
            fields: None,
            restrictions: None,
            children_ids: children.iter().map(|child| (*child).to_string()).collect(),
            background_color: String::new(),
            align: 0,
            vertical_align: 0,
            content_value: value,
        }
    }

    fn smart_block(id: &str, children: &[&str]) -> model::Block {
        base_block(
            id,
            children,
            Some(ContentValue::Smartblock(content::Smartblock {})),
        )
    }

    fn text_value(text: &str, style: i32) -> ContentValue {
        ContentValue::Text(content::Text {
            text: text.to_string(),
            style,
            marks: None,
            checked: false,
            color: String::new(),
            icon_emoji: String::new(),
            icon_image: String::new(),
        })
    }

    fn text_block(id: &str, children: &[&str], text: &str, style: i32) -> model::Block {
        base_block(id, children, Some(text_value(text, style)))
    }

    fn mark(from: i32, to: i32, mark_type: i32, param: &str) -> content::text::Mark {
        content::text::Mark {
            range: Some(model::Range { from, to }),
            r#type: mark_type,
            param: param.to_string(),
        }
    }

    fn text_block_with_marks(
        id: &str,
        text: &str,
        marks: Vec<content::text::Mark>,
    ) -> model::Block {
        let mut block = text_block(id, &[], text, 0);
        if let Some(ContentValue::Text(content)) = &mut block.content_value {
            content.marks = Some(content::text::Marks { marks });
        }
        block
    }

    fn view(root: &str, blocks: Vec<model::Block>) -> model::ObjectView {
        model::ObjectView {
            root_id: root.to_string(),
            blocks,
            ..Default::default()
        }
    }

    fn snap(view: &model::ObjectView) -> Result<BodySnapshot> {
        snapshot_from_view(SPACE, OBJECT, view, &BodyLimits::default())
    }

    fn snap_with(view: &model::ObjectView, limits: BodyLimits) -> Result<BodySnapshot> {
        snapshot_from_view(SPACE, OBJECT, view, &limits.clamped())
    }

    fn graph_kind(result: Result<BodySnapshot>) -> BodyGraphErrorKind {
        match result {
            Err(AnytypeError::BodyGraph {
                object_id, kind, ..
            }) => {
                assert_eq!(object_id, OBJECT);
                kind
            }
            Err(other) => panic!("expected BodyGraph error, got {other:?}"),
            Ok(_) => panic!("expected BodyGraph error, got a snapshot"),
        }
    }

    fn id(raw: &str) -> BlockId {
        BlockId::try_from(raw.to_string()).expect("valid block id")
    }

    // ------------------------------------------------------------------
    // Snapshot shape, order, and accessors
    // ------------------------------------------------------------------

    #[test]
    fn minimal_snapshot_preserves_ids_order_and_accessors() {
        // Server child order [b, a] must be preserved exactly.
        let view = view(
            "root",
            vec![
                smart_block("root", &["b", "a"]),
                text_block("a", &["c"], "alpha", 0),
                text_block("b", &[], "beta", 0),
                text_block("c", &[], "gamma", 0),
            ],
        );
        let snapshot = snap(&view).expect("valid snapshot");

        assert_eq!(snapshot.space_id, SPACE);
        assert_eq!(snapshot.object_id, OBJECT);
        assert_eq!(snapshot.root_id, id("root"));
        assert_eq!(snapshot.len(), 4);
        assert!(!snapshot.is_empty());
        assert_eq!(snapshot.root().id, id("root"));

        // Depth-first document order with sibling order kept verbatim.
        let order: Vec<&str> = snapshot.iter().map(|block| block.id.as_str()).collect();
        assert_eq!(order, vec!["root", "b", "a", "c"]);

        assert_eq!(snapshot.children(&id("root")), &[id("b"), id("a")]);
        assert_eq!(snapshot.children(&id("a")), &[id("c")]);
        assert!(snapshot.children(&id("missing")).is_empty());
        assert!(snapshot.get(&id("missing")).is_none());

        let text = snapshot.get(&id("b")).expect("block b");
        let BlockContent::Text(content) = &text.content else {
            panic!("expected text content");
        };
        assert_eq!(content.text, "beta");
        assert_eq!(content.style, TextStyle::Paragraph);

        let reference = snapshot.block_ref(&id("c")).expect("block ref");
        assert_eq!(reference.space_id, SPACE);
        assert_eq!(reference.object_id, OBJECT);
        assert_eq!(reference.block_id, id("c"));
        assert!(snapshot.block_ref(&id("missing")).is_none());
    }

    #[test]
    fn structural_fields_read_verbatim() {
        let mut block = text_block("a", &[], "styled", 0);
        block.align = model::block::Align::Center as i32;
        block.vertical_align = model::block::VerticalAlign::Bottom as i32;
        block.background_color = "teal".to_string();
        block.restrictions = Some(model::block::Restrictions {
            read: true,
            edit: false,
            remove: true,
            drag: false,
            drop_on: true,
        });
        let view = view("root", vec![smart_block("root", &["a"]), block]);
        let snapshot = snap(&view).expect("valid snapshot");
        let block = snapshot.get(&id("a")).expect("block a");
        assert_eq!(block.align, HorizontalAlign::Center);
        assert_eq!(block.vertical_align, VerticalAlign::Bottom);
        assert_eq!(
            block.background_color.as_ref().map(ColorToken::as_str),
            Some("teal")
        );
        assert_eq!(
            block.restrictions,
            BlockRestrictions {
                read: true,
                edit: false,
                remove: true,
                drag: false,
                drop_on: true,
            }
        );
        // Defaults on the root: left/top, no background, no restrictions.
        let root = snapshot.root();
        assert_eq!(root.align, HorizontalAlign::Left);
        assert_eq!(root.vertical_align, VerticalAlign::Top);
        assert!(root.background_color.is_none());
        assert_eq!(root.restrictions, BlockRestrictions::default());
    }

    // ------------------------------------------------------------------
    // Text styles, marks, icons, colors
    // ------------------------------------------------------------------

    #[test]
    fn every_supported_text_style_maps() {
        use content::text::Style;
        let cases = [
            (Style::Paragraph, TextStyle::Paragraph),
            (Style::Header1, TextStyle::Header1),
            (Style::Header2, TextStyle::Header2),
            (Style::Header3, TextStyle::Header3),
            (Style::Header4, TextStyle::Header4),
            (Style::Quote, TextStyle::Quote),
            (Style::Code, TextStyle::Code),
            (Style::Title, TextStyle::Title),
            (Style::Checkbox, TextStyle::Checkbox),
            (Style::Marked, TextStyle::Bulleted),
            (Style::Numbered, TextStyle::Numbered),
            (Style::Toggle, TextStyle::Toggle),
            (Style::Description, TextStyle::Description),
            (Style::Callout, TextStyle::Callout),
            (Style::ToggleHeader1, TextStyle::ToggleHeader1),
            (Style::ToggleHeader2, TextStyle::ToggleHeader2),
            (Style::ToggleHeader3, TextStyle::ToggleHeader3),
        ];
        let children: Vec<String> = (0..cases.len()).map(|n| format!("t{n}")).collect();
        let child_refs: Vec<&str> = children.iter().map(String::as_str).collect();
        let mut blocks = vec![smart_block("root", &child_refs)];
        for (position, (proto_style, _)) in cases.iter().enumerate() {
            blocks.push(text_block(
                &children[position],
                &[],
                "text",
                *proto_style as i32,
            ));
        }
        let snapshot = snap(&view("root", blocks)).expect("valid snapshot");
        for (position, (_, expected)) in cases.iter().enumerate() {
            let block = snapshot.get(&id(&children[position])).expect("text block");
            let BlockContent::Text(content) = &block.content else {
                panic!("expected text content");
            };
            assert_eq!(content.style, *expected);
        }
    }

    #[test]
    fn unknown_text_style_reads_opaque() {
        let view = view(
            "root",
            vec![smart_block("root", &["a"]), text_block("a", &[], "x", 999)],
        );
        let snapshot = snap(&view).expect("valid snapshot");
        let BlockContent::Unsupported(opaque) = &snapshot.get(&id("a")).unwrap().content else {
            panic!("expected unsupported content");
        };
        assert_eq!(opaque.kind, "text_unknown_style");
    }

    #[test]
    fn checkbox_checked_state_reads_verbatim() {
        let mut block = text_block("a", &[], "done", content::text::Style::Checkbox as i32);
        if let Some(ContentValue::Text(text)) = &mut block.content_value {
            text.checked = true;
        }
        let view = view("root", vec![smart_block("root", &["a"]), block]);
        let snapshot = snap(&view).expect("valid snapshot");
        let BlockContent::Text(content) = &snapshot.get(&id("a")).unwrap().content else {
            panic!("expected text content");
        };
        assert_eq!(content.style, TextStyle::Checkbox);
        assert!(content.checked);
    }

    #[test]
    fn every_supported_mark_kind_maps() {
        use content::text::mark::Type;
        let marks = vec![
            mark(0, 1, Type::Bold as i32, ""),
            mark(1, 2, Type::Italic as i32, ""),
            mark(2, 3, Type::Strikethrough as i32, ""),
            mark(3, 4, Type::Underscored as i32, ""),
            mark(4, 5, Type::Keyboard as i32, ""),
            mark(5, 6, Type::Link as i32, "https://example.com/page"),
            mark(6, 7, Type::TextColor as i32, "red"),
            mark(7, 8, Type::BackgroundColor as i32, "yellow"),
            mark(8, 9, Type::Mention as i32, "target-object"),
            mark(9, 10, Type::Emoji as i32, "🎯"),
            mark(0, 10, Type::Object as i32, "linked-object"),
        ];
        let view = view(
            "root",
            vec![
                smart_block("root", &["a"]),
                text_block_with_marks("a", "0123456789", marks),
            ],
        );
        let snapshot = snap(&view).expect("valid snapshot");
        let BlockContent::Text(content) = &snapshot.get(&id("a")).unwrap().content else {
            panic!("expected text content");
        };
        let kinds: Vec<&MarkKind> = content.marks.iter().map(|mark| &mark.kind).collect();
        assert_eq!(content.marks.len(), 11);
        assert_eq!(kinds[0], &MarkKind::Bold);
        assert_eq!(kinds[1], &MarkKind::Italic);
        assert_eq!(kinds[2], &MarkKind::Strikethrough);
        assert_eq!(kinds[3], &MarkKind::Underline);
        assert_eq!(kinds[4], &MarkKind::Code);
        assert_eq!(
            kinds[5],
            &MarkKind::Link {
                url: "https://example.com/page".to_string()
            }
        );
        assert_eq!(
            kinds[6],
            &MarkKind::TextColor {
                color: ColorToken::new("red").unwrap()
            }
        );
        assert_eq!(
            kinds[7],
            &MarkKind::BackgroundColor {
                color: ColorToken::new("yellow").unwrap()
            }
        );
        assert_eq!(
            kinds[8],
            &MarkKind::Mention {
                object_id: "target-object".to_string()
            }
        );
        assert_eq!(
            kinds[9],
            &MarkKind::Emoji {
                emoji: "🎯".to_string()
            }
        );
        assert_eq!(
            kinds[10],
            &MarkKind::Object {
                object_id: "linked-object".to_string()
            }
        );
        assert_eq!(content.marks[0].range, TextRange { start: 0, end: 1 });
        assert_eq!(content.marks[10].range, TextRange { start: 0, end: 10 });
    }

    #[test]
    fn unknown_mark_type_reads_whole_block_opaque() {
        let view = view(
            "root",
            vec![
                smart_block("root", &["a"]),
                text_block_with_marks("a", "text", vec![mark(0, 1, 999, "")]),
            ],
        );
        let snapshot = snap(&view).expect("valid snapshot");
        let block = snapshot.get(&id("a")).expect("block a");
        let BlockContent::Unsupported(opaque) = &block.content else {
            panic!("expected unsupported content");
        };
        assert_eq!(opaque.kind, "text_unknown_mark");
        // Identity and structure stay intact for the opaque block.
        assert_eq!(block.id, id("a"));
    }

    #[test]
    fn malformed_mark_details_fail_the_read() {
        use content::text::mark::Type;
        let cases = vec![
            // Inverted range.
            mark(3, 1, Type::Bold as i32, ""),
            // Negative start.
            mark(-1, 1, Type::Bold as i32, ""),
            // Negative end.
            mark(0, -1, Type::Bold as i32, ""),
            // Past the end of the text (utf16 len of "text" is 4).
            mark(0, 5, Type::Bold as i32, ""),
            // Empty link URL.
            mark(0, 1, Type::Link as i32, ""),
            // Malformed color token.
            mark(0, 1, Type::TextColor as i32, "RED"),
            // Empty mention target.
            mark(0, 1, Type::Mention as i32, ""),
        ];
        for bad in cases {
            let view = view(
                "root",
                vec![
                    smart_block("root", &["a"]),
                    text_block_with_marks("a", "text", vec![bad]),
                ],
            );
            assert_eq!(graph_kind(snap(&view)), BodyGraphErrorKind::MalformedBlock);
        }
        // A missing range message is also malformed.
        let missing_range = content::text::Mark {
            range: None,
            r#type: Type::Bold as i32,
            param: String::new(),
        };
        let view = view(
            "root",
            vec![
                smart_block("root", &["a"]),
                text_block_with_marks("a", "text", vec![missing_range]),
            ],
        );
        assert_eq!(graph_kind(snap(&view)), BodyGraphErrorKind::MalformedBlock);
    }

    #[test]
    fn mark_ranges_use_utf16_code_units() {
        use content::text::mark::Type;
        // "a𐍈b" is 3 chars, 6 bytes, and 4 UTF-16 code units.
        let text = "a\u{10348}b";
        assert_eq!(utf16_len(text), 4);
        let ok = view(
            "root",
            vec![
                smart_block("root", &["a"]),
                text_block_with_marks("a", text, vec![mark(0, 4, Type::Bold as i32, "")]),
            ],
        );
        assert!(snap(&ok).is_ok());
        let bad = view(
            "root",
            vec![
                smart_block("root", &["a"]),
                text_block_with_marks("a", text, vec![mark(0, 5, Type::Bold as i32, "")]),
            ],
        );
        assert_eq!(graph_kind(snap(&bad)), BodyGraphErrorKind::MalformedBlock);

        for (start, end) in [(2, 3), (1, 2)] {
            let split = view(
                "root",
                vec![
                    smart_block("root", &["a"]),
                    text_block_with_marks("a", text, vec![mark(start, end, Type::Bold as i32, "")]),
                ],
            );
            assert_eq!(graph_kind(snap(&split)), BodyGraphErrorKind::MalformedBlock);
        }

        for (start, end) in [(0, 0), (4, 4), (1, 3)] {
            let boundary = view(
                "root",
                vec![
                    smart_block("root", &["a"]),
                    text_block_with_marks("a", text, vec![mark(start, end, Type::Bold as i32, "")]),
                ],
            );
            assert!(snap(&boundary).is_ok());
        }
    }

    #[test]
    fn read_emoji_values_enforce_exact_utf8_byte_bounds() {
        use content::text::mark::Type;
        let accepted = ["x".to_owned(), "🙂".repeat(16)];
        for emoji in accepted {
            let marked = view(
                "root",
                vec![
                    smart_block("root", &["a"]),
                    text_block_with_marks("a", "x", vec![mark(0, 1, Type::Emoji as i32, &emoji)]),
                ],
            );
            assert!(snap(&marked).is_ok());

            let mut callout = text_block("a", &[], "x", content::text::Style::Callout as i32);
            if let Some(ContentValue::Text(text)) = &mut callout.content_value {
                text.icon_emoji = emoji;
            }
            assert!(snap(&view("root", vec![smart_block("root", &["a"]), callout])).is_ok());
        }

        for emoji in [String::new(), "\n".to_owned(), "a".repeat(65)] {
            let marked = view(
                "root",
                vec![
                    smart_block("root", &["a"]),
                    text_block_with_marks("a", "x", vec![mark(0, 1, Type::Emoji as i32, &emoji)]),
                ],
            );
            assert_eq!(
                graph_kind(snap(&marked)),
                BodyGraphErrorKind::MalformedBlock
            );
        }

        for emoji in ["\n".to_owned(), "a".repeat(65)] {
            let mut callout = text_block("a", &[], "x", content::text::Style::Callout as i32);
            if let Some(ContentValue::Text(text)) = &mut callout.content_value {
                text.icon_emoji = emoji;
            }
            assert_eq!(
                graph_kind(snap(&view(
                    "root",
                    vec![smart_block("root", &["a"]), callout]
                ))),
                BodyGraphErrorKind::MalformedBlock
            );
        }
    }

    #[test]
    fn text_range_json_rejects_negative_and_u32_overflow() {
        assert!(serde_json::from_str::<TextRange>(r#"{"start":-1,"end":0}"#).is_err());
        assert!(
            serde_json::from_str::<TextRange>(r#"{"start":4294967296,"end":4294967296}"#).is_err()
        );
        assert!(serde_json::from_str::<TextRange>(r#"{"start":0,"end":4294967296}"#).is_err());
    }

    #[test]
    fn text_range_byte_conversion_helpers_round_trip() {
        let text = "a\u{10348}b";
        let range = TextRange::from_byte_range(text, 1..5).expect("surrogate pair range");
        assert_eq!(range, TextRange { start: 1, end: 3 });
        assert_eq!(range.to_byte_range(text), Some(1..5));
        assert_eq!(
            TextRange::from_byte_range(text, 0..text.len()),
            Some(TextRange { start: 0, end: 4 })
        );
        // Byte offset 2 is inside the 4-byte scalar: not a char boundary.
        assert!(TextRange::from_byte_range(text, 2..5).is_none());
        // UTF-16 offset 2 splits the surrogate pair.
        assert!(TextRange { start: 2, end: 3 }.to_byte_range(text).is_none());
        // Inverted and out-of-bounds inputs are rejected.
        assert!(TextRange { start: 3, end: 1 }.to_byte_range(text).is_none());
        assert!(TextRange { start: 0, end: 9 }.to_byte_range(text).is_none());
        #[allow(clippy::reversed_empty_ranges)]
        {
            assert!(TextRange::from_byte_range(text, 5..1).is_none());
        }
    }

    #[test]
    fn callout_icon_prefers_image_over_emoji() {
        let build = |emoji: &str, image: &str| {
            let mut block = text_block("a", &[], "note", content::text::Style::Callout as i32);
            if let Some(ContentValue::Text(text)) = &mut block.content_value {
                text.icon_emoji = emoji.to_string();
                text.icon_image = image.to_string();
            }
            view("root", vec![smart_block("root", &["a"]), block])
        };
        let icon_of = |view: &model::ObjectView| {
            let snapshot = snap(view).expect("valid snapshot");
            let BlockContent::Text(content) = &snapshot.get(&id("a")).unwrap().content else {
                panic!("expected text content");
            };
            content.icon.clone()
        };
        assert_eq!(icon_of(&build("", "")), None);
        assert_eq!(
            icon_of(&build("🎉", "")),
            Some(CalloutIcon::Emoji("🎉".to_string()))
        );
        assert_eq!(
            icon_of(&build("🎉", "image-object")),
            Some(CalloutIcon::Image("image-object".to_string()))
        );
    }

    #[test]
    fn text_color_and_background_color_validate() {
        let colored = |color: &str, background: &str| {
            let mut block = text_block("a", &[], "tinted", 0);
            if let Some(ContentValue::Text(text)) = &mut block.content_value {
                text.color = color.to_string();
            }
            block.background_color = background.to_string();
            view("root", vec![smart_block("root", &["a"]), block])
        };
        let snapshot = snap(&colored("blue", "lime")).expect("valid snapshot");
        let block = snapshot.get(&id("a")).unwrap();
        let BlockContent::Text(content) = &block.content else {
            panic!("expected text content");
        };
        assert_eq!(content.color.as_ref().map(ColorToken::as_str), Some("blue"));
        assert_eq!(
            block.background_color.as_ref().map(ColorToken::as_str),
            Some("lime")
        );
        // Empty means absent; malformed fails the read.
        let snapshot = snap(&colored("", "")).expect("valid snapshot");
        let block = snapshot.get(&id("a")).unwrap();
        assert!(block.background_color.is_none());
        assert_eq!(
            graph_kind(snap(&colored("Bad Color", ""))),
            BodyGraphErrorKind::MalformedBlock
        );
        assert_eq!(
            graph_kind(snap(&colored("", "RED"))),
            BodyGraphErrorKind::MalformedBlock
        );
    }

    // ------------------------------------------------------------------
    // Non-text variants
    // ------------------------------------------------------------------

    #[test]
    fn layout_and_divider_styles_map_and_unknowns_read_opaque() {
        use content::{div, layout};
        let layout_cases = [
            (layout::Style::Row, LayoutStyle::Row),
            (layout::Style::Column, LayoutStyle::Column),
            (layout::Style::Div, LayoutStyle::Div),
            (layout::Style::Header, LayoutStyle::Header),
            (layout::Style::TableRows, LayoutStyle::TableRows),
            (layout::Style::TableColumns, LayoutStyle::TableColumns),
        ];
        for (proto_style, expected) in layout_cases {
            let block = base_block(
                "a",
                &[],
                Some(ContentValue::Layout(content::Layout {
                    style: proto_style as i32,
                })),
            );
            let snapshot = snap(&view("root", vec![smart_block("root", &["a"]), block]))
                .expect("valid snapshot");
            assert_eq!(
                snapshot.get(&id("a")).unwrap().content,
                BlockContent::Layout(expected)
            );
        }
        for (proto_style, expected) in [
            (div::Style::Line, DividerStyle::Line),
            (div::Style::Dots, DividerStyle::Dots),
        ] {
            let block = base_block(
                "a",
                &[],
                Some(ContentValue::Div(content::Div {
                    style: proto_style as i32,
                })),
            );
            let snapshot = snap(&view("root", vec![smart_block("root", &["a"]), block]))
                .expect("valid snapshot");
            assert_eq!(
                snapshot.get(&id("a")).unwrap().content,
                BlockContent::Divider(expected)
            );
        }
        for (value, expected_kind) in [
            (
                ContentValue::Layout(content::Layout { style: 999 }),
                "layout_unknown_style",
            ),
            (
                ContentValue::Div(content::Div { style: 999 }),
                "div_unknown_style",
            ),
        ] {
            let block = base_block("a", &[], Some(value));
            let snapshot = snap(&view("root", vec![smart_block("root", &["a"]), block]))
                .expect("valid snapshot");
            let BlockContent::Unsupported(opaque) = &snapshot.get(&id("a")).unwrap().content else {
                panic!("expected unsupported content");
            };
            assert_eq!(opaque.kind, expected_kind);
        }
    }

    #[test]
    fn bookmark_link_relation_and_system_blocks_map() {
        use content::bookmark::State;
        use content::link::{CardStyle, Description, IconSize};
        let bookmark = base_block(
            "bm",
            &[],
            Some(ContentValue::Bookmark(content::Bookmark {
                url: "https://example.com".to_string(),
                target_object_id: "bookmark-object".to_string(),
                state: State::Done as i32,
                ..Default::default()
            })),
        );
        let link = base_block(
            "ln",
            &[],
            Some(ContentValue::Link(content::Link {
                target_block_id: "linked-object".to_string(),
                card_style: CardStyle::Card as i32,
                icon_size: IconSize::SizeMedium as i32,
                description: Description::Content as i32,
                ..Default::default()
            })),
        );
        let relation = base_block(
            "rel",
            &[],
            Some(ContentValue::Relation(content::Relation {
                key: "assignee".to_string(),
            })),
        );
        let featured = base_block(
            "feat",
            &[],
            Some(ContentValue::FeaturedRelations(
                content::FeaturedRelations {},
            )),
        );
        let toc = base_block(
            "toc",
            &[],
            Some(ContentValue::TableOfContents(content::TableOfContents {})),
        );
        let view = view(
            "root",
            vec![
                smart_block("root", &["bm", "ln", "rel", "feat", "toc"]),
                bookmark,
                link,
                relation,
                featured,
                toc,
            ],
        );
        let snapshot = snap(&view).expect("valid snapshot");
        assert_eq!(
            snapshot.get(&id("bm")).unwrap().content,
            BlockContent::Bookmark(BookmarkContent {
                url: "https://example.com".to_string(),
                target_object_id: Some("bookmark-object".to_string()),
                state: BookmarkState::Done,
            })
        );
        assert_eq!(
            snapshot.get(&id("ln")).unwrap().content,
            BlockContent::Link(LinkCard {
                target_object_id: "linked-object".to_string(),
                card_style: LinkCardStyle::Card,
                icon_size: LinkIconSize::Medium,
                description: LinkDescriptionMode::Content,
                relations: Vec::new(),
            })
        );
        assert_eq!(
            snapshot.get(&id("rel")).unwrap().content,
            BlockContent::Relation(RelationView {
                key: "assignee".to_string(),
            })
        );
        assert_eq!(
            snapshot.get(&id("feat")).unwrap().content,
            BlockContent::FeaturedRelations
        );
        assert_eq!(
            snapshot.get(&id("toc")).unwrap().content,
            BlockContent::TableOfContents
        );
    }

    #[test]
    fn bookmark_and_link_unknown_enum_values_read_opaque() {
        let bookmark = base_block(
            "a",
            &[],
            Some(ContentValue::Bookmark(content::Bookmark {
                url: "https://example.com".to_string(),
                state: 999,
                ..Default::default()
            })),
        );
        let snapshot = snap(&view("root", vec![smart_block("root", &["a"]), bookmark]))
            .expect("valid snapshot");
        let BlockContent::Unsupported(opaque) = &snapshot.get(&id("a")).unwrap().content else {
            panic!("expected unsupported content");
        };
        assert_eq!(opaque.kind, "bookmark_unknown_state");

        let link = base_block(
            "a",
            &[],
            Some(ContentValue::Link(content::Link {
                target_block_id: "linked-object".to_string(),
                card_style: 999,
                ..Default::default()
            })),
        );
        let snapshot =
            snap(&view("root", vec![smart_block("root", &["a"]), link])).expect("valid snapshot");
        let BlockContent::Unsupported(opaque) = &snapshot.get(&id("a")).unwrap().content else {
            panic!("expected unsupported content");
        };
        assert_eq!(opaque.kind, "link_unknown_appearance");

        // A bookmark with an empty target object id reads as None.
        let empty_target = base_block(
            "a",
            &[],
            Some(ContentValue::Bookmark(content::Bookmark {
                url: "https://example.com".to_string(),
                state: 0,
                ..Default::default()
            })),
        );
        let snapshot = snap(&view(
            "root",
            vec![smart_block("root", &["a"]), empty_target],
        ))
        .expect("valid snapshot");
        let BlockContent::Bookmark(content) = &snapshot.get(&id("a")).unwrap().content else {
            panic!("expected bookmark content");
        };
        assert_eq!(content.target_object_id, None);
        assert_eq!(content.state, BookmarkState::Empty);
    }

    #[test]
    fn embed_processors_map_and_unreviewed_processors_read_opaque() {
        use content::latex::Processor;
        let embed = |processor: i32| {
            let block = base_block(
                "a",
                &[],
                Some(ContentValue::Latex(content::Latex {
                    text: "E = mc^2".to_string(),
                    processor,
                })),
            );
            view("root", vec![smart_block("root", &["a"]), block])
        };
        for (raw, expected) in [
            (Processor::Latex as i32, EmbedProcessor::Latex),
            (Processor::Mermaid as i32, EmbedProcessor::Mermaid),
            (Processor::Youtube as i32, EmbedProcessor::Youtube),
        ] {
            let snapshot = snap(&embed(raw)).expect("valid snapshot");
            assert_eq!(
                snapshot.get(&id("a")).unwrap().content,
                BlockContent::Embed(EmbedContent {
                    processor: expected,
                    text: "E = mc^2".to_string(),
                })
            );
        }
        for (raw, expected_kind) in [
            (Processor::Chart as i32, "latex_unsupported_processor"),
            (Processor::Vimeo as i32, "latex_unsupported_processor"),
            (999, "latex_unknown_processor"),
        ] {
            let snapshot = snap(&embed(raw)).expect("valid snapshot");
            let BlockContent::Unsupported(opaque) = &snapshot.get(&id("a")).unwrap().content else {
                panic!("expected unsupported content");
            };
            assert_eq!(opaque.kind, expected_kind);
        }
    }

    #[test]
    fn table_family_maps() {
        let table = base_block(
            "table",
            &["cols", "rows"],
            Some(ContentValue::Table(content::Table {})),
        );
        let columns_layout = base_block(
            "cols",
            &["c1"],
            Some(ContentValue::Layout(content::Layout {
                style: content::layout::Style::TableColumns as i32,
            })),
        );
        let rows_layout = base_block(
            "rows",
            &["r1"],
            Some(ContentValue::Layout(content::Layout {
                style: content::layout::Style::TableRows as i32,
            })),
        );
        let column = base_block(
            "c1",
            &[],
            Some(ContentValue::TableColumn(content::TableColumn {})),
        );
        let row = base_block(
            "r1",
            &[],
            Some(ContentValue::TableRow(content::TableRow {
                is_header: true,
            })),
        );
        let view = view(
            "root",
            vec![
                smart_block("root", &["table"]),
                table,
                columns_layout,
                rows_layout,
                column,
                row,
            ],
        );
        let snapshot = snap(&view).expect("valid snapshot");
        assert_eq!(
            snapshot.get(&id("table")).unwrap().content,
            BlockContent::Table
        );
        assert_eq!(
            snapshot.get(&id("cols")).unwrap().content,
            BlockContent::Layout(LayoutStyle::TableColumns)
        );
        assert_eq!(
            snapshot.get(&id("r1")).unwrap().content,
            BlockContent::TableRow { is_header: true }
        );
        assert_eq!(
            snapshot.get(&id("c1")).unwrap().content,
            BlockContent::TableColumn
        );
    }

    #[test]
    fn file_blocks_map_and_unknown_enum_values_read_opaque() {
        use content::file::{State, Style, Type};
        let file = base_block(
            "a",
            &[],
            Some(ContentValue::File(content::File {
                target_object_id: "file-object".to_string(),
                r#type: Type::Image as i32,
                mime: "image/png".to_string(),
                size: 2048,
                state: State::Done as i32,
                style: Style::Embed as i32,
                ..Default::default()
            })),
        );
        let snapshot =
            snap(&view("root", vec![smart_block("root", &["a"]), file])).expect("valid snapshot");
        assert_eq!(
            snapshot.get(&id("a")).unwrap().content,
            BlockContent::File(FileView {
                target_object_id: "file-object".to_string(),
                kind: FileBlockKind::Image,
                mime: "image/png".to_string(),
                size: 2048,
                state: FileBlockState::Done,
                style: FileBlockStyle::Embed,
            })
        );
        for (value, expected_kind) in [
            (
                content::File {
                    r#type: 999,
                    ..Default::default()
                },
                "file_unknown_type",
            ),
            (
                content::File {
                    state: 999,
                    ..Default::default()
                },
                "file_unknown_state",
            ),
            (
                content::File {
                    style: 999,
                    ..Default::default()
                },
                "file_unknown_style",
            ),
        ] {
            let block = base_block("a", &[], Some(ContentValue::File(value)));
            let snapshot = snap(&view("root", vec![smart_block("root", &["a"]), block]))
                .expect("valid snapshot");
            let BlockContent::Unsupported(opaque) = &snapshot.get(&id("a")).unwrap().content else {
                panic!("expected unsupported content");
            };
            assert_eq!(opaque.kind, expected_kind);
        }
    }

    // ------------------------------------------------------------------
    // Opaque fail-closed reads
    // ------------------------------------------------------------------

    #[test]
    fn unmodeled_variants_read_opaque_with_content_free_summaries() {
        let cases: Vec<(ContentValue, &str)> = vec![
            (
                ContentValue::Smartblock(content::Smartblock {}),
                "smartblock",
            ),
            (
                ContentValue::Dataview(content::Dataview {
                    source: vec!["secret-source".to_string()],
                    target_object_id: "secret-target".to_string(),
                    ..Default::default()
                }),
                "dataview",
            ),
            (
                ContentValue::Widget(content::Widget {
                    view_id: "secret-view".to_string(),
                    ..Default::default()
                }),
                "widget",
            ),
            (ContentValue::Chat(content::Chat {}), "chat"),
            (
                ContentValue::Icon(content::Icon {
                    name: "secret-icon".to_string(),
                }),
                "icon",
            ),
        ];
        for (value, expected_kind) in cases {
            let mut block = base_block("a", &["b"], Some(value));
            block.align = model::block::Align::Center as i32;
            block.background_color = "red".to_string();
            let view = view(
                "root",
                vec![
                    smart_block("root", &["a"]),
                    block,
                    text_block("b", &[], "nested", 0),
                ],
            );
            let snapshot = snap(&view).expect("valid snapshot");
            let block = snapshot.get(&id("a")).expect("opaque block");
            let BlockContent::Unsupported(opaque) = &block.content else {
                panic!("expected unsupported content");
            };
            assert_eq!(opaque.kind, expected_kind);
            assert_eq!(opaque.summary.child_count, 1);
            assert!(opaque.summary.approx_bytes > 0);
            // Identity, order, and structure are preserved on opaque blocks.
            assert_eq!(block.children, vec![id("b")]);
            assert_eq!(block.align, HorizontalAlign::Center);
            assert_eq!(
                block.background_color.as_ref().map(ColorToken::as_str),
                Some("red")
            );
            // The serialized snapshot never leaks unmodeled content.
            let serialized = serde_json::to_string(&snapshot).expect("serialize snapshot");
            assert!(!serialized.contains("secret"));
        }
        // A missing/unrecognized oneof tag reads as "unknown".
        let block = base_block("a", &[], None);
        let snapshot =
            snap(&view("root", vec![smart_block("root", &["a"]), block])).expect("valid snapshot");
        let BlockContent::Unsupported(opaque) = &snapshot.get(&id("a")).unwrap().content else {
            panic!("expected unsupported content");
        };
        assert_eq!(opaque.kind, "unknown");
    }

    // ------------------------------------------------------------------
    // Graph validation failures
    // ------------------------------------------------------------------

    #[test]
    fn missing_root_fails() {
        let empty = view("", vec![smart_block("a", &[])]);
        assert_eq!(graph_kind(snap(&empty)), BodyGraphErrorKind::MissingRoot);
        let absent = view("nope", vec![smart_block("a", &[])]);
        assert_eq!(graph_kind(snap(&absent)), BodyGraphErrorKind::MissingRoot);
    }

    #[test]
    fn duplicate_block_ids_fail() {
        let view = view(
            "root",
            vec![
                smart_block("root", &["a"]),
                text_block("a", &[], "one", 0),
                text_block("a", &[], "two", 0),
            ],
        );
        assert_eq!(graph_kind(snap(&view)), BodyGraphErrorKind::DuplicateBlock);
    }

    #[test]
    fn dangling_child_reference_fails() {
        let view = view("root", vec![smart_block("root", &["ghost"])]);
        assert_eq!(graph_kind(snap(&view)), BodyGraphErrorKind::DanglingChild);
    }

    #[test]
    fn shared_child_and_parented_root_fail() {
        let shared = view(
            "root",
            vec![
                smart_block("root", &["a", "b"]),
                text_block("a", &["c"], "one", 0),
                text_block("b", &["c"], "two", 0),
                text_block("c", &[], "shared", 0),
            ],
        );
        assert_eq!(graph_kind(snap(&shared)), BodyGraphErrorKind::SharedChild);
        let parented_root = view(
            "root",
            vec![
                smart_block("root", &["a"]),
                text_block("a", &["root"], "loop", 0),
            ],
        );
        assert_eq!(
            graph_kind(snap(&parented_root)),
            BodyGraphErrorKind::SharedChild
        );
    }

    #[test]
    fn cyclic_graphs_fail() {
        let ring = view(
            "root",
            vec![
                smart_block("root", &[]),
                text_block("a", &["b"], "one", 0),
                text_block("b", &["a"], "two", 0),
            ],
        );
        assert_eq!(graph_kind(snap(&ring)), BodyGraphErrorKind::Cycle);
        let self_loop = view(
            "root",
            vec![smart_block("root", &[]), text_block("a", &["a"], "me", 0)],
        );
        assert_eq!(graph_kind(snap(&self_loop)), BodyGraphErrorKind::Cycle);
    }

    #[test]
    fn orphaned_blocks_fail() {
        let view = view(
            "root",
            vec![
                smart_block("root", &[]),
                text_block("stray", &[], "lost", 0),
            ],
        );
        assert_eq!(graph_kind(snap(&view)), BodyGraphErrorKind::Orphaned);
    }

    #[test]
    fn malformed_ids_and_structural_enums_fail() {
        let empty_id = view(
            "root",
            vec![smart_block("root", &[]), text_block("", &[], "x", 0)],
        );
        assert_eq!(
            graph_kind(snap(&empty_id)),
            BodyGraphErrorKind::MalformedBlock
        );

        let long_id = "x".repeat(MAX_BLOCK_ID_BYTES + 1);
        let oversized_id = view(
            "root",
            vec![smart_block("root", &[]), text_block(&long_id, &[], "x", 0)],
        );
        assert_eq!(
            graph_kind(snap(&oversized_id)),
            BodyGraphErrorKind::MalformedBlock
        );

        let mut bad_align = text_block("a", &[], "x", 0);
        bad_align.align = 999;
        let bad_align = view("root", vec![smart_block("root", &["a"]), bad_align]);
        assert_eq!(
            graph_kind(snap(&bad_align)),
            BodyGraphErrorKind::MalformedBlock
        );

        let mut bad_vertical = text_block("a", &[], "x", 0);
        bad_vertical.vertical_align = 999;
        let bad_vertical = view("root", vec![smart_block("root", &["a"]), bad_vertical]);
        assert_eq!(
            graph_kind(snap(&bad_vertical)),
            BodyGraphErrorKind::MalformedBlock
        );
    }

    #[test]
    fn oversized_reads_fail_instead_of_truncating() {
        // Block count.
        let three = view(
            "root",
            vec![
                smart_block("root", &["a", "b"]),
                text_block("a", &[], "x", 0),
                text_block("b", &[], "y", 0),
            ],
        );
        let limits = BodyLimits {
            max_blocks: 2,
            ..BodyLimits::default()
        };
        assert_eq!(
            graph_kind(snap_with(&three, limits)),
            BodyGraphErrorKind::Oversized
        );

        // Fanout.
        let limits = BodyLimits {
            max_children: 1,
            ..BodyLimits::default()
        };
        assert_eq!(
            graph_kind(snap_with(&three, limits)),
            BodyGraphErrorKind::Oversized
        );

        // Depth (root at depth 1).
        let chain = view(
            "root",
            vec![
                smart_block("root", &["a"]),
                text_block("a", &["b"], "x", 0),
                text_block("b", &[], "y", 0),
            ],
        );
        let limits = BodyLimits {
            max_depth: 2,
            ..BodyLimits::default()
        };
        assert_eq!(
            graph_kind(snap_with(&chain, limits)),
            BodyGraphErrorKind::Oversized
        );

        // Text bytes.
        let text = view(
            "root",
            vec![
                smart_block("root", &["a"]),
                text_block("a", &[], "abcdef", 0),
            ],
        );
        let limits = BodyLimits {
            max_text_bytes: 5,
            ..BodyLimits::default()
        };
        assert_eq!(
            graph_kind(snap_with(&text, limits)),
            BodyGraphErrorKind::Oversized
        );

        // Mark count.
        use content::text::mark::Type;
        let marked = view(
            "root",
            vec![
                smart_block("root", &["a"]),
                text_block_with_marks(
                    "a",
                    "text",
                    vec![
                        mark(0, 1, Type::Bold as i32, ""),
                        mark(1, 2, Type::Italic as i32, ""),
                    ],
                ),
            ],
        );
        let limits = BodyLimits {
            max_marks_per_text: 1,
            ..BodyLimits::default()
        };
        assert_eq!(
            graph_kind(snap_with(&marked, limits)),
            BodyGraphErrorKind::Oversized
        );

        // Embed source bytes.
        let embed = view(
            "root",
            vec![
                smart_block("root", &["a"]),
                base_block(
                    "a",
                    &[],
                    Some(ContentValue::Latex(content::Latex {
                        text: "123456".to_string(),
                        processor: 0,
                    })),
                ),
            ],
        );
        let limits = BodyLimits {
            max_embed_text_bytes: 5,
            ..BodyLimits::default()
        };
        assert_eq!(
            graph_kind(snap_with(&embed, limits)),
            BodyGraphErrorKind::Oversized
        );

        // Table rows and columns.
        let rows = view(
            "root",
            vec![
                smart_block("root", &["rows"]),
                base_block(
                    "rows",
                    &["r1", "r2"],
                    Some(ContentValue::Layout(content::Layout {
                        style: content::layout::Style::TableRows as i32,
                    })),
                ),
                base_block(
                    "r1",
                    &[],
                    Some(ContentValue::TableRow(content::TableRow {
                        is_header: false,
                    })),
                ),
                base_block(
                    "r2",
                    &[],
                    Some(ContentValue::TableRow(content::TableRow {
                        is_header: false,
                    })),
                ),
            ],
        );
        let limits = BodyLimits {
            max_table_rows: 1,
            ..BodyLimits::default()
        };
        assert_eq!(
            graph_kind(snap_with(&rows, limits)),
            BodyGraphErrorKind::Oversized
        );

        let columns = view(
            "root",
            vec![
                smart_block("root", &["cols"]),
                base_block(
                    "cols",
                    &["c1", "c2"],
                    Some(ContentValue::Layout(content::Layout {
                        style: content::layout::Style::TableColumns as i32,
                    })),
                ),
                base_block(
                    "c1",
                    &[],
                    Some(ContentValue::TableColumn(content::TableColumn {})),
                ),
                base_block(
                    "c2",
                    &[],
                    Some(ContentValue::TableColumn(content::TableColumn {})),
                ),
            ],
        );
        let limits = BodyLimits {
            max_table_columns: 1,
            ..BodyLimits::default()
        };
        assert_eq!(
            graph_kind(snap_with(&columns, limits)),
            BodyGraphErrorKind::Oversized
        );
    }

    // ------------------------------------------------------------------
    // Limits, identifiers, serialization
    // ------------------------------------------------------------------

    #[test]
    fn limits_clamp_to_hard_ceilings_and_default_to_them() {
        let raised = BodyLimits {
            max_blocks: usize::MAX,
            max_depth: usize::MAX,
            max_children: usize::MAX,
            max_text_bytes: usize::MAX,
            max_marks_per_text: usize::MAX,
            max_table_rows: usize::MAX,
            max_table_columns: usize::MAX,
            max_block_id_bytes: usize::MAX,
            max_embed_text_bytes: usize::MAX,
        }
        .clamped();
        assert_eq!(raised, BodyLimits::default());
        let lowered = BodyLimits {
            max_blocks: 5,
            ..BodyLimits::default()
        }
        .clamped();
        assert_eq!(lowered.max_blocks, 5);
        assert_eq!(lowered.max_depth, MAX_BODY_DEPTH);
    }

    #[test]
    fn block_id_and_color_token_validate_on_construction() {
        assert!(BlockId::try_from(String::new()).is_err());
        assert!(BlockId::try_from("x".repeat(MAX_BLOCK_ID_BYTES + 1)).is_err());
        let ok = BlockId::try_from("block-1".to_string()).expect("valid id");
        assert_eq!(ok.as_str(), "block-1");
        assert_eq!(ok.to_string(), "block-1");

        assert!(ColorToken::new("").is_err());
        assert!(ColorToken::new("RED").is_err());
        assert!(ColorToken::new("with space").is_err());
        assert!(ColorToken::new("x".repeat(MAX_COLOR_TOKEN_BYTES + 1)).is_err());
        assert!(ColorToken::new("r\u{e9}d").is_err());
        for palette in COLOR_TOKEN_PALETTE {
            assert!(ColorToken::new(*palette).is_ok());
        }
        // Unknown-but-well-formed tokens read verbatim.
        let custom = ColorToken::new("custom-tone9").expect("well-formed token");
        assert_eq!(custom.as_str(), "custom-tone9");
    }

    #[test]
    fn serialized_snapshot_shape_is_stable() {
        use content::text::mark::Type;
        let view = view(
            "root",
            vec![
                smart_block("root", &["a"]),
                text_block_with_marks("a", "hello", vec![mark(0, 5, Type::Bold as i32, "")]),
            ],
        );
        let snapshot = snap(&view).expect("valid snapshot");
        let value = serde_json::to_value(&snapshot).expect("serialize snapshot");
        assert_eq!(value["space_id"], "space-1");
        assert_eq!(value["object_id"], "obj-1");
        assert_eq!(value["root_id"], "root");
        assert_eq!(value["blocks"][0]["id"], "root");
        assert_eq!(value["blocks"][0]["content"]["type"], "unsupported");
        assert_eq!(value["blocks"][0]["align"], "left");
        assert_eq!(value["blocks"][1]["id"], "a");
        assert_eq!(value["blocks"][1]["content"]["type"], "text");
        assert_eq!(
            value["blocks"][1]["content"]["content"]["style"],
            "paragraph"
        );
        assert_eq!(value["blocks"][1]["content"]["content"]["text"], "hello");
        assert_eq!(
            value["blocks"][1]["content"]["content"]["marks"][0]["kind"]["type"],
            "bold"
        );
        // The internal index must not serialize.
        assert!(value.get("index").is_none());

        let reference = snapshot.block_ref(&id("a")).expect("block ref");
        let round_trip: BlockRef =
            serde_json::from_str(&serde_json::to_string(&reference).expect("serialize block ref"))
                .expect("deserialize block ref");
        assert_eq!(round_trip, reference);
        // Malformed IDs are rejected on deserialization.
        assert!(
            serde_json::from_str::<BlockRef>(r#"{"space_id":"s","object_id":"o","block_id":""}"#)
                .is_err()
        );
    }

    #[test]
    fn graph_error_kind_display_is_stable() {
        assert_eq!(BodyGraphErrorKind::MissingRoot.to_string(), "missing_root");
        assert_eq!(BodyGraphErrorKind::Oversized.to_string(), "oversized");
        assert_eq!(
            BodyGraphErrorKind::MalformedBlock.to_string(),
            "malformed_block"
        );
    }
}
