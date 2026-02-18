use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyback_reader::archive::{ArchiveReader, ArchiveSourceKind};
use anyhow::Result;
use serde_json::Value;

use crate::cli::decode::{
    Manifest, detail_value, format_datetime_display, format_last_modified, parse_expanded_entries,
    parse_snapshot_details_from_pb, parse_snapshot_details_from_pb_json,
    read_manifest_prefer_sidecar, value_as_i64,
};

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ArchiveIndex {
    pub archive_path: String,
    pub source_kind: ArchiveSourceKind,
    pub manifest: Option<Manifest>,
    pub manifest_error: Option<String>,
    pub file_count: usize,
    pub total_bytes: u64,
    pub format: String,
    pub created_at: String,
    pub entries: Vec<ObjectEntry>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ObjectEntry {
    pub id: String,
    pub short_id: String,
    pub type_id: String,
    pub type_short_id: String,
    pub type_name: Option<String>,
    pub type_display: String,
    pub name: String,
    pub sb_type: String,
    pub layout_name: String,
    pub created: String,
    pub modified: String,
    pub created_epoch: i64,
    pub modified_epoch: i64,
    pub size: u64,
    pub size_display: String,
    pub archived: bool,
    pub path: String,
    pub readable: bool,
    pub error: Option<String>,
    pub preview: String,
    pub file_mime: Option<String>,
    pub image_payload_path: Option<String>,
    pub properties_count: usize,
    pub properties: Vec<(String, String)>,
    pub links: Vec<String>,
    pub backlinks: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct SortState {
    pub column: SortColumn,
    pub ascending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Id,
    Type,
    Modified,
}

impl SortColumn {
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Type,
            Self::Type => Self::Modified,
            Self::Modified => Self::Id,
            Self::Id => Self::Name,
        }
    }

    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Id => "Id",
            Self::Type => "Type",
            Self::Modified => "Modified",
        }
    }
}

impl Default for SortState {
    fn default() -> Self {
        Self {
            column: SortColumn::Name,
            ascending: true,
        }
    }
}

