use anyback_reader::archive::{
    ArchiveFileEntry, ArchiveReader, infer_object_id_from_snapshot_path,
};
use anyhow::{Context, Result, anyhow};
use anytype_rpc::anytype::SnapshotWithType;
use chrono::{DateTime, FixedOffset, Utc};
use prost::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const MANIFEST_NAME: &str = "manifest.json";
pub const MANIFEST_SIDECAR_SUFFIX: &str = ".manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectDescriptor {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_id: Option<String>,
    pub name: Option<String>,
    pub r#type: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub tool: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_display: Option<String>,
    pub source_space_id: String,
    pub source_space_name: String,
    pub format: String,
    pub object_count: usize,
    pub objects: Vec<ObjectDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectImportError {
    pub id: String,
    pub name: Option<String>,
    pub r#type: Option<String>,
    pub last_modified: Option<String>,
    pub error_code: String,
    pub message: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportReport {
    pub archive: String,
    pub space_id: String,
    pub attempted: usize,
    pub imported: usize,
    pub failed: usize,
    pub success: Vec<ObjectDescriptor>,
    pub errors: Vec<ObjectImportError>,
    pub summary: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_progress: Option<ImportEventProgressReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportEventProgressReport {
    pub processes_started: usize,
    pub processes_done: usize,
    pub process_updates: usize,
    pub import_finish_events: usize,
    pub import_finish_objects: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_process_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_process_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_done: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_total: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_process_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestSummary {
    pub schema_version: u32,
    pub tool: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at_display: Option<String>,
    pub source_space_id: String,
    pub source_space_name: String,
    pub format: String,
    pub object_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_display: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until_display: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExpandedSnapshotEntry {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreadable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sb_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_type: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_date: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified_date: Option<Value>,
}

pub fn proto_value_to_json(value: &prost_types::Value) -> Value {
    use prost_types::value::Kind;
    match value.kind.as_ref() {
        None | Some(Kind::NullValue(_)) => Value::Null,
        Some(Kind::NumberValue(n)) => {
            serde_json::Number::from_f64(*n).map_or(Value::Null, Value::Number)
        }
        Some(Kind::StringValue(s)) => Value::String(s.clone()),
        Some(Kind::BoolValue(b)) => Value::Bool(*b),
        Some(Kind::StructValue(s)) => {
            let mut map = serde_json::Map::<String, Value>::new();
            for (k, v) in &s.fields {
                map.insert(k.clone(), proto_value_to_json(v));
            }
            Value::Object(map)
        }
        Some(Kind::ListValue(list)) => {
            Value::Array(list.values.iter().map(proto_value_to_json).collect())
        }
    }
}

pub fn normalize_jsonpb_value(value: &Value) -> Value {
    let Some(obj) = value.as_object() else {
        return value.clone();
    };
    if let Some(v) = obj.get("stringValue").and_then(Value::as_str) {
        return Value::String(v.to_string());
    }
    if let Some(v) = obj.get("numberValue").and_then(Value::as_f64) {
        return serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number);
    }
    if let Some(v) = obj.get("boolValue").and_then(Value::as_bool) {
        return Value::Bool(v);
    }
    if obj.get("nullValue").is_some() {
        return Value::Null;
    }
    if let Some(fields) = obj
        .get("structValue")
        .and_then(Value::as_object)
        .and_then(|o| o.get("fields"))
        .and_then(Value::as_object)
    {
        let mut out = serde_json::Map::<String, Value>::new();
        for (k, v) in fields {
            out.insert(k.clone(), normalize_jsonpb_value(v));
        }
        return Value::Object(out);
    }
    if let Some(values) = obj
        .get("listValue")
        .and_then(Value::as_object)
        .and_then(|o| o.get("values"))
        .and_then(Value::as_array)
    {
        return Value::Array(values.iter().map(normalize_jsonpb_value).collect());
    }
    value.clone()
}

#[allow(clippy::cast_possible_truncation)]
pub fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|n| n as i64))
        .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()))
}

pub fn value_as_bool(value: &Value) -> Option<bool> {
    value
        .as_bool()
        .or_else(|| value.as_i64().map(|n| n != 0))
        .or_else(|| {
            value.as_str().and_then(|s| match s {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            })
        })
}

