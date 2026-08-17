use std::fmt;
use std::fs;
use std::path::Path;
use std::{collections::HashMap, hash::RandomState};

use anyhow::{Context, Result, anyhow, bail};
use prost::Message;
use prost_types::{Struct, value::Kind};

use crate::archive::ArchiveReader;
use anytype_rpc::{
    anytype::SnapshotWithType,
    model::{
        Block, Range, SmartBlockType,
        block::{
            ContentValue,
            content::{
                Bookmark, Div, File, Latex, Link, Table, TableColumn, TableRow, Text,
                div::Style as DivStyle,
                file::{State as FileState, Type as FileType},
                layout::Style as LayoutStyle,
                text::{Mark, Style as TextStyle, mark::Type as MarkType},
            },
        },
    },
};
use serde_json::Value as JsonValue;

/// Metadata used to resolve object links and file names during markdown rendering.
#[derive(Debug, Clone)]
pub struct ArchiveObjectInfo {
    pub id: String,
    pub name: String,
    pub snippet: String,
    pub layout: Option<i64>,
    pub file_ext: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedObjectKind {
    Markdown,
    Raw,
}

/// Error returned when a file-layout snapshot has no matching archive payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingRawPayloadError {
    object_id: String,
}

impl MissingRawPayloadError {
    fn new(object_id: &str) -> Self {
        Self {
            object_id: object_id.to_string(),
        }
    }

    /// Returns the object whose payload could not be resolved.
    #[must_use]
    pub fn object_id(&self) -> &str {
        &self.object_id
    }
}

impl fmt::Display for MissingRawPayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "could not resolve raw payload for object: {}",
            self.object_id
        )
    }
}

impl std::error::Error for MissingRawPayloadError {}

#[derive(Debug, Clone, Default)]
struct RenderState {
    indent: String,
    list_opened: bool,
    list_number: usize,
}

impl RenderState {
    fn with_space_indent(&self) -> Self {
        let mut next = self.clone();
        next.indent.push_str("    ");
        next
    }

    fn with_nb_indent(&self) -> Self {
        let mut next = self.clone();
        next.indent.push_str("  ");
        next
    }
}

#[derive(Debug)]
struct MarkdownConverter<'a> {
    blocks_by_id: HashMap<String, &'a Block>,
    docs: &'a HashMap<String, ArchiveObjectInfo, RandomState>,
}