impl ArchiveIndex {
    #[allow(
        clippy::case_sensitive_file_extension_comparisons,
        clippy::too_many_lines
    )]
    pub fn build(path: &Path) -> Result<Self> {
        let reader = ArchiveReader::from_path(path)?;
        let source_kind = reader.source();
        let files = reader.list_files()?;
        let (manifest, manifest_error) = read_manifest_prefer_sidecar(path, &reader);
        let file_count = files.len();
        let total_bytes = files
            .iter()
            .fold(0u64, |sum, entry| sum.saturating_add(entry.bytes));

        let expanded = parse_expanded_entries(&reader, &files);

        let file_sizes: HashMap<String, u64> =
            files.iter().map(|f| (f.path.clone(), f.bytes)).collect();

        let mut format = "unknown".to_string();
        let mut seen_pb = false;
        let mut seen_pb_json = false;
        for f in &files {
            let lower = f.path.to_ascii_lowercase();
            if lower.ends_with(".pb.json") {
                seen_pb_json = true;
            } else if lower.ends_with(".pb") {
                seen_pb = true;
            }
        }
        if seen_pb && seen_pb_json {
            format = "mixed".to_string();
        } else if seen_pb {
            format = "pb".to_string();
        } else if seen_pb_json {
            format = "pb-json".to_string();
        }
        if let Some(m) = manifest.as_ref() {
            format.clone_from(&m.format);
        }

        let created_at = manifest.as_ref().map_or_else(
            || "-".to_string(),
            |m| {
                m.created_at_display
                    .clone()
                    .or_else(|| format_datetime_display(&m.created_at))
                    .unwrap_or_else(|| m.created_at.clone())
            },
        );

        let details_by_path: HashMap<String, serde_json::Map<String, Value>> = expanded
            .iter()
            .filter_map(|entry| {
                load_details_for_path(&reader, &entry.path)
                    .ok()
                    .flatten()
                    .map(|details| (entry.path.clone(), details))
            })
            .collect();

        let name_by_id: HashMap<String, String> = expanded
            .iter()
            .filter_map(|entry| {
                let id = entry.id.as_ref()?;
                let name = entry.name.as_ref()?;
                if name.trim().is_empty() {
                    None
                } else {
                    Some((id.clone(), name.clone()))
                }
            })
            .collect();

        let known_ids: HashSet<String> = expanded.iter().filter_map(|e| e.id.clone()).collect();

        let mut entries = Vec::with_capacity(expanded.len());
        for entry in &expanded {
            let readable = entry.status == "ok";
            let error = if readable {
                None
            } else {
                entry.unreadable_reason.clone()
            };
            let details = details_by_path.get(&entry.path);
            let id = entry.id.clone().unwrap_or_default();
            let short_id = if id.len() >= 5 {
                id[id.len() - 5..].to_string()
            } else {
                id.clone()
            };
            let layout_name = entry.layout_name.clone().unwrap_or_else(|| "-".to_string());
            let sb_type = entry.sb_type.clone().unwrap_or_else(|| "-".to_string());
            let (type_id, type_short_id, type_name, type_display) =
                resolve_type_info(details, &layout_name, &sb_type, &name_by_id);
            let name = entry
                .name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "(untitled)".to_string());
            let created_epoch = entry
                .created_date
                .as_ref()
                .and_then(value_as_i64)
                .unwrap_or(0);
            let modified_epoch = entry
                .last_modified_date
                .as_ref()
                .and_then(value_as_i64)
                .unwrap_or(0);
            let created = format_last_modified(entry.created_date.as_ref())
                .unwrap_or_else(|| "-".to_string());
            let modified = format_last_modified(entry.last_modified_date.as_ref())
                .unwrap_or_else(|| "-".to_string());
            let size = file_sizes.get(&entry.path).copied().unwrap_or(0);
            let size_display = format_size(size);
            let archived = entry.archived.unwrap_or(false);
            let properties =
                details.map_or_else(Vec::new, |d| collect_user_properties(d, &name_by_id));
            let preview = details.map_or_else(
                || "(no preview available)".to_string(),
                |d| build_preview(d, &name),
            );
            let file_mime = details
                .and_then(|d| detail_value(d, "fileMimeType"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let image_payload_path = details.and_then(|d| infer_image_payload_path(&id, d, &files));
            let links = details
                .map(|d| extract_links(d, &known_ids, &id))
                .unwrap_or_default();
            let properties_count = properties.len();

            entries.push(ObjectEntry {
                id,
                short_id,
                type_id,
                type_short_id,
                type_name,
                type_display,
                name,
                sb_type,
                layout_name,
                created,
                modified,
                created_epoch,
                modified_epoch,
                size,
                size_display,
                archived,
                path: entry.path.clone(),
                readable,
                error,
                preview,
                file_mime,
                image_payload_path,
                properties_count,
                properties,
                links,
                backlinks: Vec::new(),
            });
        }

        let mut backlinks: HashMap<String, BTreeSet<String>> = HashMap::new();
        for entry in &entries {
            for target in &entry.links {
                backlinks
                    .entry(target.clone())
                    .or_default()
                    .insert(entry.id.clone());
            }
        }
        for entry in &mut entries {
            entry.backlinks = backlinks
                .remove(&entry.id)
                .map(|ids| ids.into_iter().collect())
                .unwrap_or_default();
        }

        let mut index = Self {
            archive_path: path.display().to_string(),
            source_kind,
            manifest,
            manifest_error,
            file_count,
            total_bytes,
            format,
            created_at,
            entries,
        };
        index.sort(SortState::default());
        Ok(index)
    }

    pub fn sort(&mut self, sort: SortState) {
        let asc = sort.ascending;
        match sort.column {
            SortColumn::Name => self.entries.sort_by(|a, b| {
                let cmp = a.name.to_lowercase().cmp(&b.name.to_lowercase());
                if asc { cmp } else { cmp.reverse() }
            }),
            SortColumn::Id => self.entries.sort_by(|a, b| {
                let cmp = a.id.cmp(&b.id);
                if asc { cmp } else { cmp.reverse() }
            }),
            SortColumn::Type => self.entries.sort_by(|a, b| {
                let cmp = a.type_display.cmp(&b.type_display);
                if asc { cmp } else { cmp.reverse() }
            }),
            SortColumn::Modified => self.entries.sort_by(|a, b| {
                let cmp = a.modified_epoch.cmp(&b.modified_epoch);
                if asc { cmp } else { cmp.reverse() }
            }),
        }
    }
}