pub fn derive_layout_name(layout: Option<i64>) -> Option<String> {
    let layout = layout?;
    let parsed =
        anytype_rpc::model::object_type::Layout::try_from(i32::try_from(layout).ok()?).ok()?;
    Some(parsed.as_str_name().to_string())
}

pub fn parse_snapshot_details_from_pb(
    bytes: &[u8],
) -> Result<(Option<String>, serde_json::Map<String, Value>)> {
    let snapshot = SnapshotWithType::decode(bytes).context("failed to decode protobuf snapshot")?;
    let sb_type = anytype_rpc::model::SmartBlockType::try_from(snapshot.sb_type)
        .ok()
        .map(|s| s.as_str_name().to_string());
    let data = snapshot
        .snapshot
        .and_then(|s| s.data)
        .ok_or_else(|| anyhow!("snapshot payload missing data"))?;
    let details = data
        .details
        .ok_or_else(|| anyhow!("snapshot payload missing details"))?;
    let mut map = serde_json::Map::new();
    for (k, v) in &details.fields {
        map.insert(k.clone(), proto_value_to_json(v));
    }
    Ok((sb_type, map))
}

pub fn parse_snapshot_details_from_pb_json(
    bytes: &[u8],
) -> Result<(Option<String>, serde_json::Map<String, Value>)> {
    let root: Value = serde_json::from_slice(bytes).context("invalid pb-json")?;
    let sb_type = root
        .get("sbType")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            root.get("sbType")
                .and_then(Value::as_i64)
                .and_then(|n| i32::try_from(n).ok())
                .and_then(|n| anytype_rpc::model::SmartBlockType::try_from(n).ok())
                .map(|s| s.as_str_name().to_string())
        });
    let details = root
        .get("snapshot")
        .and_then(|v| v.get("data"))
        .and_then(|v| v.get("details"))
        .ok_or_else(|| anyhow!("pb-json snapshot missing snapshot.data.details"))?;
    let fields = if let Some(fields) = details.get("fields").and_then(Value::as_object) {
        fields
    } else {
        details
            .as_object()
            .ok_or_else(|| anyhow!("pb-json snapshot details is not an object"))?
    };
    let mut map = serde_json::Map::new();
    for (k, v) in fields {
        map.insert(k.clone(), normalize_jsonpb_value(v));
    }
    Ok((sb_type, map))
}

pub fn detail_value<'a>(
    details: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Option<&'a Value> {
    details.get(key)
}