impl MarkdownConverter<'_> {
    fn render(&self, root: &Block) -> String {
        let mut out = String::new();
        let mut state = RenderState::default();
        self.render_children(&mut out, &mut state, root);
        out
    }

    fn render_children(&self, out: &mut String, state: &mut RenderState, parent: &Block) {
        for child_id in &parent.children_ids {
            let Some(block) = self.blocks_by_id.get(child_id) else {
                continue;
            };
            self.render_block(out, state, block);
        }
    }

    fn render_block(&self, out: &mut String, state: &mut RenderState, block: &Block) {
        match block.content_value.as_ref() {
            Some(ContentValue::Text(text)) => self.render_text(out, state, block, text),
            Some(ContentValue::File(file)) => self.render_file(out, state, file),
            Some(ContentValue::Bookmark(bookmark)) => self.render_bookmark(out, state, bookmark),
            Some(ContentValue::Table(_)) => self.render_table(out, state, block),
            Some(ContentValue::Div(div)) => {
                if matches!(
                    DivStyle::try_from(div.style).ok(),
                    Some(DivStyle::Dots | DivStyle::Line)
                ) {
                    out.push_str(" --- \n");
                }
                self.render_children(out, state, block);
            }
            Some(ContentValue::Link(link)) => self.render_link(out, state, link),
            Some(ContentValue::Latex(latex)) => self.render_latex(out, state, latex),
            _ => self.render_children(out, state, block),
        }
    }

    fn render_text(&self, out: &mut String, state: &mut RenderState, block: &Block, text: &Text) {
        let style = TextStyle::try_from(text.style).unwrap_or(TextStyle::Paragraph);
        if state.list_opened && !matches!(style, TextStyle::Marked | TextStyle::Numbered) {
            out.push_str("   \n");
            state.list_opened = false;
            state.list_number = 0;
        }

        out.push_str(&state.indent);
        match style {
            TextStyle::Header1 | TextStyle::ToggleHeader1 | TextStyle::Title => {
                out.push_str("# ");
                self.render_text_content(out, text);
                let mut nested = state.with_space_indent();
                self.render_children(out, &mut nested, block);
            }
            TextStyle::Header2 | TextStyle::ToggleHeader2 => {
                out.push_str("## ");
                self.render_text_content(out, text);
                let mut nested = state.with_space_indent();
                self.render_children(out, &mut nested, block);
            }
            TextStyle::Header3 | TextStyle::ToggleHeader3 => {
                out.push_str("### ");
                self.render_text_content(out, text);
                let mut nested = state.with_space_indent();
                self.render_children(out, &mut nested, block);
            }
            TextStyle::Header4 => {
                out.push_str("#### ");
                self.render_text_content(out, text);
                let mut nested = state.with_space_indent();
                self.render_children(out, &mut nested, block);
            }
            TextStyle::Quote | TextStyle::Toggle => {
                out.push_str("> ");
                out.push_str(&text.text.replace('\n', "   \n> "));
                out.push_str("   \n\n");
                self.render_children(out, state, block);
            }
            TextStyle::Code => {
                out.push_str("```\n");
                out.push_str(&state.indent);
                out.push_str(&text.text.replace("```", "\\`\\`\\`"));
                out.push('\n');
                out.push_str(&state.indent);
                out.push_str("```\n");
                self.render_children(out, state, block);
            }
            TextStyle::Checkbox => {
                if text.checked {
                    out.push_str("- [x] ");
                } else {
                    out.push_str("- [ ] ");
                }
                self.render_text_content(out, text);
                let mut nested = state.with_nb_indent();
                self.render_children(out, &mut nested, block);
            }
            TextStyle::Marked => {
                out.push_str("- ");
                self.render_text_content(out, text);
                let mut nested = state.with_space_indent();
                self.render_children(out, &mut nested, block);
                state.list_opened = true;
            }
            TextStyle::Numbered => {
                state.list_number += 1;
                out.push_str(&format!("{}. ", state.list_number));
                self.render_text_content(out, text);
                let mut nested = state.with_space_indent();
                self.render_children(out, &mut nested, block);
                state.list_opened = true;
            }
            _ => {
                self.render_text_content(out, text);
                let mut nested = state.with_nb_indent();
                self.render_children(out, &mut nested, block);
            }
        }
    }

    fn render_text_content(&self, out: &mut String, text: &Text) {
        let mut marks = MarksWriter::new(self, text);
        let chars: Vec<char> = text.text.chars().collect();
        for (idx, ch) in chars.iter().enumerate() {
            marks.write_marks(out, idx);
            escape_markdown_char(*ch, out);
        }
        marks.write_marks(out, chars.len());
        out.push_str("   \n");
    }

    fn render_file(&self, out: &mut String, state: &RenderState, file: &File) {
        if !matches!(FileState::try_from(file.state).ok(), Some(FileState::Done)) {
            return;
        }
        let (title, filename) = self.link_info_for_file(file);
        if title.is_empty() || filename.is_empty() {
            return;
        }
        out.push_str(&state.indent);
        if matches!(FileType::try_from(file.r#type).ok(), Some(FileType::Image)) {
            out.push_str(&format!("![{title}]({filename})    \n"));
        } else {
            out.push_str(&format!("[{title}]({filename})    \n"));
        }
    }

    #[allow(clippy::unused_self)]
    fn render_bookmark(&self, out: &mut String, state: &RenderState, bookmark: &Bookmark) {
        if bookmark.url.is_empty() {
            return;
        }
        out.push_str(&state.indent);
        let title = if bookmark.title.is_empty() {
            bookmark.url.clone()
        } else {
            escape_markdown_string(&bookmark.title)
        };
        out.push_str(&format!("[{}]({})    \n", title, bookmark.url));
    }

    fn render_link(&self, out: &mut String, state: &RenderState, link: &Link) {
        if link.target_block_id.is_empty() {
            return;
        }
        let Some((title, filename)) = self.link_info(&link.target_block_id) else {
            return;
        };
        out.push_str(&state.indent);
        out.push_str(&format!(
            "[{}]({})    \n",
            escape_markdown_string(&title),
            filename
        ));
    }

    #[allow(clippy::unused_self)]
    fn render_latex(&self, out: &mut String, state: &RenderState, latex: &Latex) {
        out.push_str(&state.indent);
        out.push_str("\n$$\n");
        out.push_str(&latex.text);
        out.push_str("\n$$\n");
    }

    fn render_table(&self, out: &mut String, state: &mut RenderState, table_block: &Block) {
        let mut column_ids: Vec<String> = Vec::new();
        let mut row_ids: Vec<String> = Vec::new();

        for child_id in &table_block.children_ids {
            let Some(child) = self.blocks_by_id.get(child_id) else {
                continue;
            };
            match child.content_value.as_ref() {
                Some(ContentValue::Layout(layout)) => {
                    match LayoutStyle::try_from(layout.style).ok() {
                        Some(LayoutStyle::TableColumns) => {
                            column_ids.clone_from(&child.children_ids);
                        }
                        Some(LayoutStyle::TableRows) => {
                            row_ids.clone_from(&child.children_ids);
                        }
                        _ => {}
                    }
                }
                Some(ContentValue::TableRow(_)) => row_ids.push(child.id.clone()),
                Some(ContentValue::TableColumn(_)) => column_ids.push(child.id.clone()),
                _ => {}
            }
        }

        if row_ids.is_empty() {
            self.render_children(out, state, table_block);
            return;
        }

        let rows = self.build_table_rows(&row_ids, &column_ids);
        write_markdown_table(out, &state.indent, rows);
    }

    fn build_table_rows(&self, row_ids: &[String], column_ids: &[String]) -> Vec<Vec<String>> {
        let mut rows: Vec<Vec<String>> = Vec::new();
        for row_id in row_ids {
            let Some(row_block) = self.blocks_by_id.get(row_id) else {
                continue;
            };
            let mut by_col: HashMap<String, String> = HashMap::new();
            let mut unordered: Vec<String> = Vec::new();

            for cell_id in &row_block.children_ids {
                let Some(cell_block) = self.blocks_by_id.get(cell_id) else {
                    continue;
                };
                let content = self.render_cell(cell_block);
                if let Some(col_id) = cell_id.strip_prefix(&format!("{row_id}-")) {
                    by_col.insert(col_id.to_string(), content);
                } else {
                    unordered.push(content);
                }
            }

            if column_ids.is_empty() {
                if by_col.is_empty() {
                    rows.push(unordered);
                } else {
                    let mut pairs: Vec<(String, String)> = by_col.into_iter().collect();
                    pairs.sort_by(|a, b| a.0.cmp(&b.0));
                    rows.push(pairs.into_iter().map(|(_, v)| v).collect());
                }
                continue;
            }

            let mut row = Vec::with_capacity(column_ids.len());
            for (idx, col_id) in column_ids.iter().enumerate() {
                if let Some(cell) = by_col.remove(col_id) {
                    row.push(cell);
                } else if let Some(cell) = unordered.get(idx) {
                    row.push(cell.clone());
                } else {
                    row.push(" ".to_string());
                }
            }
            rows.push(row);
        }
        rows
    }

    fn render_cell(&self, block: &Block) -> String {
        let mut text = String::new();
        let mut state = RenderState::default();
        self.render_block(&mut text, &mut state, block);
        text = text.replace("\r\n", " ").replace('\n', " ");
        let trimmed = text.trim();
        if trimmed.is_empty() {
            " ".to_string()
        } else {
            trimmed.to_string()
        }
    }

    fn link_info_for_file(&self, file: &File) -> (String, String) {
        if !file.target_object_id.is_empty() {
            if let Some((title, filename)) = self.link_info(&file.target_object_id) {
                return (title, filename);
            }
            let fallback_title = path_basename(&file.name).to_string();
            let fallback_ext = file_ext_from_name(&file.name).unwrap_or_default();
            let filename =
                file_name_for_file(&file.target_object_id, &fallback_title, &fallback_ext);
            return (fallback_title, filename);
        }

        let title = path_basename(&file.name).to_string();
        let ext = file_ext_from_name(&file.name).unwrap_or_default();
        let filename = file_name_for_file(&file.hash, &title, &ext);
        (title, filename)
    }

    fn link_info(&self, object_id: &str) -> Option<(String, String)> {
        let info = self.docs.get(object_id)?;
        let mut title = info.name.clone();
        if title.is_empty() {
            title.clone_from(&info.snippet);
        }
        if title.is_empty() {
            title = object_id.to_string();
        }

        let is_file = matches!(info.layout, Some(8..=12));
        if is_file {
            let ext = info
                .file_ext
                .as_deref()
                .map(|ext| format!(".{}", ext.trim_start_matches('.')))
                .unwrap_or_default();
            let file_title = title.trim_end_matches(&ext).to_string();
            let filename = file_name_for_file(object_id, &file_title, &ext);
            return Some((file_title, filename));
        }

        let filename = file_name_for_doc(object_id, &title);
        Some((title, filename))
    }
}

#[derive(Debug, Clone)]
struct MarkRange {
    from: usize,
    to: usize,
    mark: Mark,
}

#[derive(Debug)]
struct MarksWriter<'a, 'b> {
    converter: &'a MarkdownConverter<'b>,
    starts: HashMap<usize, Vec<MarkRange>>,
    ends: HashMap<usize, Vec<MarkRange>>,
    open: Vec<MarkRange>,
}