fn load_details_for_path(
    reader: &ArchiveReader,
    path: &str,
) -> Result<Option<serde_json::Map<String, Value>>> {
    let lower = path.to_ascii_lowercase();
    // lower is already lowercased, so extension checks are case-insensitive
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    if !(lower.ends_with(".pb") || lower.ends_with(".pb.json")) {
        return Ok(None);
    }
    let bytes = reader.read_bytes(path)?;
    let parsed = if lower.ends_with(".pb.json") {
        parse_snapshot_details_from_pb_json(&bytes)
    } else {
        parse_snapshot_details_from_pb(&bytes)
    };
    Ok(parsed.ok().map(|(_, details)| details))
}

fn infer_image_payload_path(
    object_id: &str,
    details: &serde_json::Map<String, Value>,
    files: &[anyback_reader::archive::ArchiveFileEntry],
) -> Option<String> {
    let mime = detail_value(details, "fileMimeType")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)?;
    if !mime.starts_with("image/") {
        return None;
    }

    let mut tokens = Vec::new();
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
    ] {
        if let Some(value) = detail_value(details, key).and_then(Value::as_str) {
            let token = value.trim().to_ascii_lowercase();
            if !token.is_empty() {
                tokens.push(token);
            }
        }
    }
    if let Some(ext) = detail_value(details, "fileExt").and_then(Value::as_str) {
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
        if is_image_path(&path_lc) {
            score += 40;
        }
        for token in &tokens {
            if token.len() >= 6 && path_lc.contains(token) {
                score += 20;
            }
        }

        if score > 0 && best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((&file.path, score));
        }
    }

    best.map(|(path, _)| path.to_string())
}

fn is_image_path(path: &str) -> bool {
    [
        ".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp", ".tif", ".tiff", ".ico", ".avif", ".heic",
    ]
    .iter()
    .any(|ext| path.ends_with(ext))
}

fn resolve_type_info(
    details: Option<&serde_json::Map<String, Value>>,
    layout_name: &str,
    sb_type: &str,
    type_name_by_id: &HashMap<String, String>,
) -> (String, String, Option<String>, String) {
    let mut type_id = String::new();
    let mut type_name: Option<String> = None;
    let mut type_display: Option<String> = None;

    if let Some(type_value) = details.and_then(|map| map.get("type")) {
        if let Some(s) = type_value.as_str() {
            if looks_like_object_id(s) {
                type_id = s.to_string();
            } else if !s.is_empty() {
                type_display = Some(s.to_string());
            }
        } else if let Some(obj) = type_value.as_object() {
            if let Some(id) = obj.get("id").and_then(Value::as_str)
                && looks_like_object_id(id)
            {
                type_id = id.to_string();
            }
            if type_display.is_none() {
                type_display = obj
                    .get("key")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string);
            }
        }
    }

    if !type_id.is_empty() {
        type_name = type_name_by_id.get(&type_id).cloned();
        if type_display.is_none() {
            type_display = type_name.clone().or_else(|| Some(short_id(&type_id)));
        }
    }

    let display = type_display
        .or_else(|| {
            if layout_name == "-" {
                None
            } else {
                Some(layout_name.to_string())
            }
        })
        .or_else(|| {
            if sb_type == "-" {
                None
            } else {
                Some(sb_type.to_string())
            }
        })
        .unwrap_or_else(|| "-".to_string());

    let short = if type_id.is_empty() {
        String::new()
    } else {
        short_id(&type_id)
    };
    (type_id, short, type_name, display)
}

fn short_id(id: &str) -> String {
    if id.len() >= 5 {
        id[id.len() - 5..].to_string()
    } else {
        id.to_string()
    }
}

fn build_preview(details: &serde_json::Map<String, Value>, fallback_name: &str) -> String {
    const MAX_LINES: usize = 512;
    let mut lines = Vec::<String>::new();

    if let Some(name) = details.get("name").and_then(Value::as_str)
        && !name.trim().is_empty()
    {
        push_preview_lines(&mut lines, name);
    }

    for key in ["description", "snippet", "text", "body"] {
        if let Some(text) = details.get(key).and_then(Value::as_str) {
            push_preview_lines(&mut lines, text);
        }
    }

    for key in ["blocks", "details", "fields", "content"] {
        if let Some(value) = details.get(key) {
            collect_preview_strings(value, &mut lines, 0);
        }
    }

    if lines.is_empty() {
        lines.push(fallback_name.to_string());
    }

    let mut uniq = BTreeSet::new();
    let mut compact = Vec::new();
    for line in lines {
        let normalized = line.trim();
        if normalized.is_empty() {
            continue;
        }
        if uniq.insert(normalized.to_string()) {
            compact.push(normalized.to_string());
            if compact.len() >= MAX_LINES {
                break;
            }
        }
    }

    if compact.is_empty() {
        "(no preview available)".to_string()
    } else {
        compact.join("\n")
    }
}