pub fn build_expanded_entry_from_details(
    path: &str,
    id_from_path: Option<String>,
    sb_type: Option<String>,
    details: &serde_json::Map<String, Value>,
) -> ExpandedSnapshotEntry {
    let id = detail_value(details, "id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or(id_from_path);
    let name = detail_value(details, "name")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let object_type = detail_value(details, "type").cloned();
    let layout = detail_value(details, "layout")
        .and_then(value_as_i64)
        .or_else(|| detail_value(details, "resolvedLayout").and_then(value_as_i64));
    let layout_name = derive_layout_name(layout);
    let archived = detail_value(details, "isArchived").and_then(value_as_bool);
    let created_date = detail_value(details, "createdDate").cloned();
    let last_modified_date = detail_value(details, "lastModifiedDate").cloned();
    ExpandedSnapshotEntry {
        path: path.to_string(),
        id,
        status: "ok".to_string(),
        unreadable_reason: None,
        sb_type,
        name,
        object_type,
        layout,
        layout_name,
        archived,
        created_date,
        last_modified_date,
    }
}

pub fn parse_expanded_entries(
    reader: &ArchiveReader,
    files: &[ArchiveFileEntry],
) -> Vec<ExpandedSnapshotEntry> {
    let mut out = Vec::new();
    for file in files {
        let lower = file.path.to_ascii_lowercase();
        // lower is already lowercased, so extension checks are case-insensitive
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        if !(lower.ends_with(".pb") || lower.ends_with(".pb.json")) {
            continue;
        }
        let id_from_path = infer_object_id_from_snapshot_path(&file.path);
        let bytes = match reader.read_bytes(&file.path) {
            Ok(bytes) => bytes,
            Err(err) => {
                out.push(ExpandedSnapshotEntry {
                    path: file.path.clone(),
                    id: id_from_path,
                    status: "unreadable".to_string(),
                    unreadable_reason: Some(format!("failed to read file: {err}")),
                    sb_type: None,
                    name: None,
                    object_type: None,
                    layout: None,
                    layout_name: None,
                    archived: None,
                    created_date: None,
                    last_modified_date: None,
                });
                continue;
            }
        };

        let parsed = if lower.ends_with(".pb.json") {
            parse_snapshot_details_from_pb_json(&bytes)
        } else {
            parse_snapshot_details_from_pb(&bytes)
        };
        match parsed {
            Ok((sb_type, details)) => {
                out.push(build_expanded_entry_from_details(
                    &file.path,
                    id_from_path,
                    sb_type,
                    &details,
                ));
            }
            Err(err) => {
                out.push(ExpandedSnapshotEntry {
                    path: file.path.clone(),
                    id: id_from_path,
                    status: "unreadable".to_string(),
                    unreadable_reason: Some(err.to_string()),
                    sb_type: None,
                    name: None,
                    object_type: None,
                    layout: None,
                    layout_name: None,
                    archived: None,
                    created_date: None,
                    last_modified_date: None,
                });
            }
        }
    }
    out
}

pub fn read_manifest_from_reader(reader: &ArchiveReader) -> (Option<Manifest>, Option<String>) {
    let Ok(Some(manifest_bytes)) = reader.read_bytes_if_exists(MANIFEST_NAME) else {
        return (None, None);
    };
    match serde_json::from_slice::<Manifest>(&manifest_bytes) {
        Ok(manifest) => (Some(manifest), None),
        Err(err) => (None, Some(format!("invalid manifest json: {err}"))),
    }
}

pub fn manifest_sidecar_path(archive_path: &Path) -> PathBuf {
    let base_name = archive_path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("archive");
    let sidecar_name = format!("{base_name}{MANIFEST_SIDECAR_SUFFIX}");
    archive_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(sidecar_name)
}

pub fn read_manifest_from_sidecar(archive_path: &Path) -> (Option<Manifest>, Option<String>) {
    let sidecar = manifest_sidecar_path(archive_path);
    let bytes = match std::fs::read(&sidecar) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return (None, None),
        Err(err) => {
            return (
                None,
                Some(format!(
                    "failed to read sidecar manifest {}: {err}",
                    sidecar.display()
                )),
            );
        }
    };
    match serde_json::from_slice::<Manifest>(&bytes) {
        Ok(manifest) => (Some(manifest), None),
        Err(err) => (
            None,
            Some(format!(
                "invalid sidecar manifest {}: {err}",
                sidecar.display()
            )),
        ),
    }
}

pub fn read_manifest_prefer_sidecar(
    archive_path: &Path,
    reader: &ArchiveReader,
) -> (Option<Manifest>, Option<String>) {
    let (sidecar_manifest, sidecar_error) = read_manifest_from_sidecar(archive_path);
    if sidecar_manifest.is_some() || sidecar_error.is_some() {
        return (sidecar_manifest, sidecar_error);
    }
    read_manifest_from_reader(reader)
}

fn offset_label(offset: FixedOffset) -> String {
    let seconds = offset.local_minus_utc();
    if seconds == 0 {
        return "UTC".to_string();
    }
    let sign = if seconds >= 0 { '+' } else { '-' };
    let abs = seconds.unsigned_abs();
    let hours = abs / 3600;
    let minutes = (abs % 3600) / 60;
    format!("{sign}{hours:02}:{minutes:02}")
}

fn format_datetime_with_tz(dt: DateTime<FixedOffset>) -> String {
    format!(
        "{} {}",
        dt.format("%Y-%m-%d %H:%M:%S"),
        offset_label(*dt.offset())
    )
}

fn format_utc_datetime_with_tz(dt: DateTime<Utc>) -> String {
    format!("{} UTC", dt.format("%Y-%m-%d %H:%M:%S"))
}

pub fn format_datetime_display(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(format_datetime_with_tz)
}

pub fn format_last_modified(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        if let Ok(parsed) = DateTime::parse_from_rfc3339(text) {
            return Some(format_datetime_with_tz(parsed));
        }
        return Some(text.to_string());
    }
    if let Some(raw) = value.as_i64() {
        let dt = if raw > 2_000_000_000_000 {
            DateTime::<Utc>::from_timestamp_millis(raw)
        } else {
            DateTime::<Utc>::from_timestamp(raw, 0)
        };
        return dt.map(format_utc_datetime_with_tz);
    }
    #[allow(clippy::cast_possible_truncation)]
    if let Some(raw) = value.as_f64() {
        return format_last_modified(Some(&Value::Number(serde_json::Number::from(raw as i64))));
    }
    Some(value.to_string())
}