impl<'a, 'b> MarksWriter<'a, 'b> {
    fn new(converter: &'a MarkdownConverter<'b>, text: &Text) -> Self {
        let mut starts: HashMap<usize, Vec<MarkRange>> = HashMap::new();
        let mut ends: HashMap<usize, Vec<MarkRange>> = HashMap::new();
        if let Some(marks) = text.marks.as_ref() {
            for mark in &marks.marks {
                let Some(range) = mark.range.as_ref() else {
                    continue;
                };
                if range.from == range.to || range.from < 0 || range.to < 0 {
                    continue;
                }
                #[allow(clippy::cast_sign_loss)]
                let item = MarkRange {
                    from: range.from as usize,
                    to: range.to as usize,
                    mark: mark.clone(),
                };
                starts.entry(item.from).or_default().push(item.clone());
                ends.entry(item.to).or_default().push(item);
            }
        }
        for values in starts.values_mut() {
            values.sort_by(|a, b| {
                let la = a.to.saturating_sub(a.from);
                let lb = b.to.saturating_sub(b.from);
                lb.cmp(&la).then_with(|| a.mark.r#type.cmp(&b.mark.r#type))
            });
        }
        for values in ends.values_mut() {
            values.sort_by(|a, b| {
                let la = a.to.saturating_sub(a.from);
                let lb = b.to.saturating_sub(b.from);
                lb.cmp(&la).then_with(|| a.mark.r#type.cmp(&b.mark.r#type))
            });
        }
        Self {
            converter,
            starts,
            ends,
            open: Vec::new(),
        }
    }

    fn write_marks(&mut self, out: &mut String, pos: usize) {
        if let Some(ends) = self.ends.get(&pos).cloned() {
            for item in ends.iter().rev() {
                if let Some(last) = self.open.pop()
                    && (last.from != item.from || last.to != item.to || last.mark != item.mark)
                {
                    self.open.push(last.clone());
                }
                self.write_mark(out, &item.mark, false);
            }
        }
        if let Some(starts) = self.starts.get(&pos).cloned() {
            for item in &starts {
                self.write_mark(out, &item.mark, true);
                self.open.push(item.clone());
            }
        }
    }

    fn write_mark(&self, out: &mut String, mark: &Mark, start: bool) {
        let kind = MarkType::try_from(mark.r#type).ok();
        match kind {
            Some(MarkType::Strikethrough) => out.push_str("~~"),
            Some(MarkType::Italic) => out.push('*'),
            Some(MarkType::Bold) => out.push_str("**"),
            Some(MarkType::Keyboard) => out.push('`'),
            Some(MarkType::Link) => {
                if start {
                    out.push('[');
                } else {
                    out.push_str(&format!("]({})", mark.param));
                }
            }
            Some(MarkType::Mention | MarkType::Object) => {
                if let Some((_, filename)) = self.converter.link_info(&mark.param) {
                    if start {
                        out.push('[');
                    } else {
                        out.push_str(&format!("]({filename})"));
                    }
                }
            }
            Some(MarkType::Emoji) if start => {
                out.push_str(&mark.param);
            }
            _ => {}
        }
    }
}

fn write_markdown_table(out: &mut String, indent: &str, mut rows: Vec<Vec<String>>) {
    if rows.is_empty() {
        return;
    }
    let cols = rows.iter().map(std::vec::Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return;
    }
    for row in &mut rows {
        while row.len() < cols {
            row.push(" ".to_string());
        }
    }

    let mut widths = vec![3usize; cols];
    for row in &rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell.len());
        }
    }

    for (idx, row) in rows.iter().enumerate() {
        out.push_str(indent);
        out.push('|');
        for (col, cell) in row.iter().enumerate() {
            out.push_str(&format!(" {:<width$} |", cell, width = widths[col]));
        }
        out.push('\n');

        if idx == 0 {
            out.push_str(indent);
            out.push('|');
            for width in &widths {
                out.push(':');
                out.push_str(&"-".repeat(width.saturating_add(1)));
                out.push('|');
            }
            out.push('\n');
        }
    }
    out.push('\n');
}

fn escape_markdown_char(ch: char, out: &mut String) {
    if matches!(
        ch,
        '\\' | '`'
            | '*'
            | '_'
            | '{'
            | '}'
            | '['
            | ']'
            | '('
            | ')'
            | '#'
            | '+'
            | '-'
            | '.'
            | '!'
            | '|'
            | '>'
            | '~'
    ) {
        out.push('\\');
    }
    out.push(ch);
}

fn escape_markdown_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for ch in value.chars() {
        escape_markdown_char(ch, &mut out);
    }
    out
}

fn path_basename(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or(path)
}

fn file_ext_from_name(name: &str) -> Option<String> {
    Path::new(name)
        .extension()
        .and_then(|v| v.to_str())
        .map(|v| format!(".{v}"))
}

fn sanitize_filename(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_whitespace() || matches!(ch, '/' | '\\') {
            out.push('_');
        }
    }
    let compact = out.trim_matches('_');
    if compact.is_empty() {
        "untitled".to_string()
    } else {
        compact.to_string()
    }
}

fn file_name_for_doc(id: &str, title: &str) -> String {
    let base = sanitize_filename(title);
    format!("{base}_{id}.md")
}

fn file_name_for_file(id: &str, title: &str, ext: &str) -> String {
    let base = sanitize_filename(title);
    format!("files/{base}_{id}{ext}")
}

fn struct_field_as_string(details: &Struct, key: &str) -> Option<String> {
    let value = details.fields.get(key)?;
    match value.kind.as_ref()? {
        Kind::StringValue(v) => Some(v.clone()),
        Kind::NumberValue(v) => Some(v.to_string()),
        Kind::BoolValue(v) => Some(v.to_string()),
        _ => None,
    }
}

/// Build a lightweight archive-wide object metadata index used for markdown link rendering.
pub fn build_archive_object_index(
    reader: &ArchiveReader,
) -> Result<HashMap<String, ArchiveObjectInfo>> {
    let mut out = HashMap::new();
    for file in reader.list_files()? {
        let lower = file.path.to_ascii_lowercase();
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        if !lower.ends_with(".pb") && !lower.ends_with(".pb.json") {
            continue;
        }
        let Ok(bytes) = reader.read_bytes(&file.path) else {
            continue;
        };
        let Ok(details) = parse_snapshot_details_to_map(&file.path, &bytes) else {
            continue;
        };
        let Some(id) = details.get("id").cloned().filter(|v| !v.is_empty()) else {
            continue;
        };
        let info = ArchiveObjectInfo {
            id: id.clone(),
            name: details.get("name").cloned().unwrap_or_default(),
            snippet: details.get("snippet").cloned().unwrap_or_default(),
            layout: details
                .get("layout")
                .and_then(|v| v.parse::<i64>().ok())
                .or_else(|| {
                    details
                        .get("resolvedLayout")
                        .and_then(|v| v.parse::<i64>().ok())
                }),
            file_ext: details.get("fileExt").cloned(),
        };
        out.insert(info.id.clone(), info);
    }
    Ok(out)
}

fn find_snapshot_path(reader: &ArchiveReader, object_id: &str) -> Option<String> {
    let pb = format!("{object_id}.pb");
    let pb_json = format!("{object_id}.pb.json");
    let files = reader.list_files().ok()?;
    files.iter().find_map(|f| {
        let lower = f.path.to_ascii_lowercase();
        if lower.ends_with(&pb) || lower.ends_with(&pb_json) {
            Some(f.path.clone())
        } else {
            None
        }
    })
}

/// Convert a snapshot file (`objects/<id>.pb` or `objects/<id>.pb.json`) to markdown text,
/// using a prebuilt object index for link/name resolution.
pub fn convert_archive_snapshot_to_markdown(
    reader: &ArchiveReader,
    snapshot_path: &str,
    object_index: &HashMap<String, ArchiveObjectInfo, RandomState>,
) -> Result<String> {
    let snapshot_bytes = reader
        .read_bytes(snapshot_path)
        .with_context(|| format!("failed reading snapshot from archive: {snapshot_path}"))?;
    convert_snapshot_bytes_to_markdown(snapshot_path, &snapshot_bytes, object_index)
}