fn collect_user_properties(
    details: &serde_json::Map<String, Value>,
    name_by_id: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (key, value) in details {
        if is_system_property(key) {
            continue;
        }
        if let Some(text) = property_value_text(value, name_by_id) {
            out.push((key.clone(), text));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn is_system_property(key: &str) -> bool {
    matches!(
        key,
        "id" | "name"
            | "type"
            | "layout"
            | "layoutName"
            | "sbType"
            | "createdDate"
            | "lastModifiedDate"
            | "isArchived"
            | "archived"
            | "description"
            | "snippet"
            | "text"
            | "body"
            | "blocks"
            | "details"
            | "fields"
            | "content"
            | "sourceFile"
    )
}

fn property_value_text(value: &Value, name_by_id: &HashMap<String, String>) -> Option<String> {
    match value {
        Value::String(s) => {
            let text = s.trim();
            if text.is_empty() {
                None
            } else if looks_like_object_id(text) {
                Some(format_object_ref(text, name_by_id))
            } else {
                Some(text.to_string())
            }
        }
        Value::Bool(v) => Some(v.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(items) => {
            let values: Vec<String> = items
                .iter()
                .filter_map(|item| property_item_text(item, name_by_id))
                .collect();
            (!values.is_empty()).then(|| values.join(", "))
        }
        Value::Object(map) => {
            if let Some(id) = map.get("id").and_then(Value::as_str) {
                let trimmed = id.trim();
                if looks_like_object_id(trimmed) {
                    return Some(format_object_ref(trimmed, name_by_id));
                }
            }
            for key in ["name", "title", "key", "id", "value"] {
                if let Some(text) = map
                    .get(key)
                    .and_then(|v| property_value_text(v, name_by_id))
                    && !text.is_empty()
                {
                    return Some(text);
                }
            }
            serde_json::to_string(map).ok()
        }
        Value::Null => None,
    }
}

fn property_item_text(value: &Value, name_by_id: &HashMap<String, String>) -> Option<String> {
    match value {
        Value::Array(items) => {
            let values: Vec<String> = items
                .iter()
                .filter_map(|item| property_item_text(item, name_by_id))
                .collect();
            (!values.is_empty()).then(|| values.join(", "))
        }
        _ => property_value_text(value, name_by_id),
    }
}

fn format_object_ref(object_id: &str, name_by_id: &HashMap<String, String>) -> String {
    if let Some(name) = name_by_id.get(object_id)
        && !name.trim().is_empty()
    {
        return format!("{name} ({object_id})");
    }
    object_id.to_string()
}

fn collect_preview_strings(value: &Value, out: &mut Vec<String>, depth: usize) {
    if depth > 6 {
        return;
    }
    match value {
        Value::String(s) => {
            if s.len() > 2 && !looks_like_object_id(s) {
                push_preview_lines(out, s);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_preview_strings(item, out, depth + 1);
            }
        }
        Value::Object(map) => {
            for nested in map.values() {
                collect_preview_strings(nested, out, depth + 1);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn push_preview_lines(out: &mut Vec<String>, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    for line in trimmed.lines() {
        let line = line.trim_end();
        if !line.trim().is_empty() {
            out.push(line.to_string());
        }
    }
}

fn extract_links(
    details: &serde_json::Map<String, Value>,
    known_ids: &HashSet<String>,
    self_id: &str,
) -> Vec<String> {
    let mut links = BTreeSet::<String>::new();
    for value in details.values() {
        collect_link_candidates(value, known_ids, self_id, &mut links, 0);
    }
    links.into_iter().collect()
}

fn collect_link_candidates(
    value: &Value,
    known_ids: &HashSet<String>,
    self_id: &str,
    out: &mut BTreeSet<String>,
    depth: usize,
) {
    if depth > 6 {
        return;
    }
    match value {
        Value::String(s) => {
            if s != self_id && known_ids.contains(s) {
                out.insert(s.clone());
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_link_candidates(item, known_ids, self_id, out, depth + 1);
            }
        }
        Value::Object(map) => {
            for nested in map.values() {
                collect_link_candidates(nested, known_ids, self_id, out, depth + 1);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn looks_like_object_id(value: &str) -> bool {
    value.starts_with("baf") && value.chars().all(|c| c.is_ascii_alphanumeric())
}

#[allow(clippy::cast_precision_loss)]
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyback_reader::archive::ArchiveFileEntry;

    #[test]
    fn extract_links_detects_known_ids_only() {
        let known_ids = HashSet::from([
            "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "bafyreidbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
        ]);
        let self_id = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let details = serde_json::json!({
            "name": "Source",
            "links": [
                "bafyreidbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "not-an-id"
            ],
            "nested": {
                "target": "bafyreidbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "self": "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }
        });
        let map = details.as_object().unwrap();
        let links = extract_links(map, &known_ids, self_id);
        assert_eq!(
            links,
            vec!["bafyreidbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()]
        );
    }

    #[test]
    fn build_preview_is_stable_and_compact() {
        let details = serde_json::json!({
            "name": "Example",
            "description": "A long description",
            "blocks": [
                {"text": "Line one"},
                {"text": "Line two"},
                {"text": "Line two"}
            ]
        });
        let map = details.as_object().unwrap();
        let preview = build_preview(map, "fallback");
        assert!(preview.contains("Example"));
        assert!(preview.contains("Line one"));
        assert!(!preview.contains("fallback"));
    }

    #[test]
    fn build_preview_preserves_markdown_lines_without_truncating_headings() {
        let details = serde_json::json!({
            "name": "Widgets & Sidebar",
            "body": "# Widgets & Sidebar\n\n## Section A\nLine one\nLine two\n\n### Section B\nFinal paragraph"
        });
        let map = details.as_object().unwrap();
        let preview = build_preview(map, "fallback");
        assert!(preview.contains("# Widgets & Sidebar"));
        assert!(preview.contains("## Section A"));
        assert!(preview.contains("### Section B"));
        assert!(preview.contains("Final paragraph"));
    }

    #[test]
    fn infer_image_payload_path_prefers_files_match() {
        let details = serde_json::json!({
            "id": "bafyreifileobj",
            "name": "photo.png",
            "fileMimeType": "image/png",
            "source": "QmImageHash1234"
        });
        let files = vec![
            ArchiveFileEntry {
                path: "objects/bafyreifileobj.pb".to_string(),
                bytes: 123,
            },
            ArchiveFileEntry {
                path: "files/QmImageHash1234".to_string(),
                bytes: 2048,
            },
            ArchiveFileEntry {
                path: "files/something-else.bin".to_string(),
                bytes: 2048,
            },
        ];
        let map = details.as_object().unwrap();
        let selected = infer_image_payload_path("bafyreifileobj", map, &files);
        assert_eq!(selected.as_deref(), Some("files/QmImageHash1234"));
    }

    #[test]
    fn collect_user_properties_includes_array_and_object_values() {
        let details = serde_json::json!({
            "name": "Example",
            "layout": 0,
            "tag": [{"name": "alpha"}, {"name": "beta"}],
            "category": {"name": "ops"},
            "rating": 5
        });
        let map = details.as_object().unwrap();
        let props = collect_user_properties(map, &HashMap::new());

        assert!(props.contains(&("tag".to_string(), "alpha, beta".to_string())));
        assert!(props.contains(&("category".to_string(), "ops".to_string())));
        assert!(props.contains(&("rating".to_string(), "5".to_string())));
    }

    #[test]
    fn collect_user_properties_resolves_object_ids_to_name_and_id() {
        let status_id = "bafyreistatus111111111111111111111111111111111111111111111111";
        let tag_a = "bafyreitagaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let tag_b = "bafyreitagbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        let details = serde_json::json!({
            "name": "Example",
            "status": status_id,
            "tags": [tag_a, tag_b]
        });
        let map = details.as_object().unwrap();
        let name_by_id = HashMap::from([
            (status_id.to_string(), "Open".to_string()),
            (tag_a.to_string(), "Homework".to_string()),
            (tag_b.to_string(), "Geography".to_string()),
        ]);

        let props = collect_user_properties(map, &name_by_id);

        assert!(props.contains(&("status".to_string(), format!("Open ({status_id})"))));
        assert!(props.contains(&(
            "tags".to_string(),
            format!("Homework ({tag_a}), Geography ({tag_b})")
        )));
    }
}