pub fn manifest_summary(manifest: &Manifest) -> ManifestSummary {
    ManifestSummary {
        schema_version: manifest.schema_version,
        tool: manifest.tool.clone(),
        created_at: manifest.created_at.clone(),
        created_at_display: manifest.created_at_display.clone(),
        source_space_id: manifest.source_space_id.clone(),
        source_space_name: manifest.source_space_name.clone(),
        format: manifest.format.clone(),
        object_count: manifest.object_count,
        mode: manifest.mode.clone(),
        since: manifest.since.clone(),
        since_display: manifest.since_display.clone(),
        until: manifest.until.clone(),
        until_display: manifest.until_display.clone(),
        type_ids: manifest.type_ids.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn parse_pb_json_accepts_details_fields_wrapper() {
        let json = serde_json::json!({
            "sbType": "Page",
            "snapshot": {
                "data": {
                    "details": {
                        "fields": {
                            "name": {"stringValue": "Wrapped"},
                            "layout": {"numberValue": 1.0},
                            "isArchived": {"boolValue": false}
                        }
                    }
                }
            }
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        let (sb_type, details) = parse_snapshot_details_from_pb_json(&bytes).unwrap();
        assert_eq!(sb_type.as_deref(), Some("Page"));
        assert_eq!(details.get("name").and_then(Value::as_str), Some("Wrapped"));
        assert_eq!(details.get("layout").and_then(value_as_i64), Some(1));
        assert_eq!(
            details.get("isArchived").and_then(value_as_bool),
            Some(false)
        );
    }

    #[test]
    fn parse_pb_json_accepts_direct_details_object() {
        let json = serde_json::json!({
            "sbType": "Page",
            "snapshot": {
                "data": {
                    "details": {
                        "id": "bafyreitest",
                        "name": "Templates",
                        "layout": 0,
                        "isArchived": false,
                        "type": "bafyreitype"
                    }
                }
            }
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        let (sb_type, details) = parse_snapshot_details_from_pb_json(&bytes).unwrap();
        assert_eq!(sb_type.as_deref(), Some("Page"));
        assert_eq!(
            details.get("name").and_then(Value::as_str),
            Some("Templates")
        );
        assert_eq!(details.get("layout").and_then(value_as_i64), Some(0));
        assert_eq!(
            details.get("isArchived").and_then(value_as_bool),
            Some(false)
        );
        assert_eq!(
            details.get("type").and_then(Value::as_str),
            Some("bafyreitype")
        );
    }

    #[test]
    fn manifest_sidecar_path_uses_sibling_name() {
        let zip_path = Path::new("/tmp/archive.zip");
        assert_eq!(
            manifest_sidecar_path(zip_path),
            Path::new("/tmp/archive.zip.manifest.json")
        );

        let dir_path = Path::new("/tmp/archive-dir");
        assert_eq!(
            manifest_sidecar_path(dir_path),
            Path::new("/tmp/archive-dir.manifest.json")
        );
    }

    #[test]
    fn read_manifest_prefers_sidecar_over_archive_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let archive_dir = temp.path().join("backup-dir");
        fs::create_dir_all(&archive_dir).unwrap();

        fs::write(archive_dir.join(MANIFEST_NAME), b"{not-json").unwrap();

        let sidecar = manifest_sidecar_path(&archive_dir);
        let manifest = Manifest {
            schema_version: 1,
            tool: "anyback/test".to_string(),
            created_at: "2026-02-16T00:00:00Z".to_string(),
            created_at_display: None,
            source_space_id: "space-id".to_string(),
            source_space_name: "space-name".to_string(),
            format: "pb".to_string(),
            object_count: 0,
            objects: Vec::new(),
            mode: Some("full".to_string()),
            since: None,
            since_display: None,
            until: None,
            until_display: None,
            type_ids: None,
        };
        fs::write(&sidecar, serde_json::to_vec(&manifest).unwrap()).unwrap();

        let reader = ArchiveReader::from_path(&archive_dir).unwrap();
        let (resolved, err) = read_manifest_prefer_sidecar(&archive_dir, &reader);
        assert!(err.is_none());
        assert_eq!(resolved.unwrap().tool, "anyback/test");
    }
}