/// Convert raw snapshot bytes (`*.pb`/`*.pb.json`) to markdown using a prebuilt object index.
pub fn convert_snapshot_bytes_to_markdown(
    snapshot_path: &str,
    snapshot_bytes: &[u8],
    object_index: &HashMap<String, ArchiveObjectInfo, RandomState>,
) -> Result<String> {
    let lower = snapshot_path.to_ascii_lowercase();
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    if lower.ends_with(".pb") {
        return convert_pb_snapshot_to_markdown(snapshot_bytes, object_index);
    }
    if lower.ends_with(".pb.json") {
        return convert_pb_json_snapshot_to_markdown(snapshot_bytes, object_index);
    }
    bail!("unsupported snapshot format: {snapshot_path}")
}

fn parse_snapshot_details_to_map(path: &str, bytes: &[u8]) -> Result<HashMap<String, String>> {
    let lower = path.to_ascii_lowercase();
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    if lower.ends_with(".pb") {
        let snapshot =
            SnapshotWithType::decode(bytes).context("failed to decode protobuf snapshot")?;
        let data = snapshot
            .snapshot
            .and_then(|v| v.data)
            .ok_or_else(|| anyhow!("snapshot payload missing data"))?;
        let Some(details) = data.details else {
            return Ok(HashMap::new());
        };
        let mut map = HashMap::new();
        for (k, v) in details.fields {
            if let Some(value) = prost_value_to_string(&v) {
                map.insert(k, value);
            }
        }
        return Ok(map);
    }
    if lower.ends_with(".pb.json") {
        let root: JsonValue = serde_json::from_slice(bytes).context("invalid pb-json")?;
        let details = root
            .get("snapshot")
            .and_then(|v| v.get("data"))
            .and_then(|v| v.get("details"))
            .and_then(JsonValue::as_object)
            .ok_or_else(|| anyhow!("pb-json snapshot missing details object"))?;
        let mut map = HashMap::new();
        for (k, v) in details {
            if let Some(value) = json_value_to_string(v) {
                map.insert(k.clone(), value);
            }
        }
        return Ok(map);
    }
    bail!("unsupported snapshot format: {path}")
}

fn prost_value_to_string(v: &prost_types::Value) -> Option<String> {
    match v.kind.as_ref()? {
        Kind::StringValue(s) => Some(s.clone()),
        Kind::NumberValue(n) => Some(n.to_string()),
        Kind::BoolValue(b) => Some(b.to_string()),
        _ => None,
    }
}

fn json_value_to_string(v: &JsonValue) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if let Some(n) = v.as_i64() {
        return Some(n.to_string());
    }
    if let Some(n) = v.as_f64() {
        return Some(n.to_string());
    }
    if let Some(b) = v.as_bool() {
        return Some(b.to_string());
    }
    None
}

fn infer_raw_payload_path(
    object_id: &str,
    details: &HashMap<String, String>,
    files: &[crate::archive::ArchiveFileEntry],
) -> Option<String> {
    let mut tokens = Vec::<String>::new();
    if !object_id.is_empty() {
        tokens.push(object_id.to_ascii_lowercase());
    }
    for key in [
        "source",
        "fileHash",
        "hash",
        "fileObjectId",
        "targetObjectId",
        "fileName",
        "name",
        "oldAnytypeID",
    ] {
        if let Some(value) = details.get(key) {
            let token = value.trim().to_ascii_lowercase();
            if !token.is_empty() {
                tokens.push(token);
            }
        }
    }
    if let Some(ext) = details.get("fileExt") {
        let token = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        if !token.is_empty() {
            tokens.push(format!(".{token}"));
        }
    }

    let mut best: Option<(&str, i32)> = None;
    for file in files {
        let path_lc = file.path.to_ascii_lowercase();
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        if path_lc.ends_with(".pb") || path_lc.ends_with(".pb.json") || path_lc == "manifest.json" {
            continue;
        }
        let mut score = 0;
        if path_lc.starts_with("files/") {
            score += 30;
        }
        for token in &tokens {
            if token.len() < 3 {
                continue;
            }
            if path_lc.contains(token) {
                score += 25;
            }
        }
        if score == 0 {
            continue;
        }
        match best {
            Some((_, best_score)) if best_score >= score => {}
            _ => best = Some((file.path.as_str(), score)),
        }
    }
    best.map(|(path, _)| path.to_string())
}

fn should_skip_export(sb_type: SmartBlockType) -> bool {
    matches!(
        sb_type,
        SmartBlockType::StType
            | SmartBlockType::StRelation
            | SmartBlockType::StRelationOption
            | SmartBlockType::Participant
            | SmartBlockType::SpaceView
            | SmartBlockType::ChatObjectDeprecated
            | SmartBlockType::ChatDerivedObject
    )
}

/// Convert one archive object snapshot (`objects/<id>.pb`) to markdown text.
///
/// This reads the target object snapshot from the archive and builds a lightweight
/// index of object details from sibling `*.pb` snapshots so object/file links can be
/// rendered as markdown links.
pub fn convert_archive_object_pb_to_markdown(
    archive_path: &Path,
    object_id: &str,
) -> Result<String> {
    let reader = ArchiveReader::from_path(archive_path)?;
    let snapshot_path = find_snapshot_path(&reader, object_id)
        .ok_or_else(|| anyhow!("snapshot not found in archive for object: {object_id}"))?;
    if !snapshot_path.to_ascii_lowercase().ends_with(".pb") {
        bail!("markdown conversion currently supports protobuf snapshots (*.pb) only");
    }
    let snapshot_bytes = reader
        .read_bytes(&snapshot_path)
        .with_context(|| format!("failed reading snapshot from archive: {snapshot_path}"))?;
    let object_index = build_archive_object_index(&reader)?;
    convert_pb_snapshot_to_markdown(&snapshot_bytes, &object_index)
}

/// Convert one archive object snapshot (`objects/<id>.pb` or `objects/<id>.pb.json`) to markdown.
pub fn convert_archive_object_to_markdown(archive_path: &Path, object_id: &str) -> Result<String> {
    let reader = ArchiveReader::from_path(archive_path)?;
    let snapshot_path = find_snapshot_path(&reader, object_id)
        .ok_or_else(|| anyhow!("snapshot not found in archive for object: {object_id}"))?;
    let object_index = build_archive_object_index(&reader)?;
    convert_archive_snapshot_to_markdown(&reader, &snapshot_path, &object_index)
}

pub fn save_archive_object(
    archive_path: &Path,
    object_id: &str,
    dest: &Path,
) -> Result<SavedObjectKind> {
    let reader = ArchiveReader::from_path(archive_path)?;
    let files = reader.list_files()?;
    let snapshot_path = find_snapshot_path(&reader, object_id)
        .ok_or_else(|| anyhow!("snapshot not found in archive for object: {object_id}"))?;
    let snapshot_bytes = reader
        .read_bytes(&snapshot_path)
        .with_context(|| format!("failed reading snapshot from archive: {snapshot_path}"))?;
    let details = parse_snapshot_details_to_map(&snapshot_path, &snapshot_bytes)?;

    if !is_file_layout_from_details(&details) {
        let markdown = convert_archive_object_to_markdown(archive_path, object_id)?;
        fs::write(dest, markdown)
            .with_context(|| format!("failed writing markdown to {}", dest.display()))?;
        return Ok(SavedObjectKind::Markdown);
    }

    let payload = infer_raw_payload_path(object_id, &details, &files)
        .ok_or_else(|| MissingRawPayloadError::new(object_id))?;
    let bytes = reader
        .read_bytes(&payload)
        .with_context(|| format!("failed reading payload from archive: {payload}"))?;
    fs::write(dest, bytes)
        .with_context(|| format!("failed writing raw payload to {}", dest.display()))?;
    Ok(SavedObjectKind::Raw)
}

fn is_file_layout_from_details(details: &HashMap<String, String>) -> bool {
    let parse_i64 = |key: &str| details.get(key).and_then(|v| v.parse::<i64>().ok());
    matches!(
        parse_i64("layout").or_else(|| parse_i64("resolvedLayout")),
        Some(8..=12)
    )
}

fn convert_pb_json_snapshot_to_markdown(
    snapshot_bytes: &[u8],
    object_index: &HashMap<String, ArchiveObjectInfo>,
) -> Result<String> {
    let root: JsonValue = serde_json::from_slice(snapshot_bytes).context("invalid pb-json")?;
    let sb_type = parse_json_smart_block_type(&root);
    if let Some(sb_type) = sb_type
        && should_skip_export(sb_type)
    {
        return Ok(String::new());
    }
    let data = root
        .get("snapshot")
        .and_then(|v| v.get("data"))
        .ok_or_else(|| anyhow!("pb-json snapshot missing snapshot.data"))?;
    let blocks_json = data
        .get("blocks")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| anyhow!("pb-json snapshot missing snapshot.data.blocks"))?;
    if blocks_json.is_empty() {
        return Ok(String::new());
    }
    let blocks: Vec<Block> = blocks_json
        .iter()
        .map(parse_json_block)
        .collect::<Result<Vec<_>>>()?;
    if blocks.is_empty() {
        return Ok(String::new());
    }

    let mut blocks_by_id = HashMap::<String, &Block>::with_capacity(blocks.len());
    for block in &blocks {
        blocks_by_id.insert(block.id.clone(), block);
    }

    let root_id = data
        .get("details")
        .and_then(JsonValue::as_object)
        .and_then(|details| details.get("id"))
        .and_then(JsonValue::as_str)
        .map_or_else(|| blocks[0].id.clone(), ToString::to_string);
    let Some(root) = blocks_by_id.get(&root_id) else {
        bail!("root block not found: {root_id}");
    };
    if root.children_ids.is_empty() {
        return Ok(String::new());
    }

    let converter = MarkdownConverter {
        blocks_by_id,
        docs: object_index,
    };
    let root = converter
        .blocks_by_id
        .get(&root_id)
        .ok_or_else(|| anyhow!("root block not found after converter init: {root_id}"))?;
    Ok(converter.render(root))
}

fn parse_json_smart_block_type(root: &JsonValue) -> Option<SmartBlockType> {
    let sb = root.get("sbType")?;
    if let Some(name) = sb.as_str() {
        return SmartBlockType::from_str_name(name);
    }
    if let Some(value) = sb.as_i64().and_then(|n| i32::try_from(n).ok()) {
        return SmartBlockType::try_from(value).ok();
    }
    None
}

fn parse_json_block(value: &JsonValue) -> Result<Block> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow!("pb-json block is not an object"))?;
    let id = obj
        .get("id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| anyhow!("pb-json block missing id"))?
        .to_string();
    let children_ids = obj
        .get("childrenIds")
        .and_then(JsonValue::as_array)
        .map_or_else(Vec::new, |items| {
            items
                .iter()
                .filter_map(JsonValue::as_str)
                .map(ToString::to_string)
                .collect()
        });
    let background_color = obj
        .get("backgroundColor")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    let align = obj
        .get("align")
        .map_or(0, |v| parse_block_align(v).unwrap_or_default());
    let vertical_align = obj
        .get("verticalAlign")
        .map_or(0, |v| parse_block_vertical_align(v).unwrap_or_default());
    let content_value = parse_json_content_value(obj)?;

    Ok(Block {
        id,
        fields: None,
        restrictions: None,
        children_ids,
        background_color,
        align,
        vertical_align,
        content_value,
    })
}

fn parse_json_content_value(
    obj: &serde_json::Map<String, JsonValue>,
) -> Result<Option<ContentValue>> {
    if let Some(v) = obj.get("text") {
        return Ok(Some(ContentValue::Text(parse_json_text(v)?)));
    }
    if let Some(v) = obj.get("file") {
        return Ok(Some(ContentValue::File(parse_json_file(v)?)));
    }
    if let Some(v) = obj.get("bookmark") {
        return Ok(Some(ContentValue::Bookmark(parse_json_bookmark(v))));
    }
    if let Some(v) = obj.get("link") {
        return Ok(Some(ContentValue::Link(parse_json_link(v))));
    }
    if let Some(v) = obj.get("latex") {
        return Ok(Some(ContentValue::Latex(parse_json_latex(v))));
    }
    if let Some(v) = obj.get("div") {
        return Ok(Some(ContentValue::Div(parse_json_div(v))));
    }
    if obj.contains_key("table") {
        return Ok(Some(ContentValue::Table(Table {})));
    }
    if obj.contains_key("tableColumn") {
        return Ok(Some(ContentValue::TableColumn(TableColumn {})));
    }
    if let Some(v) = obj.get("tableRow") {
        return Ok(Some(ContentValue::TableRow(parse_json_table_row(v))));
    }
    Ok(None)
}

fn parse_json_text(value: &JsonValue) -> Result<Text> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow!("pb-json text block is not an object"))?;
    let style = obj
        .get("style")
        .map_or(0, |v| parse_text_style(v).unwrap_or(0));
    let marks = obj
        .get("marks")
        .map(parse_json_marks)
        .transpose()?
        .or_else(|| Some(anytype_rpc::model::block::content::text::Marks { marks: Vec::new() }));

    Ok(Text {
        text: obj
            .get("text")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        style,
        marks,
        checked: obj
            .get("checked")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        color: obj
            .get("color")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        icon_emoji: obj
            .get("iconEmoji")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        icon_image: obj
            .get("iconImage")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

fn parse_json_marks(value: &JsonValue) -> Result<anytype_rpc::model::block::content::text::Marks> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow!("pb-json marks is not an object"))?;
    let marks = obj
        .get("marks")
        .and_then(JsonValue::as_array)
        .map_or_else(Vec::new, |items| {
            items.iter().filter_map(parse_json_mark).collect()
        });
    Ok(anytype_rpc::model::block::content::text::Marks { marks })
}

fn parse_json_mark(value: &JsonValue) -> Option<Mark> {
    let obj = value.as_object()?;
    let range = obj.get("range").and_then(parse_json_range);
    let r#type = obj
        .get("type")
        .map_or(0, |v| parse_mark_type(v).unwrap_or(0));
    let param = obj
        .get("param")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    Some(Mark {
        range,
        r#type,
        param,
    })
}

fn parse_json_range(value: &JsonValue) -> Option<Range> {
    let obj = value.as_object()?;
    let from = obj.get("from").and_then(JsonValue::as_i64)?;
    let to = obj.get("to").and_then(JsonValue::as_i64)?;
    Some(Range {
        from: i32::try_from(from).ok()?,
        to: i32::try_from(to).ok()?,
    })
}

fn parse_json_file(value: &JsonValue) -> Result<File> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow!("pb-json file block is not an object"))?;
    Ok(File {
        hash: obj
            .get("hash")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        name: obj
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        r#type: obj
            .get("type")
            .map_or(0, |v| parse_file_type(v).unwrap_or(0)),
        mime: obj
            .get("mime")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        size: obj.get("size").and_then(JsonValue::as_i64).unwrap_or(0),
        added_at: obj.get("addedAt").and_then(JsonValue::as_i64).unwrap_or(0),
        target_object_id: obj
            .get("targetObjectId")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        state: obj
            .get("state")
            .map_or(FileState::Done as i32, |v| parse_file_state(v).unwrap_or(0)),
        style: obj
            .get("style")
            .map_or(0, |v| parse_file_style(v).unwrap_or(0)),
    })
}

fn parse_json_bookmark(value: &JsonValue) -> Bookmark {
    let obj = value.as_object();
    Bookmark {
        url: obj
            .and_then(|o| o.get("url"))
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        title: obj
            .and_then(|o| o.get("title"))
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        description: obj
            .and_then(|o| o.get("description"))
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        image_hash: obj
            .and_then(|o| o.get("imageHash"))
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        favicon_hash: obj
            .and_then(|o| o.get("faviconHash"))
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        r#type: 0,
        target_object_id: obj
            .and_then(|o| o.get("targetObjectId"))
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        state: 0,
    }
}

fn parse_json_link(value: &JsonValue) -> Link {
    let obj = value.as_object();
    Link {
        target_block_id: obj
            .and_then(|o| o.get("targetBlockId"))
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        style: obj
            .and_then(|o| o.get("style"))
            .map_or(0, |v| parse_link_style(v).unwrap_or(0)),
        fields: None,
        icon_size: obj
            .and_then(|o| o.get("iconSize"))
            .map_or(0, |v| parse_link_icon_size(v).unwrap_or(0)),
        card_style: obj
            .and_then(|o| o.get("cardStyle"))
            .map_or(0, |v| parse_link_card_style(v).unwrap_or(0)),
        description: obj
            .and_then(|o| o.get("description"))
            .map_or(0, |v| parse_link_description(v).unwrap_or(0)),
        relations: obj
            .and_then(|o| o.get("relations"))
            .and_then(JsonValue::as_array)
            .map_or_else(Vec::new, |arr| {
                arr.iter()
                    .filter_map(JsonValue::as_str)
                    .map(ToString::to_string)
                    .collect()
            }),
    }
}

fn parse_json_latex(value: &JsonValue) -> Latex {
    let obj = value.as_object();
    Latex {
        text: obj
            .and_then(|o| o.get("text"))
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        processor: 0,
    }
}

fn parse_json_div(value: &JsonValue) -> Div {
    let style = value
        .as_object()
        .and_then(|o| o.get("style"))
        .and_then(parse_div_style)
        .unwrap_or(0);
    Div { style }
}

fn parse_json_table_row(value: &JsonValue) -> TableRow {
    let is_header = value
        .as_object()
        .and_then(|o| o.get("isHeader"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    TableRow { is_header }
}

fn parse_block_align(value: &JsonValue) -> Option<i32> {
    if let Some(name) = value.as_str() {
        return anytype_rpc::model::block::Align::from_str_name(name).map(|v| v as i32);
    }
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn parse_block_vertical_align(value: &JsonValue) -> Option<i32> {
    if let Some(name) = value.as_str() {
        return anytype_rpc::model::block::VerticalAlign::from_str_name(name).map(|v| v as i32);
    }
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn parse_text_style(value: &JsonValue) -> Option<i32> {
    if let Some(name) = value.as_str() {
        return TextStyle::from_str_name(name).map(|v| v as i32);
    }
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn parse_mark_type(value: &JsonValue) -> Option<i32> {
    if let Some(name) = value.as_str() {
        return MarkType::from_str_name(name).map(|v| v as i32);
    }
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn parse_file_type(value: &JsonValue) -> Option<i32> {
    if let Some(name) = value.as_str() {
        return FileType::from_str_name(name).map(|v| v as i32);
    }
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn parse_file_state(value: &JsonValue) -> Option<i32> {
    if let Some(name) = value.as_str() {
        return FileState::from_str_name(name).map(|v| v as i32);
    }
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn parse_file_style(value: &JsonValue) -> Option<i32> {
    if let Some(name) = value.as_str() {
        return anytype_rpc::model::block::content::file::Style::from_str_name(name)
            .map(|v| v as i32);
    }
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn parse_link_style(value: &JsonValue) -> Option<i32> {
    if let Some(name) = value.as_str() {
        return anytype_rpc::model::block::content::link::Style::from_str_name(name)
            .map(|v| v as i32);
    }
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn parse_link_icon_size(value: &JsonValue) -> Option<i32> {
    if let Some(name) = value.as_str() {
        return anytype_rpc::model::block::content::link::IconSize::from_str_name(name)
            .map(|v| v as i32);
    }
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn parse_link_card_style(value: &JsonValue) -> Option<i32> {
    if let Some(name) = value.as_str() {
        return anytype_rpc::model::block::content::link::CardStyle::from_str_name(name)
            .map(|v| v as i32);
    }
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn parse_link_description(value: &JsonValue) -> Option<i32> {
    if let Some(name) = value.as_str() {
        return anytype_rpc::model::block::content::link::Description::from_str_name(name)
            .map(|v| v as i32);
    }
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn parse_div_style(value: &JsonValue) -> Option<i32> {
    if let Some(name) = value.as_str() {
        return DivStyle::from_str_name(name).map(|v| v as i32);
    }
    value.as_i64().and_then(|n| i32::try_from(n).ok())
}

/// Convert a protobuf snapshot payload to markdown.
///
/// `object_index` should include object metadata (name/layout/file extension) for
/// linked objects so mentions/files can be rendered as markdown links.
pub fn convert_pb_snapshot_to_markdown(
    snapshot_bytes: &[u8],
    object_index: &HashMap<String, ArchiveObjectInfo, RandomState>,
) -> Result<String> {
    let snapshot =
        SnapshotWithType::decode(snapshot_bytes).context("failed to decode protobuf snapshot")?;
    let sb_type = SmartBlockType::try_from(snapshot.sb_type).unwrap_or(SmartBlockType::Page);
    if should_skip_export(sb_type) {
        return Ok(String::new());
    }
    let data = snapshot
        .snapshot
        .and_then(|v| v.data)
        .ok_or_else(|| anyhow!("snapshot payload missing data"))?;
    if data.blocks.is_empty() {
        return Ok(String::new());
    }

    let mut blocks_by_id = HashMap::<String, &Block>::with_capacity(data.blocks.len());
    for block in &data.blocks {
        blocks_by_id.insert(block.id.clone(), block);
    }

    let root_id = data
        .details
        .as_ref()
        .and_then(|details| struct_field_as_string(details, "id"))
        .unwrap_or_else(|| data.blocks[0].id.clone());
    let Some(root) = blocks_by_id.get(&root_id) else {
        bail!("root block not found: {root_id}");
    };
    if root.children_ids.is_empty() {
        return Ok(String::new());
    }

    let converter = MarkdownConverter {
        blocks_by_id,
        docs: object_index,
    };
    let root = converter
        .blocks_by_id
        .get(&root_id)
        .ok_or_else(|| anyhow!("root block not found after converter init: {root_id}"))?;
    Ok(converter.render(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    use anytype_rpc::anytype::change::Snapshot as ChangeSnapshot;
    use anytype_rpc::model::SmartBlockSnapshotBase;
    use anytype_rpc::model::block::content::text::Marks;
    use prost_types::Value as ProstValue;
    use serde_json::json;
    use std::path::PathBuf;

    /// Synthetic object id for the in-code archive fixtures.
    ///
    /// The id is invented for tests; no archive outside this repository is required.
    const DOC_ID: &str = "bafyreifixturedoc000000000000000000000000000000000000000000";
    const DOC_NAME: &str = "Fixture Handbook";
    const RAW_FILE_NAME: &str = "fixture-manual.pdf";
    const RAW_PAYLOAD: &[u8] = b"fixture raw payload\n";

    fn text_block(id: &str, body: &str, style: TextStyle) -> Block {
        Block {
            id: id.to_string(),
            content_value: Some(ContentValue::Text(Text {
                text: body.to_string(),
                style: style as i32,
                marks: Some(Marks::default()),
                ..Text::default()
            })),
            ..Block::default()
        }
    }

    fn parent_block(id: &str, children: &[&str], content: Option<ContentValue>) -> Block {
        Block {
            id: id.to_string(),
            children_ids: children.iter().map(|v| (*v).to_string()).collect(),
            content_value: content,
            ..Block::default()
        }
    }

    /// One small document exercising headings, a list/paragraph boundary and a 2x2 table.
    fn fixture_blocks() -> Vec<Block> {
        vec![
            parent_block(DOC_ID, &["title", "section", "item", "para", "table"], None),
            text_block("title", DOC_NAME, TextStyle::Header1),
            text_block("section", "Widget Basics", TextStyle::Header2),
            text_block("item", "Alpha", TextStyle::Marked),
            text_block("para", "Body paragraph", TextStyle::Paragraph),
            parent_block(
                "table",
                &["col-a", "col-b", "row-1", "row-2"],
                Some(ContentValue::Table(Table {})),
            ),
            parent_block(
                "col-a",
                &[],
                Some(ContentValue::TableColumn(TableColumn {})),
            ),
            parent_block(
                "col-b",
                &[],
                Some(ContentValue::TableColumn(TableColumn {})),
            ),
            parent_block(
                "row-1",
                &["row-1-col-a", "row-1-col-b"],
                Some(ContentValue::TableRow(TableRow { is_header: true })),
            ),
            parent_block(
                "row-2",
                &["row-2-col-a", "row-2-col-b"],
                Some(ContentValue::TableRow(TableRow::default())),
            ),
            text_block("row-1-col-a", "Widget", TextStyle::Paragraph),
            text_block("row-1-col-b", "Count", TextStyle::Paragraph),
            text_block("row-2-col-a", "Gear", TextStyle::Paragraph),
            text_block("row-2-col-b", "3", TextStyle::Paragraph),
        ]
    }

    fn string_value(value: &str) -> ProstValue {
        ProstValue {
            kind: Some(Kind::StringValue(value.to_string())),
        }
    }

    fn number_value(value: f64) -> ProstValue {
        ProstValue {
            kind: Some(Kind::NumberValue(value)),
        }
    }

    /// Details for a plain page (layout 0), keyed by the fixture object id.
    fn fixture_details(root_id: &str) -> Struct {
        Struct {
            fields: [
                ("id".to_string(), string_value(root_id)),
                ("name".to_string(), string_value(DOC_NAME)),
                ("layout".to_string(), number_value(0.0)),
            ]
            .into_iter()
            .collect(),
        }
    }

    fn pb_snapshot_bytes(blocks: Vec<Block>, details: Struct) -> Vec<u8> {
        let snapshot = SnapshotWithType {
            sb_type: SmartBlockType::Page as i32,
            snapshot: Some(ChangeSnapshot {
                data: Some(SmartBlockSnapshotBase {
                    blocks,
                    details: Some(details),
                    ..SmartBlockSnapshotBase::default()
                }),
                ..ChangeSnapshot::default()
            }),
        };
        snapshot.encode_to_vec()
    }

    /// Render one fixture block in the archive `*.pb.json` shape so both snapshot
    /// formats are generated from a single in-code document definition.
    fn block_to_json(block: &Block) -> JsonValue {
        let mut obj = serde_json::Map::new();
        obj.insert("id".to_string(), json!(block.id));
        if !block.children_ids.is_empty() {
            obj.insert("childrenIds".to_string(), json!(block.children_ids));
        }
        match block.content_value.as_ref() {
            Some(ContentValue::Text(text)) => {
                let style = TextStyle::try_from(text.style).unwrap_or(TextStyle::Paragraph);
                obj.insert(
                    "text".to_string(),
                    json!({
                        "text": text.text,
                        "style": style.as_str_name(),
                        "checked": text.checked,
                    }),
                );
            }
            Some(ContentValue::Table(_)) => {
                obj.insert("table".to_string(), json!({}));
            }
            Some(ContentValue::TableColumn(_)) => {
                obj.insert("tableColumn".to_string(), json!({}));
            }
            Some(ContentValue::TableRow(row)) => {
                obj.insert("tableRow".to_string(), json!({ "isHeader": row.is_header }));
            }
            _ => {}
        }
        JsonValue::Object(obj)
    }

    fn json_snapshot_bytes(blocks: &[Block], root_id: &str) -> Vec<u8> {
        let doc = json!({
            "sbType": SmartBlockType::Page.as_str_name(),
            "snapshot": {
                "data": {
                    "blocks": blocks.iter().map(block_to_json).collect::<Vec<_>>(),
                    "details": {
                        "id": root_id,
                        "name": DOC_NAME,
                        "layout": 0,
                    },
                }
            }
        });
        serde_json::to_vec(&doc).expect("fixture snapshot serializes")
    }

    /// Write a minimal directory archive containing a single object snapshot.
    fn write_archive(base: &Path, snapshot_name: &str, snapshot_bytes: &[u8]) -> PathBuf {
        let root = base.join("archive");
        fs::create_dir_all(root.join("objects")).unwrap();
        fs::write(root.join("manifest.json"), b"{}").unwrap();
        fs::write(root.join("objects").join(snapshot_name), snapshot_bytes).unwrap();
        root
    }

    fn pb_archive(base: &Path) -> PathBuf {
        let bytes = pb_snapshot_bytes(fixture_blocks(), fixture_details(DOC_ID));
        write_archive(base, &format!("{DOC_ID}.pb"), &bytes)
    }

    fn json_archive(base: &Path) -> PathBuf {
        let bytes = json_snapshot_bytes(&fixture_blocks(), DOC_ID);
        write_archive(base, &format!("{DOC_ID}.pb.json"), &bytes)
    }

    fn raw_json_archive(base: &Path, layout: i64, include_payload: bool) -> PathBuf {
        let bytes = serde_json::to_vec(&json!({
            "sbType": SmartBlockType::File.as_str_name(),
            "snapshot": {
                "data": {
                    "blocks": [],
                    "details": {
                        "id": DOC_ID,
                        "name": RAW_FILE_NAME,
                        "fileName": RAW_FILE_NAME,
                        "layout": layout,
                    },
                },
            },
        }))
        .expect("raw fixture snapshot serializes");
        let root = write_archive(base, &format!("{DOC_ID}.pb.json"), &bytes);
        if include_payload {
            fs::create_dir_all(root.join("files")).unwrap();
            fs::write(root.join("files").join(RAW_FILE_NAME), RAW_PAYLOAD).unwrap();
        }
        root
    }

    #[test]
    fn convert_fixture_pb_object_to_markdown_contains_headings() {
        let dir = tempfile::tempdir().unwrap();
        let archive = pb_archive(dir.path());
        let markdown = convert_archive_object_pb_to_markdown(&archive, DOC_ID).unwrap();
        assert!(markdown.contains("# Fixture Handbook"), "{markdown}");
        assert!(markdown.contains("## Widget Basics"), "{markdown}");
        assert!(!markdown.is_empty());
    }

    #[test]
    fn convert_fixture_pb_object_closes_lists_with_trailing_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let archive = pb_archive(dir.path());
        let markdown = convert_archive_object_pb_to_markdown(&archive, DOC_ID).unwrap();
        // A marked list item ends with the hard-break marker, and leaving the list
        // emits a standalone marker line before the following paragraph.
        assert!(
            markdown.contains("- Alpha   \n   \nBody paragraph   \n"),
            "{markdown}"
        );
    }

    #[test]
    fn convert_fixture_pb_object_renders_markdown_tables() {
        let dir = tempfile::tempdir().unwrap();
        let archive = pb_archive(dir.path());
        let markdown = convert_archive_object_pb_to_markdown(&archive, DOC_ID).unwrap();
        assert!(markdown.contains("| Widget | Count |"), "{markdown}");
        assert!(markdown.contains(":-"), "{markdown}");
        assert!(markdown.contains("| Gear"), "{markdown}");
        assert!(markdown.contains("| 3"), "{markdown}");
    }

    #[test]
    fn convert_fixture_pb_json_object_matches_pb_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let pb = pb_archive(&dir.path().join("pb"));
        let pb_json = json_archive(&dir.path().join("json"));

        let from_pb = convert_archive_object_to_markdown(&pb, DOC_ID).unwrap();
        let from_json = convert_archive_object_to_markdown(&pb_json, DOC_ID).unwrap();

        assert!(from_json.contains("# Fixture Handbook"), "{from_json}");
        assert!(from_json.contains("## Widget Basics"), "{from_json}");
        assert_eq!(from_pb, from_json);
    }

    #[test]
    fn save_fixture_pb_json_document_writes_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let archive = json_archive(dir.path());
        let dest = dir.path().join("out.md");
        let kind = save_archive_object(&archive, DOC_ID, &dest).unwrap();
        assert_eq!(kind, SavedObjectKind::Markdown);
        let text = fs::read_to_string(&dest).unwrap();
        assert!(text.contains("# Fixture Handbook"), "{text}");
        assert!(text.contains("| Widget | Count |"), "{text}");
    }

    #[test]
    fn save_file_layout_fixture_writes_raw_payload() {
        let dir = tempfile::tempdir().unwrap();
        for layout in 8..=12 {
            let base = dir.path().join(layout.to_string());
            let archive = raw_json_archive(&base, layout, true);
            let dest = base.join("out.pdf");

            let kind = save_archive_object(&archive, DOC_ID, &dest).unwrap();

            assert_eq!(kind, SavedObjectKind::Raw, "layout {layout}");
            assert_eq!(fs::read(&dest).unwrap(), RAW_PAYLOAD, "layout {layout}");
        }
    }

    #[test]
    fn save_file_layout_fixture_classifies_missing_payload() {
        let dir = tempfile::tempdir().unwrap();
        let archive = raw_json_archive(dir.path(), 8, false);
        let dest = dir.path().join("out.pdf");

        let err = save_archive_object(&archive, DOC_ID, &dest)
            .expect_err("a file-layout snapshot without a payload must be rejected");
        let classified = err
            .downcast_ref::<MissingRawPayloadError>()
            .expect("missing raw payload must retain its error classification");

        assert_eq!(classified.object_id(), DOC_ID);
        assert!(!dest.exists());
    }

    #[test]
    fn convert_archive_object_reports_unknown_object_id() {
        let dir = tempfile::tempdir().unwrap();
        let archive = pb_archive(dir.path());
        let err = convert_archive_object_pb_to_markdown(&archive, "bafyreimissingobject")
            .expect_err("unknown object id must be rejected");
        assert!(
            err.to_string().contains("snapshot not found in archive"),
            "{err}"
        );
    }

    #[test]
    fn convert_archive_object_pb_rejects_json_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let archive = json_archive(dir.path());
        let err = convert_archive_object_pb_to_markdown(&archive, DOC_ID)
            .expect_err("pb-only conversion must reject a pb-json snapshot");
        assert!(err.to_string().contains("protobuf snapshots"), "{err}");
    }

    #[test]
    fn convert_archive_object_reports_malformed_pb_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let archive = write_archive(
            dir.path(),
            &format!("{DOC_ID}.pb"),
            b"not a protobuf snapshot",
        );
        let err = convert_archive_object_to_markdown(&archive, DOC_ID)
            .expect_err("malformed protobuf must be rejected");
        assert!(
            err.to_string()
                .contains("failed to decode protobuf snapshot"),
            "{err}"
        );
    }

    #[test]
    fn convert_archive_object_reports_malformed_pb_json_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let archive = write_archive(dir.path(), &format!("{DOC_ID}.pb.json"), b"{ not json");
        let err = convert_archive_object_to_markdown(&archive, DOC_ID)
            .expect_err("malformed pb-json must be rejected");
        assert!(err.to_string().contains("invalid pb-json"), "{err}");
    }

    #[test]
    fn convert_snapshot_bytes_rejects_unsupported_extension() {
        let index = HashMap::new();
        let err = convert_snapshot_bytes_to_markdown("objects/note.txt", b"{}", &index)
            .expect_err("unsupported snapshot extension must be rejected");
        assert!(
            err.to_string().contains("unsupported snapshot format"),
            "{err}"
        );
    }

    #[test]
    fn convert_pb_snapshot_reports_missing_root_block() {
        let bytes = pb_snapshot_bytes(fixture_blocks(), fixture_details("no-such-block"));
        let index = HashMap::new();
        let err = convert_pb_snapshot_to_markdown(&bytes, &index)
            .expect_err("details pointing at an absent root block must be rejected");
        assert!(err.to_string().contains("root block not found"), "{err}");
    }

    #[test]
    fn convert_pb_snapshot_without_blocks_yields_empty_markdown() {
        let bytes = pb_snapshot_bytes(Vec::new(), fixture_details(DOC_ID));
        let index = HashMap::new();
        assert_eq!(
            convert_pb_snapshot_to_markdown(&bytes, &index).unwrap(),
            String::new()
        );
    }

    #[test]
    fn save_archive_object_reports_missing_archive() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent-archive");
        let dest = dir.path().join("out.md");
        let err = save_archive_object(&missing, DOC_ID, &dest)
            .expect_err("a missing archive path must be rejected");
        assert!(
            err.to_string()
                .contains("archive must be a directory or zip file"),
            "{err}"
        );
        assert!(!dest.exists());
    }

    #[test]
    fn build_archive_object_index_skips_unreadable_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let archive = pb_archive(dir.path());
        fs::write(archive.join("objects").join("broken.pb"), b"\xff\xff\xff").unwrap();

        let reader = ArchiveReader::from_path(&archive).unwrap();
        let index = build_archive_object_index(&reader).unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!(
            index.get(DOC_ID).map(|info| info.name.as_str()),
            Some(DOC_NAME)
        );
    }
}
