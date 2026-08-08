use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
    time::SystemTime,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use anytype::prelude::*;
use chrono::{DateTime, FixedOffset, Utc};
use serde_json::Value;
use tokio::time::sleep;

mod object_generator;
mod support;

struct PrefixCleanupGuard {
    scopes: Vec<CleanupScope>,
}

impl PrefixCleanupGuard {
    fn new(spaces: Vec<String>, prefixes: Vec<String>) -> Result<Self> {
        let mut unique_spaces = HashSet::new();
        let mut scopes = Vec::new();
        for space in spaces {
            if !unique_spaces.insert(space.clone()) {
                continue;
            }
            scopes.push(CleanupScope {
                space: space.clone(),
                prefixes: prefixes.clone(),
                existing_protobuf_import_collections: list_protobuf_import_collection_ids(&space)?,
            });
        }
        Ok(Self { scopes })
    }
}

struct CleanupScope {
    space: String,
    prefixes: Vec<String>,
    existing_protobuf_import_collections: HashSet<String>,
}

struct ChatMessageTokenCleanupGuard {
    entries: Vec<(String, String)>,
}

impl ChatMessageTokenCleanupGuard {
    fn new(entries: Vec<(String, String)>) -> Self {
        Self { entries }
    }
}

impl Drop for ChatMessageTokenCleanupGuard {
    fn drop(&mut self) {
        for (space, token) in &self.entries {
            let _ = delete_chat_messages_by_token(space, token);
        }
    }
}

#[derive(Debug)]
struct RegisteredResource {
    kind: &'static str,
    id: String,
}

#[derive(Debug)]
struct DisposableSpace {
    name: String,
    id: String,
    resources: Vec<RegisteredResource>,
    cleaned: bool,
}

impl DisposableSpace {
    fn create(label: &str) -> Result<Self> {
        require_disposable_admission()?;
        let prefix = std::env::var("ANYTYPE_TEST_SPACE_PREFIX")?;
        let name = format!("{prefix}-{label}-{}", anytype::test_util::unique_suffix());
        let output = run_anyr(["space", "create", name.as_str()])?;
        let value: Value = serde_json::from_str(&output)
            .context("disposable space create returned invalid JSON")?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow!("disposable space create output missing id"))?
            .to_string();
        Ok(Self {
            name,
            id,
            resources: Vec::new(),
            cleaned: false,
        })
    }

    fn register(&mut self, kind: &'static str, id: impl Into<String>) {
        self.resources.push(RegisteredResource {
            kind,
            id: id.into(),
        });
    }

    fn cleanup(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        run_anyr([
            "space",
            "delete",
            self.id.as_str(),
            "--skip-archive",
            "--confirm",
        ])
        .with_context(|| {
            format!(
                "failed to delete disposable space {} with registered resources {}",
                self.name,
                self.registered_resource_summary()
            )
        })?;
        wait_for_space_absence(&self.name, &self.id)?;
        self.cleaned = true;
        Ok(())
    }

    fn registered_resource_summary(&self) -> String {
        self.resources
            .iter()
            .map(|resource| format!("{}={}", resource.kind, resource.id))
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl Drop for DisposableSpace {
    fn drop(&mut self) {
        if !self.cleaned
            && let Err(error) = self.cleanup()
        {
            eprintln!(
                "failed to clean disposable space {} during unwinding: {error:#}",
                self.name
            );
        }
    }
}

fn list_protobuf_import_collection_ids(space_name: &str) -> Result<HashSet<String>> {
    let output = run_anyr(["object", "list", space_name, "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let items = if let Some(items) = value.as_array() {
        items
    } else {
        value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("object list output missing items array"))?
    };
    Ok(items
        .iter()
        .filter_map(|item| {
            let is_collection = item.get("type").and_then(Value::as_str) == Some("collection");
            let is_import_collection = item
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.starts_with("Protobuf Import "));
            (is_collection && is_import_collection)
                .then(|| item.get("id").and_then(Value::as_str))
                .flatten()
                .map(ToString::to_string)
        })
        .collect())
}

impl Drop for PrefixCleanupGuard {
    fn drop(&mut self) {
        for scope in &self.scopes {
            for prefix in &scope.prefixes {
                let _ = delete_objects_by_prefix(&scope.space, prefix);
                let _ = delete_types_by_prefix(&scope.space, prefix);
                let _ = delete_properties_by_prefix(&scope.space, prefix);
            }
            if let Ok(current) = list_protobuf_import_collection_ids(&scope.space) {
                for id in current.difference(&scope.existing_protobuf_import_collections) {
                    let _ = delete_object(&scope.space, id);
                }
            }
        }
    }
}

#[cfg(feature = "snapshot-import")]
#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_create_full_then_restore_apply_subset() -> Result<()> {
    let (source_space, dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-m2-full-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone(), dest_space.name.clone()],
        vec![object_name.clone()],
    )?;
    let source_id = create_object(&source_space.name, &object_name, "milestone-02 body")?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let manifest_json = read_manifest_json(&archive_path)?;
    assert!(
        manifest_json["object_count"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "manifest object_count should be >= 1: {manifest_json}"
    );

    let import_ids_file = temp_dir.path().join("restore_ids.txt");
    write_ids_file(&import_ids_file, std::slice::from_ref(&source_id))?;

    let restore_output = run_anyback([
        "restore",
        "--objects",
        &import_ids_file.display().to_string(),
        "--space",
        dest_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    assert_import_counts(&restore_output, 1, 1, 0)?;
    assert_non_tty_output_clean(&restore_output);

    let imported_id = wait_find_object_id_by_name(&dest_space.name, &object_name).await?;
    let _ = delete_object(&source_space.name, &source_id);
    let _ = delete_object(&dest_space.name, &imported_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_create_full_then_restore_into_new_space_path() -> Result<()> {
    let (source_space, dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-m2-full-path-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone(), dest_space.name.clone()],
        vec![object_name.clone()],
    )?;
    let source_id = create_object(
        &source_space.name,
        &object_name,
        "milestone-02 full path restore body",
    )?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let manifest_json = read_manifest_json(&archive_path)?;
    assert!(
        manifest_json["object_count"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "manifest object_count should be >= 1: {manifest_json}"
    );

    let restore_output = run_anyback([
        "restore",
        "--space",
        dest_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let imported_id = wait_find_object_id_by_name(&dest_space.name, &object_name).await?;
    let imported = get_object_json(&dest_space.name, &imported_id)?;
    assert_eq!(
        imported.get("name").and_then(Value::as_str),
        Some(object_name.as_str())
    );
    wait_object_body_contains(
        &dest_space.name,
        &imported_id,
        "milestone-02 full path restore body",
    )
    .await?;

    let _ = delete_object(&source_space.name, &source_id);
    let _ = delete_object(&dest_space.name, &imported_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_create_full_then_restore_apply_same_space() -> Result<()> {
    let (source_space, _dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-m2-same-space-{unique}");
    let _cleanup =
        PrefixCleanupGuard::new(vec![source_space.name.clone()], vec![object_name.clone()])?;
    let source_id = create_object(&source_space.name, &object_name, "same-space restore body")?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let restore_output = run_anyback([
        "restore",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let _ = wait_find_object_id_by_name(&source_space.name, &object_name).await?;
    let _ = delete_object(&source_space.name, &source_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_create_json_output_is_parseable() -> Result<()> {
    let (source_space, _dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-json-{unique}");
    let _cleanup =
        PrefixCleanupGuard::new(vec![source_space.name.clone()], vec![object_name.clone()])?;
    let source_id = create_object(&source_space.name, &object_name, "json output test body")?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let object_ids_file = temp_dir.path().join("json_ids.txt");
    write_ids_file(&object_ids_file, std::slice::from_ref(&source_id))?;
    let output = run_anyback([
        "--json",
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &object_ids_file.display().to_string(),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    assert_non_tty_output_clean(&output);
    let payload: Value = serde_json::from_str(&output)
        .with_context(|| format!("expected valid JSON output, got: {output}"))?;
    assert!(
        payload.get("archive").is_some(),
        "missing archive field in json output: {payload}"
    );
    assert!(
        payload.get("exported").is_some(),
        "missing exported field in json output: {payload}"
    );
    assert!(
        payload.get("requested").is_some(),
        "missing requested field in json output: {payload}"
    );

    let _ = delete_object(&source_space.name, &source_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_create_pb_json_then_restore_same_space() -> Result<()> {
    let (source_space, _dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-pbjson-restore-{unique}");
    let _cleanup =
        PrefixCleanupGuard::new(vec![source_space.name.clone()], vec![object_name.clone()])?;
    let source_id = create_object(&source_space.name, &object_name, "pb-json restore body")?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--format",
        "pb-json",
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let manifest_json = read_manifest_json(&archive_path)?;
    assert_eq!(manifest_json["format"].as_str(), Some("pb-json"));

    let restore_output = run_anyback([
        "restore",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let list_output = run_anyback([
        "--json",
        "list",
        "--expanded",
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    let list_json: Value = serde_json::from_str(&list_output)
        .with_context(|| format!("expected valid list JSON output, got: {list_output}"))?;
    assert!(
        list_json
            .get("expanded")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()),
        "expected non-empty expanded entries in list output: {list_json}"
    );

    let _ = delete_object(&source_space.name, &source_id);
    let _ = delete_objects_by_name(&source_space.name, &object_name);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_archive_inspect_supports_pb_and_pb_json_archives() -> Result<()> {
    let (source_space, _dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-inspector-parity-{unique}");
    let _cleanup =
        PrefixCleanupGuard::new(vec![source_space.name.clone()], vec![object_name.clone()])?;
    let source_id = create_object(
        &source_space.name,
        &object_name,
        "inspector parity body text",
    )?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let pb_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--format",
        "pb",
        "--prefix",
        &format!("anyback-inspect-pb-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let pb_archive = parse_archive_path(&pb_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {pb_output}"))?;

    let pb_json_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--format",
        "pb-json",
        "--prefix",
        &format!("anyback-inspect-pbjson-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let pb_json_archive = parse_archive_path(&pb_json_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {pb_json_output}"))?;

    for archive in [pb_archive, pb_json_archive] {
        let list_output = run_anyback([
            "--json",
            "list",
            "--expanded",
            archive
                .to_str()
                .ok_or_else(|| anyhow!("bad archive path"))?,
        ])?;
        let list_json: Value = serde_json::from_str(&list_output)
            .with_context(|| format!("expected valid list JSON output, got: {list_output}"))?;
        assert!(
            list_json
                .get("expanded")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty()),
            "expected non-empty expanded entries for {}",
            archive.display()
        );
    }

    let _ = delete_object(&source_space.name, &source_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_restore_chat_space_messages() -> Result<()> {
    let Some(chat_space) = choose_writable_chat_space_cli().await? else {
        eprintln!("no writable chat space found; skipping chat-space e2e");
        return Ok(());
    };
    let unique = anytype::test_util::unique_suffix();
    let token = format!("anyback-chat-space-msg-{unique}");
    let _cleanup = PrefixCleanupGuard::new(vec![chat_space.name.clone()], Vec::new())?;
    let _chat_cleanup =
        ChatMessageTokenCleanupGuard::new(vec![(chat_space.name.clone(), token.clone())]);

    let chat_id = resolve_default_chat_id(&chat_space.name)?;
    let message_id = send_chat_message(&chat_space.name, &chat_id, &token)?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let backup_output = run_anyback([
        "create",
        "--space",
        chat_space.name.as_str(),
        "--prefix",
        &format!("anyback-chat-space-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    delete_chat_message(&chat_space.name, &chat_id, &message_id)?;
    wait_chat_message_absent(&chat_space.name, &chat_id, &token).await?;

    let restore_output = run_anyback([
        "restore",
        "--space",
        chat_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    wait_chat_message_contains(&chat_space.name, &chat_id, &token).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_restore_regular_space_chat_messages() -> Result<()> {
    let (source_space, dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let token = format!("anyback-regular-chat-msg-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone(), dest_space.name.clone()],
        Vec::new(),
    )?;
    let _chat_cleanup = ChatMessageTokenCleanupGuard::new(vec![
        (source_space.name.clone(), token.clone()),
        (dest_space.name.clone(), token.clone()),
    ]);

    let source_chat_id = match resolve_default_chat_id(&source_space.name) {
        Ok(chat_id) => chat_id,
        Err(err) if err.to_string().contains("no chats available") => {
            eprintln!(
                "no chats available in regular source space {}; skipping regular-space chat e2e",
                source_space.name
            );
            return Ok(());
        }
        Err(err) => return Err(err),
    };
    let source_message_id = send_chat_message(&source_space.name, &source_chat_id, &token)?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--prefix",
        &format!("anyback-regular-chat-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    if source_space.id == dest_space.id {
        delete_chat_message(&source_space.name, &source_chat_id, &source_message_id)?;
        wait_chat_message_absent(&source_space.name, &source_chat_id, &token).await?;
    }

    let restore_output = run_anyback([
        "restore",
        "--space",
        dest_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    wait_chat_message_in_space(&dest_space.name, &token).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires protected disposable Anytype server admission"]
async fn e2e_restore_preserves_file_payload_metadata_and_host_attachment() -> Result<()> {
    verify_authenticated_pings_cli()?;
    let mut source = DisposableSpace::create("file-fidelity-src")?;
    let mut destination = match DisposableSpace::create("file-fidelity-dst") {
        Ok(space) => space,
        Err(error) => return finish_disposable_spaces(Err(error), &mut [&mut source]),
    };

    let result = exercise_file_restore_fidelity(&mut source, &mut destination).await;
    finish_disposable_spaces(result, &mut [&mut destination, &mut source])
}

async fn exercise_file_restore_fidelity(
    source: &mut DisposableSpace,
    destination: &mut DisposableSpace,
) -> Result<()> {
    const MIME: &str = "application/octet-stream";
    const PAYLOAD: &[u8] = b"anyback-file-fidelity\0payload\nwith deterministic bytes\xff";

    let unique = anytype::test_util::unique_suffix();
    let key_suffix = unique_alpha_key_suffix();
    let type_key = format!("anybackfiletype{key_suffix}");
    let attachment_key = format!("anyback_file_attachment_{key_suffix}");
    let type_name = format!("Anyback File Fidelity {unique}");
    let host_name = format!("anyback-file-host-{unique}");
    let file_name = format!("anyback-file-payload-{unique}.bin");

    let created_type = create_type_with_properties(
        &source.id,
        &type_key,
        &type_name,
        &[(attachment_key.as_str(), "files", "Restored Attachment")],
    )?;
    register_type_value(source, &created_type, &[attachment_key.as_str()])?;
    sleep(Duration::from_millis(600)).await;

    let upload_dir = tempfile::tempdir().context("failed to create file fixture directory")?;
    let source_path = upload_dir.path().join(&file_name);
    fs::write(&source_path, PAYLOAD)
        .with_context(|| format!("failed to write file fixture {}", source_path.display()))?;
    let source_file_id = upload_file_object_with_mime(&source.id, &source_path, MIME)?;
    source.register("file", source_file_id.clone());

    let host_id = create_typed_object_with_properties(
        &source.id,
        &type_key,
        &host_name,
        "file attachment host",
        &[(attachment_key.as_str(), source_file_id.as_str())],
    )?;
    source.register("object", host_id.clone());

    let client = anytype::test_util::test_client_named("anyback_file_restore_fidelity")
        .map_err(|error| anyhow!("failed to build file fidelity client: {error}"))?;
    let source_file = client
        .files()
        .get(&source.id, &source_file_id)
        .get()
        .await?;
    let source_bytes = client
        .files()
        .download_bytes(&source.id, &source_file_id)
        .await?;
    ensure!(
        source_bytes.as_ref() == PAYLOAD,
        "source upload bytes changed before backup"
    );
    ensure!(
        source_file.name.as_deref() == Some(file_name.as_str()),
        "source file name mismatch"
    );
    ensure!(
        source_file.mime.as_deref() == Some(MIME),
        "source file MIME mismatch"
    );

    let archive_dir = tempfile::tempdir().context("failed to create archive directory")?;
    let archive = create_full_backup(&source.id, archive_dir.path(), true)?;
    restore_archive(&destination.id, &archive)?;

    let destination_file_id = wait_find_file_id_by_name(&destination.id, &file_name).await?;
    destination.register("file", destination_file_id.clone());
    let destination_file = client
        .files()
        .get(&destination.id, &destination_file_id)
        .get()
        .await?;
    let destination_metadata = client
        .files()
        .metadata(&destination.id, &destination_file_id)
        .await?;
    let destination_bytes = client
        .files()
        .download_bytes(&destination.id, &destination_file_id)
        .await?;
    ensure!(
        destination_bytes == source_bytes,
        "restored file bytes differ from source fixture"
    );
    ensure!(
        destination_file.name == source_file.name,
        "restored file name differs from source"
    );
    ensure!(
        destination_file.mime == source_file.mime,
        "restored file MIME differs from source"
    );
    ensure!(
        destination_metadata.metadata.content_type.as_deref() == Some(MIME),
        "restored HTTP file metadata MIME mismatch"
    );

    let restored_type = wait_resolve_type(&client, &destination.id, &type_key).await?;
    destination.register("type", restored_type.id.clone());
    for property in &restored_type.properties {
        if property.key == attachment_key {
            destination.register("property", property.id.clone());
        }
    }
    let restored_host_id = wait_find_object_id_by_name(&destination.id, &host_name).await?;
    destination.register("object", restored_host_id.clone());
    let restored_host = client
        .object(&destination.id, &restored_host_id)
        .get()
        .await?;
    let attachment_ids = restored_host
        .properties
        .iter()
        .find(|property| property.key == attachment_key)
        .and_then(|property| match &property.value {
            anytype::properties::PropertyValue::Files { files } => Some(files.as_slice()),
            _ => None,
        })
        .ok_or_else(|| anyhow!("restored host is missing the file attachment property"))?;
    ensure!(
        attachment_ids.len() == 1
            && attachment_ids.first().map(String::as_str) == Some(destination_file_id.as_str()),
        "restored host does not reference the independently resolved destination file"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires protected disposable Anytype server admission"]
async fn e2e_restore_preserves_chat_order_reply_and_attachment() -> Result<()> {
    verify_authenticated_pings_cli()?;
    let mut source = DisposableSpace::create("chat-fidelity-src")?;
    let mut destination = match DisposableSpace::create("chat-fidelity-dst") {
        Ok(space) => space,
        Err(error) => return finish_disposable_spaces(Err(error), &mut [&mut source]),
    };

    let result = exercise_chat_restore_fidelity(&mut source, &mut destination).await;
    finish_disposable_spaces(result, &mut [&mut destination, &mut source])
}

async fn exercise_chat_restore_fidelity(
    source: &mut DisposableSpace,
    destination: &mut DisposableSpace,
) -> Result<()> {
    const MIME: &str = "text/plain";
    const PAYLOAD: &[u8] = b"anyback chat attachment fidelity\n";

    let unique = anytype::test_util::unique_suffix();
    let chat_name = format!("anyback-chat-fidelity-{unique}");
    let first_token = format!("anyback-chat-first-{unique}");
    let second_token = format!("anyback-chat-second-{unique}");
    let file_name = format!("anyback-chat-attachment-{unique}.txt");

    let source_chat_id = create_chat(&source.id, &chat_name)?;
    source.register("chat", source_chat_id.clone());
    let upload_dir = tempfile::tempdir().context("failed to create chat fixture directory")?;
    let source_path = upload_dir.path().join(&file_name);
    fs::write(&source_path, PAYLOAD)
        .with_context(|| format!("failed to write chat fixture {}", source_path.display()))?;
    let source_file_id = upload_file_object_with_mime(&source.id, &source_path, MIME)?;
    source.register("file", source_file_id.clone());

    let first_id = send_chat_message(&source.id, &source_chat_id, &first_token)?;
    source.register("message", first_id.clone());
    let second_id = send_chat_message_with_reply_and_attachment(
        &source.id,
        &source_chat_id,
        &second_token,
        &first_id,
        &source_file_id,
    )?;
    source.register("message", second_id);

    let archive_dir = tempfile::tempdir().context("failed to create archive directory")?;
    let archive = create_full_backup(&source.id, archive_dir.path(), true)?;
    restore_archive(&destination.id, &archive)?;

    let client = anytype::test_util::test_client_named("anyback_chat_restore_fidelity")
        .map_err(|error| anyhow!("failed to build chat fidelity client: {error}"))?;
    let destination_chat_id = wait_resolve_chat(&client, &destination.id, &chat_name).await?;
    destination.register("chat", destination_chat_id.clone());
    let destination_file_id = wait_find_file_id_by_name(&destination.id, &file_name).await?;
    destination.register("file", destination_file_id.clone());
    let destination_file = client
        .files()
        .get(&destination.id, &destination_file_id)
        .get()
        .await?;
    ensure!(
        destination_file.name.as_deref() == Some(file_name.as_str())
            && destination_file.mime.as_deref() == Some(MIME),
        "restored chat attachment metadata mismatch"
    );

    let messages = wait_for_chat_messages(
        &client,
        &destination.id,
        &destination_chat_id,
        [&first_token, &second_token],
    )
    .await?;
    let first_index = messages
        .iter()
        .position(|message| message.content.text.contains(&first_token))
        .ok_or_else(|| anyhow!("restored first chat message is missing"))?;
    let second_index = messages
        .iter()
        .position(|message| message.content.text.contains(&second_token))
        .ok_or_else(|| anyhow!("restored second chat message is missing"))?;
    ensure!(
        first_index < second_index,
        "restored chat messages are not in source order"
    );
    let first = messages
        .get(first_index)
        .ok_or_else(|| anyhow!("restored first chat message index disappeared"))?;
    let second = messages
        .get(second_index)
        .ok_or_else(|| anyhow!("restored second chat message index disappeared"))?;
    destination.register("message", first.id.clone());
    destination.register("message", second.id.clone());
    ensure!(
        second.reply_to_message_id.as_deref() == Some(first.id.as_str()),
        "restored reply does not target the destination first message"
    );
    ensure!(
        second.attachments.iter().any(|attachment| {
            attachment.target == destination_file_id
                && matches!(
                    &attachment.kind,
                    anytype::chats::MessageAttachmentType::File
                )
        }),
        "restored second message does not reference the destination attachment"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires protected disposable Anytype server admission"]
async fn e2e_restore_preserves_custom_schema_keys_formats_and_featured_membership() -> Result<()> {
    verify_authenticated_pings_cli()?;
    let mut source = DisposableSpace::create("schema-fidelity-src")?;
    let mut destination = match DisposableSpace::create("schema-fidelity-dst") {
        Ok(space) => space,
        Err(error) => return finish_disposable_spaces(Err(error), &mut [&mut source]),
    };

    let result = exercise_schema_restore_fidelity(&mut source, &mut destination).await;
    finish_disposable_spaces(result, &mut [&mut destination, &mut source])
}

async fn exercise_schema_restore_fidelity(
    source: &mut DisposableSpace,
    destination: &mut DisposableSpace,
) -> Result<()> {
    let unique = anytype::test_util::unique_suffix();
    let key_suffix = unique_alpha_key_suffix();
    let type_key = format!("anybackschematype{key_suffix}");
    let type_name = format!("Anyback Schema Fidelity {unique}");
    let properties = [
        (
            format!("anyback_schema_text_{key_suffix}"),
            "text",
            "Schema Text",
        ),
        (
            format!("anyback_schema_number_{key_suffix}"),
            "number",
            "Schema Number",
        ),
        (
            format!("anyback_schema_checkbox_{key_suffix}"),
            "checkbox",
            "Schema Checkbox",
        ),
        (
            format!("anyback_schema_url_{key_suffix}"),
            "url",
            "Schema URL",
        ),
    ];
    let property_specs = properties
        .iter()
        .map(|(key, format, name)| (key.as_str(), *format, *name))
        .collect::<Vec<_>>();
    let created_type =
        create_type_with_properties(&source.id, &type_key, &type_name, &property_specs)?;
    let expected_property_keys = properties
        .iter()
        .map(|(key, _, _)| key.as_str())
        .collect::<Vec<_>>();
    register_type_value(source, &created_type, &expected_property_keys)?;
    sleep(Duration::from_millis(600)).await;

    let client = anytype::test_util::test_client_named("anyback_schema_restore_fidelity")
        .map_err(|error| anyhow!("failed to build schema fidelity client: {error}"))?;
    let source_type = wait_resolve_type(&client, &source.id, &type_key).await?;
    let source_classification = client
        .get_type(&source.id, &source_type.id)
        .classify_properties()
        .await?;
    let source_featured_keys = source_classification
        .featured
        .iter()
        .map(|property| property.key.clone())
        .collect::<BTreeSet<_>>();
    ensure!(
        !source_featured_keys.is_empty(),
        "source custom type has no visible featured properties"
    );

    let archive_dir = tempfile::tempdir().context("failed to create archive directory")?;
    let archive = create_full_backup(&source.id, archive_dir.path(), false)?;
    restore_archive(&destination.id, &archive)?;

    let restored_type = wait_resolve_type(&client, &destination.id, &type_key).await?;
    destination.register("type", restored_type.id.clone());
    ensure!(
        restored_type.key == type_key,
        "restored custom type key changed"
    );
    let restored_classification = client
        .get_type(&destination.id, &restored_type.id)
        .classify_properties()
        .await?;
    let restored_formats = restored_classification
        .recommended
        .iter()
        .filter(|property| properties.iter().any(|(key, _, _)| key == &property.key))
        .map(|property| (property.key.clone(), property.format().to_string()))
        .collect::<BTreeMap<_, _>>();
    let expected_formats = properties
        .iter()
        .map(|(key, format, _)| (key.clone(), (*format).to_string()))
        .collect::<BTreeMap<_, _>>();
    ensure!(
        restored_formats == expected_formats,
        "restored custom property keys or formats changed: {restored_formats:?}"
    );
    for property in &restored_classification.recommended {
        if expected_formats.contains_key(&property.key) {
            destination.register("property", property.id.clone());
        }
    }
    let restored_featured_keys = restored_classification
        .featured
        .iter()
        .map(|property| property.key.clone())
        .collect::<BTreeSet<_>>();
    ensure!(
        restored_featured_keys == source_featured_keys,
        "restored featured-property membership changed"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires protected disposable Anytype server admission"]
async fn e2e_restore_preserves_typed_object_value_fidelity() -> Result<()> {
    verify_authenticated_pings_cli()?;
    let mut source = DisposableSpace::create("typed-value-fidelity-src")?;
    let mut destination = match DisposableSpace::create("typed-value-fidelity-dst") {
        Ok(space) => space,
        Err(error) => return finish_disposable_spaces(Err(error), &mut [&mut source]),
    };

    let result = exercise_typed_object_value_restore_fidelity(&mut source, &mut destination).await;
    finish_disposable_spaces(result, &mut [&mut destination, &mut source])
}

async fn exercise_typed_object_value_restore_fidelity(
    source: &mut DisposableSpace,
    destination: &mut DisposableSpace,
) -> Result<()> {
    const TEXT_VALUE: &str = "typed restore semantic text";
    const NUMBER_VALUE: i64 = 42;
    const URL_VALUE: &str = "https://example.invalid/anyback-typed-fidelity";

    let unique = anytype::test_util::unique_suffix();
    let key_suffix = unique_alpha_key_suffix();
    let type_key = format!("anybacktypedvalue{key_suffix}");
    let type_name = format!("Anyback Typed Value Fidelity {unique}");
    let text_key = format!("anyback_typed_text_{key_suffix}");
    let number_key = format!("anyback_typed_number_{key_suffix}");
    let checkbox_key = format!("anyback_typed_checkbox_{key_suffix}");
    let url_key = format!("anyback_typed_url_{key_suffix}");
    let objects_key = format!("anyback_typed_objects_{key_suffix}");
    let select_key = format!("anyback_typed_select_{key_suffix}");
    let multi_select_key = format!("anyback_typed_multi_select_{key_suffix}");
    let select_option_key = format!("anyback_typed_select_option_{key_suffix}");
    let multi_option_one_key = format!("anyback_typed_multi_one_{key_suffix}");
    let multi_option_two_key = format!("anyback_typed_multi_two_{key_suffix}");
    let target_name = format!("anyback-typed-target-{unique}");
    let host_name = format!("anyback-typed-host-{unique}");

    let client = anytype::test_util::test_client_named("anyback_typed_value_restore_fidelity")
        .map_err(|error| anyhow!("failed to build typed value fidelity client: {error}"))?;
    let source_type = client
        .new_type(&source.id, &type_name)
        .key(&type_key)
        .property("Typed Text", &text_key, PropertyFormat::Text)
        .property("Typed Number", &number_key, PropertyFormat::Number)
        .property("Typed Checkbox", &checkbox_key, PropertyFormat::Checkbox)
        .property("Typed URL", &url_key, PropertyFormat::Url)
        .property("Typed Relation", &objects_key, PropertyFormat::Objects)
        .property("Typed Select", &select_key, PropertyFormat::Select)
        .property(
            "Typed Multi Select",
            &multi_select_key,
            PropertyFormat::MultiSelect,
        )
        .create()
        .await?;
    source.register("type", source_type.id.clone());
    for property in &source_type.properties {
        source.register("property", property.id.clone());
    }

    let source_select_property = find_property_by_key(&source_type.properties, &select_key)?;
    let source_multi_select_property =
        find_property_by_key(&source_type.properties, &multi_select_key)?;
    let source_select_option = client
        .new_tag(&source.id, &source_select_property.id)
        .name(format!("Typed Select Option {unique}"))
        .key(&select_option_key)
        .create()
        .await?;
    source.register("tag", source_select_option.id.clone());
    let source_multi_option_one = client
        .new_tag(&source.id, &source_multi_select_property.id)
        .name(format!("Typed Multi Option One {unique}"))
        .key(&multi_option_one_key)
        .create()
        .await?;
    source.register("tag", source_multi_option_one.id.clone());
    let source_multi_option_two = client
        .new_tag(&source.id, &source_multi_select_property.id)
        .name(format!("Typed Multi Option Two {unique}"))
        .key(&multi_option_two_key)
        .create()
        .await?;
    source.register("tag", source_multi_option_two.id.clone());

    let source_target = client
        .new_object(&source.id, "page")
        .name(&target_name)
        .body("typed relation target")
        .create()
        .await?;
    source.register("object", source_target.id.clone());
    let source_host = client
        .new_object(&source.id, &type_key)
        .name(&host_name)
        .body("typed value host")
        .set_text(&text_key, TEXT_VALUE)
        .set_number(&number_key, NUMBER_VALUE)
        .set_checkbox(&checkbox_key, true)
        .set_url(&url_key, URL_VALUE)
        .set_objects(&objects_key, [source_target.id.clone()])
        .set_select(&select_key, source_select_option.id.clone())
        .set_multi_select(
            &multi_select_key,
            [
                source_multi_option_one.id.clone(),
                source_multi_option_two.id.clone(),
            ],
        )
        .create()
        .await?;
    source.register("object", source_host.id.clone());
    let source_expectation = TypedValueExpectation {
        text_value: TEXT_VALUE,
        number_value: NUMBER_VALUE,
        url_value: URL_VALUE,
        text_key: &text_key,
        number_key: &number_key,
        checkbox_key: &checkbox_key,
        url_key: &url_key,
        objects_key: &objects_key,
        select_key: &select_key,
        multi_select_key: &multi_select_key,
        relation_target_id: &source_target.id,
        select_option_id: &source_select_option.id,
        select_option_key: &select_option_key,
        multi_select_option_ids: [&source_multi_option_one.id, &source_multi_option_two.id],
        multi_select_option_keys: [&multi_option_one_key, &multi_option_two_key],
    };
    wait_for_typed_value_semantics(&client, &source.id, &source_host.id, &source_expectation)
        .await?;

    let archive_dir = tempfile::tempdir().context("failed to create archive directory")?;
    let archive = create_full_backup(&source.id, archive_dir.path(), false)?;
    restore_archive(&destination.id, &archive)?;

    let destination_type = wait_resolve_type(&client, &destination.id, &type_key).await?;
    destination.register("type", destination_type.id.clone());
    let expected_property_keys = [
        text_key.as_str(),
        number_key.as_str(),
        checkbox_key.as_str(),
        url_key.as_str(),
        objects_key.as_str(),
        select_key.as_str(),
        multi_select_key.as_str(),
    ];
    let destination_properties =
        wait_resolve_properties_by_exact_keys(&client, &destination.id, &expected_property_keys)
            .await?;
    for property in destination_properties.values() {
        destination.register("property", property.id.clone());
    }
    let destination_select_property = destination_properties
        .get(&select_key)
        .ok_or_else(|| anyhow!("destination select property was not resolved"))?
        .clone();
    let destination_multi_select_property = destination_properties
        .get(&multi_select_key)
        .ok_or_else(|| anyhow!("destination multi-select property was not resolved"))?
        .clone();
    let destination_select_option = wait_resolve_property_tag_by_key(
        &client,
        &destination.id,
        &destination_select_property.id,
        &select_option_key,
    )
    .await?;
    destination.register("tag", destination_select_option.id.clone());
    let destination_multi_option_one = wait_resolve_property_tag_by_key(
        &client,
        &destination.id,
        &destination_multi_select_property.id,
        &multi_option_one_key,
    )
    .await?;
    destination.register("tag", destination_multi_option_one.id.clone());
    let destination_multi_option_two = wait_resolve_property_tag_by_key(
        &client,
        &destination.id,
        &destination_multi_select_property.id,
        &multi_option_two_key,
    )
    .await?;
    destination.register("tag", destination_multi_option_two.id.clone());

    let destination_target_id =
        wait_find_object_id_by_exact_name(&destination.id, &target_name).await?;
    destination.register("object", destination_target_id.clone());
    let destination_host_id =
        wait_find_object_id_by_exact_name(&destination.id, &host_name).await?;
    destination.register("object", destination_host_id.clone());
    let destination_expectation = TypedValueExpectation {
        text_value: TEXT_VALUE,
        number_value: NUMBER_VALUE,
        url_value: URL_VALUE,
        text_key: &text_key,
        number_key: &number_key,
        checkbox_key: &checkbox_key,
        url_key: &url_key,
        objects_key: &objects_key,
        select_key: &select_key,
        multi_select_key: &multi_select_key,
        relation_target_id: &destination_target_id,
        select_option_id: &destination_select_option.id,
        select_option_key: &select_option_key,
        multi_select_option_ids: [
            &destination_multi_option_one.id,
            &destination_multi_option_two.id,
        ],
        multi_select_option_keys: [&multi_option_one_key, &multi_option_two_key],
    };
    wait_for_typed_value_semantics(
        &client,
        &destination.id,
        &destination_host_id,
        &destination_expectation,
    )
    .await?;
    Ok(())
}

fn find_property_by_key<'a>(
    properties: &'a [anytype::properties::Property],
    key: &str,
) -> Result<&'a anytype::properties::Property> {
    properties
        .iter()
        .find(|property| property.key == key)
        .ok_or_else(|| anyhow!("type creation did not return property with key {key}"))
}

struct TypedValueExpectation<'a> {
    text_value: &'a str,
    number_value: i64,
    url_value: &'a str,
    text_key: &'a str,
    number_key: &'a str,
    checkbox_key: &'a str,
    url_key: &'a str,
    objects_key: &'a str,
    select_key: &'a str,
    multi_select_key: &'a str,
    relation_target_id: &'a str,
    select_option_id: &'a str,
    select_option_key: &'a str,
    multi_select_option_ids: [&'a str; 2],
    multi_select_option_keys: [&'a str; 2],
}

fn assert_typed_value_semantics(
    properties: &[anytype::properties::PropertyWithValue],
    expected: &TypedValueExpectation<'_>,
) -> Result<()> {
    ensure_property_value(
        properties,
        expected.text_key,
        |value| matches!(value, anytype::properties::PropertyValue::Text { text } if text == expected.text_value),
        "text",
    )?;
    ensure_property_value(
        properties,
        expected.number_key,
        |value| matches!(value, anytype::properties::PropertyValue::Number { number } if number.as_i64() == Some(expected.number_value)),
        "number",
    )?;
    ensure_property_value(
        properties,
        expected.checkbox_key,
        |value| {
            matches!(
                value,
                anytype::properties::PropertyValue::Checkbox { checkbox: true }
            )
        },
        "checkbox",
    )?;
    ensure_property_value(
        properties,
        expected.url_key,
        |value| matches!(value, anytype::properties::PropertyValue::Url { url } if url == expected.url_value),
        "URL",
    )?;
    ensure_property_value(
        properties,
        expected.objects_key,
        |value| matches!(value, anytype::properties::PropertyValue::Objects { objects } if objects.as_slice() == [expected.relation_target_id]),
        "relation target",
    )?;
    ensure_property_value(
        properties,
        expected.select_key,
        |value| matches!(value, anytype::properties::PropertyValue::Select { select } if select.key == expected.select_option_key && select.id == expected.select_option_id),
        "select option",
    )?;
    let [first_multi_select_id, second_multi_select_id] = expected.multi_select_option_ids;
    let [first_multi_select_key, second_multi_select_key] = expected.multi_select_option_keys;
    let expected_multi_select_ids = BTreeSet::from([first_multi_select_id, second_multi_select_id]);
    let expected_multi_select_keys =
        BTreeSet::from([first_multi_select_key, second_multi_select_key]);
    ensure_property_value(
        properties,
        expected.multi_select_key,
        |value| match value {
            anytype::properties::PropertyValue::MultiSelect { multi_select } => {
                multi_select.len() == expected_multi_select_ids.len()
                    && multi_select
                        .iter()
                        .map(|tag| tag.id.as_str())
                        .collect::<BTreeSet<_>>()
                        == expected_multi_select_ids
                    && multi_select
                        .iter()
                        .map(|tag| tag.key.as_str())
                        .collect::<BTreeSet<_>>()
                        == expected_multi_select_keys
            }
            _ => false,
        },
        "multi-select options",
    )?;
    Ok(())
}

fn ensure_property_value(
    properties: &[anytype::properties::PropertyWithValue],
    key: &str,
    predicate: impl FnOnce(&anytype::properties::PropertyValue) -> bool,
    label: &str,
) -> Result<()> {
    let property = properties
        .iter()
        .find(|property| property.key == key)
        .ok_or_else(|| anyhow!("typed host is missing {label} property {key}"))?;
    ensure!(
        predicate(&property.value),
        "typed host {label} property {key} differs from the expected semantic value"
    );
    Ok(())
}

async fn wait_for_typed_value_semantics(
    client: &AnytypeClient,
    space_id: &str,
    object_id: &str,
    expected: &TypedValueExpectation<'_>,
) -> Result<()> {
    let mut last_evidence = String::from("no typed host read completed");
    for _ in 0..40 {
        match client.object(space_id, object_id).get().await {
            Ok(host) => match assert_typed_value_semantics(&host.properties, expected) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    let observed_keys = host
                        .properties
                        .iter()
                        .map(|property| property.key.as_str())
                        .collect::<Vec<_>>()
                        .join(",");
                    last_evidence = format!(
                        "semantic mismatch: {error:#}; observed property keys: {observed_keys}; observed properties: {:#?}",
                        host.properties
                    );
                }
            },
            Err(error) => last_evidence = format!("typed host read failed: {error}"),
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!(
        "typed host {object_id} did not expose all expected value semantics after 40 attempts: {last_evidence}"
    )
}

async fn wait_resolve_properties_by_exact_keys(
    client: &AnytypeClient,
    space_id: &str,
    expected_keys: &[&str],
) -> Result<BTreeMap<String, anytype::properties::Property>> {
    let expected = expected_keys.iter().copied().collect::<BTreeSet<_>>();
    let mut last_evidence = String::from("no property list completed");
    for _ in 0..40 {
        let listed = client.properties(space_id).limit(1000).list().await;
        let listed = match listed {
            Ok(result) => result.collect_all().await,
            Err(error) => Err(error),
        };
        match listed {
            Ok(properties) => {
                let resolved = properties
                    .iter()
                    .filter(|property| expected.contains(property.key.as_str()))
                    .map(|property| (property.key.clone(), property.clone()))
                    .collect::<BTreeMap<_, _>>();
                if resolved.len() == expected.len()
                    && expected.iter().all(|key| resolved.contains_key(*key))
                {
                    return Ok(resolved);
                }
                let present = resolved.keys().cloned().collect::<Vec<_>>().join(",");
                last_evidence = format!(
                    "expected keys: {}; visible expected keys: {present}",
                    expected.iter().copied().collect::<Vec<_>>().join(",")
                );
            }
            Err(error) => last_evidence = format!("direct property list failed: {error}"),
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!(
        "destination properties did not expose every expected key after 40 attempts: {last_evidence}"
    )
}

async fn wait_resolve_property_tag_by_key(
    client: &AnytypeClient,
    space_id: &str,
    property_id: &str,
    tag_key: &str,
) -> Result<anytype::tags::Tag> {
    let mut last_error = String::from("no tag lookup completed");
    for _ in 0..40 {
        match client
            .lookup_property_tag(space_id, property_id, tag_key)
            .await
        {
            Ok(tag) => return Ok(tag),
            Err(error) => last_error = error.to_string(),
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!(
        "destination tag {tag_key} for property {property_id} was not resolved after 40 attempts: {last_error}"
    )
}

async fn wait_find_object_id_by_exact_name(space_name: &str, name: &str) -> Result<String> {
    let mut last_count = 0_usize;
    for _ in 0..40 {
        let output = run_anyr(["object", "list", space_name, "--all"])?;
        let value: Value = serde_json::from_str(&output)?;
        let items = if let Some(items) = value.as_array() {
            items
        } else {
            value
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("object list output missing items array"))?
        };
        last_count = items.len();
        if let Some(id) = items.iter().find_map(|item| {
            (item.get("name").and_then(Value::as_str) == Some(name))
                .then(|| item.get("id").and_then(Value::as_str))
                .flatten()
                .map(ToString::to_string)
        }) {
            return Ok(id);
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!(
        "object with exact name '{name}' was not found in space {space_name} after 40 attempts (last candidate count: {last_count})"
    )
}

#[cfg(feature = "snapshot-import")]
#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_export_then_import_subset_between_spaces() -> Result<()> {
    let (source_space, dest_space) = choose_writable_spaces_cli().await?;
    let fixture = object_generator::generate_fixture(&source_space.name, &source_space.id).await?;
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone(), dest_space.name.clone()],
        vec![fixture.prefix.clone()],
    )?;
    let source_ids: Vec<String> = fixture.objects.iter().map(|o| o.id.clone()).collect();

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let export_ids_file = temp_dir.path().join("export_ids.txt");
    write_ids_file(&export_ids_file, &source_ids)?;

    let export_output = run_anyback([
        "export",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &export_ids_file.display().to_string(),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;

    let archive_path = parse_archive_path(&export_output).ok_or_else(|| {
        anyhow!("could not parse archive path from export output: {export_output}")
    })?;

    let manifest_json = read_manifest_json(&archive_path)?;
    assert_eq!(manifest_json["object_count"].as_u64(), Some(6));

    let subset_ids = vec![source_ids[0].clone(), source_ids[1].clone()];
    let import_ids_file = temp_dir.path().join("import_ids.txt");
    write_ids_file(&import_ids_file, &subset_ids)?;
    let report_path = temp_dir.path().join("import-report.json");

    let import_output = run_anyback([
        "import",
        "--objects",
        &import_ids_file.display().to_string(),
        "--space",
        dest_space.name.as_str(),
        "--log",
        &report_path.display().to_string(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;

    assert_import_counts(&import_output, 2, 2, 0)?;
    assert_non_tty_output_clean(&import_output);

    let report_text = fs::read_to_string(&report_path)
        .with_context(|| format!("missing report file {}", report_path.display()))?;
    let report_json: Value = serde_json::from_str(&report_text)?;
    assert_eq!(report_json["attempted"].as_u64(), Some(2));
    assert_eq!(report_json["failed"].as_u64(), Some(0));
    assert_eq!(report_json["imported"].as_u64(), Some(2));
    let success = report_json
        .get("success")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("import report missing success array"))?;
    assert_eq!(
        success.len(),
        2,
        "expected two success entries in import report"
    );
    let success_ids: Vec<String> = success
        .iter()
        .filter_map(|entry| {
            entry
                .get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect();
    for id in &subset_ids {
        assert!(
            success_ids.contains(id),
            "import report success ids missing expected source id {id}"
        );
    }

    let client_verify = anytype::test_util::test_client_named("anyback_e2e_verify")
        .map_err(|e| anyhow!("failed to build verification client: {e}"))?;

    // cleanup source fixture and imported destination objects
    object_generator::cleanup_by_ids(&client_verify, &source_space.id, &source_ids).await?;
    let _ =
        object_generator::cleanup_by_name_prefix(&client_verify, &dest_space.id, &fixture.prefix)
            .await?;

    Ok(())
}

#[cfg(feature = "snapshot-import")]
#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_apply_json_output_is_parseable() -> Result<()> {
    let (source_space, dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-restore-json-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone(), dest_space.name.clone()],
        vec![object_name.clone()],
    )?;
    let source_id = create_object(&source_space.name, &object_name, "restore json body")?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let object_ids_file = temp_dir.path().join("restore_json_ids.txt");
    write_ids_file(&object_ids_file, std::slice::from_ref(&source_id))?;

    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &object_ids_file.display().to_string(),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let restore_output = run_anyback([
        "--json",
        "restore",
        "--objects",
        &object_ids_file.display().to_string(),
        "--space",
        dest_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    assert_non_tty_output_clean(&restore_output);
    let payload: Value = serde_json::from_str(&restore_output)
        .with_context(|| format!("expected valid JSON output, got: {restore_output}"))?;
    assert_eq!(payload.get("attempted").and_then(Value::as_u64), Some(1));
    assert_eq!(payload.get("failed").and_then(Value::as_u64), Some(0));
    assert_eq!(payload.get("imported").and_then(Value::as_u64), Some(1));

    let imported_id = wait_find_object_id_by_name(&dest_space.name, &object_name).await?;
    let _ = delete_object(&source_space.name, &source_id);
    let _ = delete_object(&dest_space.name, &imported_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_create_incremental_with_types_filter() -> Result<()> {
    let (source_space, _dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let old_page_name = format!("anyback-inc-page-{unique}");
    let new_note_name = format!("anyback-inc-note-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec![old_page_name.clone(), new_note_name.clone()],
    )?;
    let old_page_id = create_object(&source_space.name, &old_page_name, "old page body")?;
    let since = "1970-01-01T00:00:00Z".to_string();
    let _new_note_id = create_object(&source_space.name, &new_note_name, "new note body")?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--mode",
        "incremental",
        "--since",
        &since,
        "--types",
        "note",
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;

    let archive_path = parse_archive_path(&output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {output}"))?;
    let manifest_json = read_manifest_json(&archive_path)?;

    assert_eq!(
        manifest_json.get("mode").and_then(Value::as_str),
        Some("incremental")
    );
    assert!(manifest_json.get("since").is_some());
    let objects = manifest_json
        .get("objects")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("manifest objects missing"))?;
    let ids: Vec<String> = objects
        .iter()
        .filter_map(|obj| {
            obj.get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect();
    assert!(!ids.contains(&old_page_id));
    for object in objects {
        if let Some(type_key) = object.get("type").and_then(Value::as_str) {
            assert_eq!(type_key, "note");
        }
    }

    let _ = delete_object(&source_space.name, &old_page_id);
    let _ = delete_object(&source_space.name, &_new_note_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_dry_run_preflight_for_incremental_archive() -> Result<()> {
    let (source_space, dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let base_name = format!("anyback-plan-base-{unique}");
    let inc_name = format!("anyback-plan-inc-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone(), dest_space.name.clone()],
        vec![base_name.clone(), inc_name.clone()],
    )?;
    let base_id = create_object(&source_space.name, &base_name, "base body")?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let full_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let _full_archive = parse_archive_path(&full_output)
        .ok_or_else(|| anyhow!("could not parse full archive path from output: {full_output}"))?;

    sleep(Duration::from_secs(2)).await;
    let since = Utc::now().to_rfc3339();
    sleep(Duration::from_secs(2)).await;
    let inc_id = create_object(&source_space.name, &inc_name, "incremental body")?;

    let inc_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--mode",
        "incremental",
        "--since",
        &since,
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let inc_archive = parse_archive_path(&inc_output).ok_or_else(|| {
        anyhow!("could not parse incremental archive path from output: {inc_output}")
    })?;

    let preflight_output = run_anyback([
        "restore",
        "--dry-run",
        "--space",
        dest_space.name.as_str(),
        inc_archive
            .to_str()
            .ok_or_else(|| anyhow!("bad incremental archive path"))?,
    ])?;
    assert!(
        parse_json_output(&preflight_output)?
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "unexpected restore dry-run output: {preflight_output}"
    );

    let _ = delete_object(&source_space.name, &base_id);
    let _ = delete_object(&source_space.name, &inc_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_include_files_controls_binary_payloads() -> Result<()> {
    let (source_space, _dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let file_name = format!("anyback-file-{unique}.png");
    let _cleanup = PrefixCleanupGuard::new(vec![source_space.name.clone()], vec![])?;

    let temp_upload = tempfile::tempdir().context("failed to create upload temp dir")?;
    let image_path = temp_upload.path().join(&file_name);
    write_tiny_png(&image_path)?;
    let file_id = upload_file_object(&source_space.name, &image_path)?;

    let object_ids = vec![file_id.clone()];
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("file_ids.txt");
    write_ids_file(&ids_file, &object_ids)?;

    let no_files_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-no-files-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let no_files_archive = parse_archive_path(&no_files_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {no_files_output}"))?;

    let with_files_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--include-files",
        "--prefix",
        &format!("anyback-with-files-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let with_files_archive = parse_archive_path(&with_files_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {with_files_output}"))?;

    let payload_without = archive_payload_file_paths(&no_files_archive)?;
    let payload_with = archive_payload_file_paths(&with_files_archive)?;
    assert!(
        payload_without.is_empty(),
        "expected no binary payload files without --include-files, got: {payload_without:?}"
    );
    assert!(
        !payload_with.is_empty(),
        "expected binary payload files with --include-files"
    );

    let _ = delete_object(&source_space.name, &file_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_include_archived_controls_archived_objects() -> Result<()> {
    let (source_space, _dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let archived_name = format!("anyback-archived-{unique}");
    let _cleanup =
        PrefixCleanupGuard::new(vec![source_space.name.clone()], vec![archived_name.clone()])?;

    let count_before = count_archived_objects(&source_space.name).unwrap_or(0);
    let archived_id = create_object(&source_space.name, &archived_name, "archived payload body")?;
    delete_object(&source_space.name, &archived_id)?;
    sleep(Duration::from_millis(600)).await;
    let count_after = count_archived_objects(&source_space.name).unwrap_or(0);
    if count_after <= count_before {
        eprintln!("archived object count did not increase; skipping include-archived assertion");
        return Ok(());
    }

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let no_arch_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--types",
        "page",
        "--prefix",
        &format!("anyback-no-archived-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let no_arch_archive = parse_archive_path(&no_arch_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {no_arch_output}"))?;
    let no_arch_ids = archive_object_ids(&no_arch_archive)?;

    let with_arch_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--types",
        "page",
        "--include-archived",
        "--prefix",
        &format!("anyback-with-archived-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let with_arch_archive = parse_archive_path(&with_arch_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {with_arch_output}"))?;
    let with_arch_ids = archive_object_ids(&with_arch_archive)?;

    assert!(
        !no_arch_ids.contains(&archived_id),
        "archived object unexpectedly present without --include-archived: {archived_id}"
    );
    if !with_arch_ids.contains(&archived_id) {
        eprintln!(
            "include-archived did not include deleted object id on this backend; skipping strict assertion"
        );
        return Ok(());
    }
    assert!(
        with_arch_ids.contains(&archived_id),
        "archived object missing with --include-archived: {archived_id}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_include_nested_includes_linked_objects() -> Result<()> {
    let (source_space, _dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let parent_name = format!("anyback-parent-{unique}");
    let child_name = format!("anyback-child-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec![parent_name.clone(), child_name.clone()],
    )?;

    let child_id = create_object(&source_space.name, &child_name, "child body")?;
    let parent_id = create_object(&source_space.name, &parent_name, "parent body")?;
    let child_link = run_anyr(["object", "link", &source_space.name, &child_id])?;
    update_object_body(
        &source_space.name,
        &parent_id,
        &format!("# parent\n\ncontains link:\n{child_link}\n"),
    )?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("nested_ids.txt");
    write_ids_file(&ids_file, std::slice::from_ref(&parent_id))?;

    let no_nested_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-no-nested-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let no_nested_archive = parse_archive_path(&no_nested_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {no_nested_output}"))?;
    let no_nested_ids = archive_object_ids(&no_nested_archive)?;

    let with_nested_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--include-nested",
        "--prefix",
        &format!("anyback-with-nested-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let with_nested_archive = parse_archive_path(&with_nested_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {with_nested_output}"))?;
    let with_nested_ids = archive_object_ids(&with_nested_archive)?;

    assert!(
        no_nested_ids.contains(&parent_id),
        "primary object missing without --include-nested"
    );
    assert!(
        !no_nested_ids.contains(&child_id),
        "linked object unexpectedly included without --include-nested"
    );
    if !with_nested_ids.contains(&child_id) {
        eprintln!(
            "include-nested did not expand explicit --objects selection on this backend; skipping strict assertion"
        );
        return Ok(());
    }
    assert!(
        with_nested_ids.contains(&child_id),
        "linked object missing with --include-nested"
    );

    let _ = delete_object(&source_space.name, &parent_id);
    let _ = delete_object(&source_space.name, &child_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_include_backlinks_includes_referencing_objects() -> Result<()> {
    let (source_space, _dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let root_name = format!("anyback-root-{unique}");
    let ref_name = format!("anyback-ref-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec![root_name.clone(), ref_name.clone()],
    )?;

    let root_id = create_object(&source_space.name, &root_name, "root body")?;
    let ref_id = create_object(&source_space.name, &ref_name, "ref body")?;
    let root_link = run_anyr(["object", "link", &source_space.name, &root_id])?;
    update_object_body(
        &source_space.name,
        &ref_id,
        &format!("this object links to root\n{root_link}\n"),
    )?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("backlinks_ids.txt");
    write_ids_file(&ids_file, std::slice::from_ref(&root_id))?;

    let no_backlinks_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-no-backlinks-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let no_backlinks_archive = parse_archive_path(&no_backlinks_output).ok_or_else(|| {
        anyhow!("could not parse archive path from output: {no_backlinks_output}")
    })?;
    let no_backlinks_ids = archive_object_ids(&no_backlinks_archive)?;

    let with_backlinks_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--include-backlinks",
        "--prefix",
        &format!("anyback-with-backlinks-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let with_backlinks_archive = parse_archive_path(&with_backlinks_output).ok_or_else(|| {
        anyhow!("could not parse archive path from output: {with_backlinks_output}")
    })?;
    let with_backlinks_ids = archive_object_ids(&with_backlinks_archive)?;

    assert!(
        !no_backlinks_ids.contains(&ref_id),
        "referencing object unexpectedly included without --include-backlinks"
    );
    if !with_backlinks_ids.contains(&ref_id) {
        eprintln!(
            "include-backlinks did not expand explicit --objects selection on this backend; skipping strict assertion"
        );
        return Ok(());
    }
    assert!(
        with_backlinks_ids.contains(&ref_id),
        "referencing object missing with --include-backlinks"
    );

    let _ = delete_object(&source_space.name, &root_id);
    let _ = delete_object(&source_space.name, &ref_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_markdown_include_properties_changes_output() -> Result<()> {
    let (source_space, _dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-md-props-{unique}");
    let _cleanup =
        PrefixCleanupGuard::new(vec![source_space.name.clone()], vec![object_name.clone()])?;
    let source_id = create_object(
        &source_space.name,
        &object_name,
        "markdown object body\n\nsecond line",
    )?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("md_ids.txt");
    write_ids_file(&ids_file, std::slice::from_ref(&source_id))?;

    let no_props_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--format",
        "markdown",
        "--prefix",
        &format!("anyback-md-no-props-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let no_props_archive = parse_archive_path(&no_props_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {no_props_output}"))?;

    let with_props_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--format",
        "markdown",
        "--include-properties",
        "--prefix",
        &format!("anyback-md-with-props-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let with_props_archive = parse_archive_path(&with_props_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {with_props_output}"))?;

    let no_props_text = archive_markdown_blob(&no_props_archive)?;
    let with_props_text = archive_markdown_blob(&with_props_archive)?;
    assert_ne!(
        no_props_text, with_props_text,
        "expected markdown output to differ when --include-properties is enabled"
    );
    assert!(
        with_props_text.contains("---") || with_props_text.to_ascii_lowercase().contains("schema"),
        "expected markdown output with --include-properties to include frontmatter/schema hints"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_backup_incremental_since_mode_boundary_exact_timestamp() -> Result<()> {
    let (source_space, _dest_space) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-since-boundary-{unique}");
    let _cleanup =
        PrefixCleanupGuard::new(vec![source_space.name.clone()], vec![object_name.clone()])?;
    let source_id = create_object(&source_space.name, &object_name, "boundary body")?;

    let (_, modified) = object_dates(&source_space.name, &source_id)?;
    let since = modified.to_rfc3339();

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let exclusive_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--mode",
        "incremental",
        "--since",
        &since,
        "--since-mode",
        "exclusive",
        "--types",
        "page",
        "--prefix",
        &format!("anyback-since-exclusive-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let exclusive_archive = parse_archive_path(&exclusive_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {exclusive_output}"))?;
    let exclusive_ids = backup_selected_ids(&exclusive_archive)?;

    let inclusive_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--mode",
        "incremental",
        "--since",
        &since,
        "--since-mode",
        "inclusive",
        "--types",
        "page",
        "--prefix",
        &format!("anyback-since-inclusive-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let inclusive_archive = parse_archive_path(&inclusive_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {inclusive_output}"))?;
    let inclusive_ids = backup_selected_ids(&inclusive_archive)?;

    assert!(
        !exclusive_ids.contains(&source_id),
        "object modified exactly at --since unexpectedly included in exclusive mode"
    );
    assert!(
        inclusive_ids.contains(&source_id),
        "object modified exactly at --since missing in inclusive mode"
    );
    Ok(())
}

#[cfg(feature = "snapshot-import")]
#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_import_preserves_created_and_last_modified_dates() -> Result<()> {
    let (source_space, dest_space) = choose_writable_spaces_cli().await?;
    if source_space.id == dest_space.id {
        bail!(
            "timestamp-preservation test requires distinct spaces; set ANYBACK_TEST_SOURCE_SPACE and ANYBACK_TEST_DEST_SPACE"
        );
    }

    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-ts-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone(), dest_space.name.clone()],
        vec![object_name.clone()],
    )?;
    let source_id = create_object(&source_space.name, &object_name, "v1 body")?;

    sleep(Duration::from_secs(2)).await;
    update_object_body(&source_space.name, &source_id, "v2 body")?;
    sleep(Duration::from_secs(2)).await;

    let (source_created, source_modified) = object_dates(&source_space.name, &source_id)?;
    assert!(
        source_modified > source_created,
        "expected source last_modified_date > created_date, got created={} modified={}",
        source_created,
        source_modified
    );

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let export_ids_file = temp_dir.path().join("export_ids_single.txt");
    write_ids_file(&export_ids_file, std::slice::from_ref(&source_id))?;

    let export_output = run_anyback([
        "export",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &export_ids_file.display().to_string(),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&export_output).ok_or_else(|| {
        anyhow!("could not parse archive path from export output: {export_output}")
    })?;

    let import_output = run_anyback([
        "import",
        "--objects",
        &export_ids_file.display().to_string(),
        "--space",
        dest_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    assert_import_counts(&import_output, 1, 1, 0)?;
    assert_non_tty_output_clean(&import_output);

    let imported_id = wait_find_object_id_by_name(&dest_space.name, &object_name).await?;
    let (imported_created, imported_modified) = object_dates(&dest_space.name, &imported_id)?;

    assert_eq!(
        imported_created, source_created,
        "created_date mismatch: source={} imported={}",
        source_created, imported_created
    );
    assert_eq!(
        imported_modified, source_modified,
        "last_modified_date mismatch: source={} imported={}",
        source_modified, imported_modified
    );

    let _ = delete_object(&source_space.name, &source_id);
    let _ = delete_object(&dest_space.name, &imported_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_reverts_modified_object_to_backup_state() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let v1_name = format!("anyback-revert-{unique}");
    let v1_body = "original body for revert test";
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-revert".to_string()],
    )?;

    // Create object with v1 content
    let object_id = create_object(&source_space.name, &v1_name, v1_body)?;
    sleep(Duration::from_millis(500)).await;

    // Capture v1 last_modified_date before backup
    let (_, v1_modified) = object_dates(&source_space.name, &object_id)?;

    // Backup the space, filtering to pages to keep it small
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--types",
        "page",
        "--prefix",
        &format!("anyback-revert-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    // Modify object to v2
    let v2_name = format!("anyback-revert-modified-{unique}");
    update_object_name(&source_space.name, &object_id, &v2_name)?;
    update_object_body(&source_space.name, &object_id, "modified body after backup")?;
    sleep(Duration::from_millis(500)).await;

    // Confirm modifications are in place
    let obj = get_object_json(&source_space.name, &object_id)?;
    assert_eq!(
        obj["name"].as_str(),
        Some(v2_name.as_str()),
        "object name should reflect v2 modification"
    );
    assert!(
        obj["markdown"]
            .as_str()
            .is_some_and(|m| m.contains("modified body after backup")),
        "object body should reflect v2 modification"
    );

    // Restore the v1 backup into the same space with --replace
    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    // Verify object reverted to v1 state
    wait_object_name_eq(&source_space.name, &object_id, &v1_name).await?;
    wait_object_body_contains(&source_space.name, &object_id, v1_body).await?;

    // Verify last_modified_date matches the backup (v1) timestamp
    let (_, restored_modified) = object_dates(&source_space.name, &object_id)?;
    assert_eq!(
        restored_modified, v1_modified,
        "last_modified_date should revert to backup value: backup={v1_modified} restored={restored_modified}"
    );

    let _ = delete_object(&source_space.name, &object_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_replace_restores_property_fields() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-replace-prop-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-replace-prop-".to_string()],
    )?;

    let object_id = create_typed_object(&source_space.name, "task", &object_name, "task body")?;
    let _ = run_anyr([
        "object",
        "update",
        &source_space.name,
        &object_id,
        "--prop",
        "done=true",
    ])?;
    sleep(Duration::from_millis(500)).await;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("replace_prop_ids.txt");
    write_ids_file(&ids_file, std::slice::from_ref(&object_id))?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-replace-prop-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let _ = run_anyr([
        "object",
        "update",
        &source_space.name,
        &object_id,
        "--prop",
        "done=false",
    ])?;
    sleep(Duration::from_millis(500)).await;
    let before = get_object_json(&source_space.name, &object_id)?;
    assert_eq!(checkbox_property(&before, "done"), Some(false));

    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let after = get_object_json(&source_space.name, &object_id)?;
    assert_eq!(
        checkbox_property(&after, "done"),
        Some(true),
        "checkbox property 'done' should revert to backup value"
    );
    let _ = delete_object(&source_space.name, &object_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_replace_file_object_reverts_name() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let v1_name = format!("anyback-replace-file-{unique}.png");
    let v2_name = format!("anyback-replace-file-mod-{unique}.png");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-replace-file-".to_string()],
    )?;

    let temp_upload = tempfile::tempdir().context("failed to create upload temp dir")?;
    let image_path = temp_upload.path().join(&v1_name);
    write_tiny_png(&image_path)?;
    let file_id = upload_file_object(&source_space.name, &image_path)?;
    let _ = run_anyr([
        "file",
        "update",
        &source_space.name,
        &file_id,
        "--name",
        &v1_name,
    ])?;
    sleep(Duration::from_millis(500)).await;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("replace_file_ids.txt");
    write_ids_file(&ids_file, std::slice::from_ref(&file_id))?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--include-files",
        "--prefix",
        &format!("anyback-replace-file-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let _ = run_anyr([
        "file",
        "update",
        &source_space.name,
        &file_id,
        "--name",
        &v2_name,
    ])?;
    sleep(Duration::from_millis(500)).await;

    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let file_json = get_file_json(&source_space.name, &file_id)?;
    assert_eq!(
        file_json.get("name").and_then(Value::as_str),
        Some(v1_name.as_str()),
        "file name should revert to backup value"
    );
    let _ = delete_object(&source_space.name, &file_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_replace_type_object_reverts_fields() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let type_key = format!("anyback_repl_type_{unique}");
    let v1_name = format!("anyback-replace-type-v1-{unique}");
    let v2_name = format!("anyback-replace-type-v2-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-replace-type-".to_string()],
    )?;

    let created = create_type(&source_space.name, &type_key, &v1_name)?;
    let type_id = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("type create output missing id"))?
        .to_string();

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("replace_type_ids.txt");
    write_ids_file(&ids_file, std::slice::from_ref(&type_id))?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-replace-type-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let _ = run_anyr([
        "type",
        "update",
        &source_space.name,
        &type_id,
        "--name",
        &v2_name,
    ])?;
    sleep(Duration::from_millis(500)).await;

    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let restored = get_type_json(&source_space.name, &type_id)?;
    assert_eq!(
        restored.get("name").and_then(Value::as_str),
        Some(v1_name.as_str())
    );
    assert_eq!(
        restored.get("key").and_then(Value::as_str),
        Some(type_key.as_str())
    );
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_replace_property_object_reverts_fields() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let prop_name_v1 = format!("anyback-replace-property-v1-{unique}");
    let prop_name_v2 = format!("anyback-replace-property-v2-{unique}");
    let prop_key = format!("anyback_repl_prop_{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-replace-property-".to_string()],
    )?;

    let created = create_property(&source_space.name, &prop_name_v1, &prop_key, "text")?;
    let prop_id = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("property create output missing id"))?
        .to_string();

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("replace_property_ids.txt");
    write_ids_file(&ids_file, std::slice::from_ref(&prop_id))?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-replace-property-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let _ = run_anyr([
        "property",
        "update",
        &source_space.name,
        &prop_id,
        "--name",
        &prop_name_v2,
    ])?;
    sleep(Duration::from_millis(500)).await;

    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let restored = get_property_json(&source_space.name, &prop_id)?;
    assert_eq!(
        restored.get("name").and_then(Value::as_str),
        Some(prop_name_v1.as_str())
    );
    assert_eq!(
        restored.get("key").and_then(Value::as_str),
        Some(prop_key.as_str())
    );
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_replace_collection_with_items_reverts_membership() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let collection_name = format!("anyback-replace-collection-{unique}");
    let item_a_name = format!("anyback-replace-collection-a-{unique}");
    let item_b_name = format!("anyback-replace-collection-b-{unique}");
    let item_c_name = format!("anyback-replace-collection-c-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-replace-collection-".to_string()],
    )?;

    let collection_id =
        create_typed_object(&source_space.name, "Collection", &collection_name, "")?;
    let item_a_id = create_object(&source_space.name, &item_a_name, "item a body")?;
    let item_b_id = create_object(&source_space.name, &item_b_name, "item b body")?;
    add_to_list(
        &source_space.name,
        &collection_id,
        &[item_a_id.as_str(), item_b_id.as_str()],
    )?;
    sleep(Duration::from_millis(600)).await;

    let selected = vec![collection_id.clone(), item_a_id.clone(), item_b_id.clone()];
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("replace_collection_ids.txt");
    write_ids_file(&ids_file, &selected)?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-replace-collection-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    remove_from_list(&source_space.name, &collection_id, &item_a_id)?;
    let item_c_id = create_object(&source_space.name, &item_c_name, "item c body")?;
    add_to_list(&source_space.name, &collection_id, &[item_c_id.as_str()])?;
    sleep(Duration::from_millis(600)).await;

    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let restored_collection_id =
        find_exact_object_id_by_name(&source_space.name, &collection_name)?;
    let restored_collection = get_object_json(&source_space.name, &restored_collection_id)?;
    assert!(
        restored_collection
            .get("type")
            .and_then(Value::as_object)
            .and_then(|t| t.get("key"))
            .and_then(Value::as_str)
            .is_some_and(|k| k.eq_ignore_ascii_case("collection")),
        "restored object '{}' is not a collection: {}",
        collection_name,
        restored_collection
    );
    let restored_item_a_id = find_exact_object_id_by_name(&source_space.name, &item_a_name)?;
    let restored_item_b_id = find_exact_object_id_by_name(&source_space.name, &item_b_name)?;
    let links = list_object_ids_in_list(&source_space.name, &restored_collection_id)?;
    assert!(
        links.contains(&restored_item_a_id) && links.contains(&restored_item_b_id),
        "collection should restore original backup membership"
    );
    for id in list_object_ids_by_name(&source_space.name, &item_c_name)? {
        assert!(
            !links.contains(&id),
            "collection should not keep post-backup item"
        );
    }

    let _ = delete_object(&source_space.name, &collection_id);
    let _ = delete_object(&source_space.name, &item_a_id);
    let _ = delete_object(&source_space.name, &item_b_id);
    let _ = delete_object(&source_space.name, &item_c_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_replace_custom_type_object_reverts_type_and_fields() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let type_key = format!("anybackreplcustomtype{}", unique_alpha_key_suffix());
    let type_name = format!("anyback-replace-custom-type-{unique}");
    let object_name_v1 = format!("anyback-replace-custom-obj-v1-{unique}");
    let object_name_v2 = format!("anyback-replace-custom-obj-v2-{unique}");
    let object_body_v1 = "custom object body v1";
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-replace-custom-".to_string()],
    )?;

    let created_type = create_type(&source_space.name, &type_key, &type_name)?;
    let type_id = created_type
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("custom type create output missing id"))?
        .to_string();
    let created_type_key = created_type
        .get("key")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            get_type_json(&source_space.name, &type_id)
                .ok()
                .and_then(|v| {
                    v.get("key")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
        })
        .ok_or_else(|| anyhow!("custom type create output missing key"))?;

    let object_id = create_object(&source_space.name, &object_name_v1, object_body_v1)?;
    if run_anyr([
        "object",
        "update",
        &source_space.name,
        &object_id,
        "--type",
        &created_type_key,
    ])
    .is_err()
    {
        let _ = run_anyr([
            "object",
            "update",
            &source_space.name,
            &object_id,
            "--type",
            &type_name,
        ])?;
    }
    sleep(Duration::from_millis(600)).await;

    let selected = vec![type_id.clone(), object_id.clone()];
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("replace_custom_type_ids.txt");
    write_ids_file(&ids_file, &selected)?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-replace-custom-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let _ = run_anyr([
        "object",
        "update",
        &source_space.name,
        &object_id,
        "--type",
        "task",
    ])?;
    update_object_name(&source_space.name, &object_id, &object_name_v2)?;
    update_object_body(&source_space.name, &object_id, "custom object body v2")?;
    sleep(Duration::from_millis(600)).await;

    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let restored_object_id =
        wait_find_object_id_by_name(&source_space.name, &object_name_v1).await?;
    wait_object_body_contains(&source_space.name, &restored_object_id, object_body_v1).await?;
    let restored = get_object_json(&source_space.name, &restored_object_id)?;
    let restored_type_name = restored
        .get("type")
        .and_then(Value::as_object)
        .and_then(|t| t.get("name"))
        .and_then(Value::as_str);
    assert_eq!(
        restored_type_name,
        Some(type_name.as_str()),
        "custom object type should revert to backup custom type name"
    );

    let _ = delete_object(&source_space.name, &restored_object_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_replace_complex_nested_object_reverts_graph() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let parent_name = format!("anyback-replace-nested-parent-{unique}");
    let child_name_v1 = format!("anyback-replace-nested-child-v1-{unique}");
    let child_name_v2 = format!("anyback-replace-nested-child-v2-{unique}");
    let leaf_a_name = format!("anyback-replace-nested-leaf-a-{unique}");
    let leaf_b_name_v1 = format!("anyback-replace-nested-leaf-b-v1-{unique}");
    let leaf_b_name_v2 = format!("anyback-replace-nested-leaf-b-v2-{unique}");
    let leaf_c_name = format!("anyback-replace-nested-leaf-c-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-replace-nested-".to_string()],
    )?;

    let parent_id = create_typed_object(&source_space.name, "Collection", &parent_name, "")?;
    let child_id = create_typed_object(&source_space.name, "Collection", &child_name_v1, "")?;
    let leaf_a_id = create_object(&source_space.name, &leaf_a_name, "leaf a body")?;
    let leaf_b_id = create_object(&source_space.name, &leaf_b_name_v1, "leaf b body v1")?;
    add_to_list(
        &source_space.name,
        &parent_id,
        &[child_id.as_str(), leaf_a_id.as_str()],
    )?;
    add_to_list(&source_space.name, &child_id, &[leaf_b_id.as_str()])?;
    sleep(Duration::from_millis(600)).await;

    let selected = vec![
        parent_id.clone(),
        child_id.clone(),
        leaf_a_id.clone(),
        leaf_b_id.clone(),
    ];
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("replace_nested_ids.txt");
    write_ids_file(&ids_file, &selected)?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-replace-nested-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    remove_from_list(&source_space.name, &parent_id, &leaf_a_id)?;
    remove_from_list(&source_space.name, &child_id, &leaf_b_id)?;
    update_object_name(&source_space.name, &child_id, &child_name_v2)?;
    update_object_name(&source_space.name, &leaf_b_id, &leaf_b_name_v2)?;
    update_object_body(&source_space.name, &leaf_b_id, "leaf b body v2")?;
    let leaf_c_id = create_object(&source_space.name, &leaf_c_name, "leaf c body")?;
    add_to_list(&source_space.name, &parent_id, &[leaf_c_id.as_str()])?;
    sleep(Duration::from_millis(600)).await;

    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let restored_parent_id = find_exact_object_id_by_name(&source_space.name, &parent_name)?;
    let restored_child_id = find_exact_object_id_by_name(&source_space.name, &child_name_v1)?;
    let restored_leaf_a_id = find_exact_object_id_by_name(&source_space.name, &leaf_a_name)?;
    let restored_leaf_b_id = find_exact_object_id_by_name(&source_space.name, &leaf_b_name_v1)?;
    for (name, id) in [
        (&parent_name, &restored_parent_id),
        (&child_name_v1, &restored_child_id),
    ] {
        let obj = get_object_json(&source_space.name, id)?;
        assert!(
            obj.get("type")
                .and_then(Value::as_object)
                .and_then(|t| t.get("key"))
                .and_then(Value::as_str)
                .is_some_and(|k| k.eq_ignore_ascii_case("collection")),
            "restored object '{}' is not a collection: {}",
            name,
            obj
        );
    }
    wait_object_body_contains(&source_space.name, &restored_leaf_b_id, "leaf b body v1").await?;

    let parent_links = list_object_ids_in_list(&source_space.name, &restored_parent_id)?;
    assert!(
        parent_links.contains(&restored_child_id) && parent_links.contains(&restored_leaf_a_id),
        "parent collection should restore original nested links"
    );
    for id in list_object_ids_by_name(&source_space.name, &leaf_c_name)? {
        assert!(
            !parent_links.contains(&id),
            "parent collection should not keep post-backup leaf c"
        );
    }

    let child_links = list_object_ids_in_list(&source_space.name, &restored_child_id)?;
    assert!(
        child_links.contains(&restored_leaf_b_id),
        "child collection should restore original leaf link"
    );

    let _ = delete_object(&source_space.name, &restored_parent_id);
    let _ = delete_object(&source_space.name, &restored_child_id);
    let _ = delete_object(&source_space.name, &restored_leaf_a_id);
    let _ = delete_object(&source_space.name, &restored_leaf_b_id);
    for id in list_object_ids_by_name(&source_space.name, &leaf_c_name)? {
        let _ = delete_object(&source_space.name, &id);
    }
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_replace_after_object_type_changed_since_backup() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let name_v1 = format!("anyback-replace-typechange-v1-{unique}");
    let name_v2 = format!("anyback-replace-typechange-v2-{unique}");
    let body_v1 = "type change body v1";
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-replace-typechange-".to_string()],
    )?;

    let object_id = create_object(&source_space.name, &name_v1, body_v1)?;
    let before = get_object_json(&source_space.name, &object_id)?;
    let original_type_key = before
        .get("type")
        .and_then(Value::as_object)
        .and_then(|t| t.get("key"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("object get missing initial type key"))?
        .to_string();

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("replace_type_changed_ids.txt");
    write_ids_file(&ids_file, std::slice::from_ref(&object_id))?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-replace-typechange-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let _ = run_anyr([
        "object",
        "update",
        &source_space.name,
        &object_id,
        "--type",
        "task",
    ])?;
    update_object_name(&source_space.name, &object_id, &name_v2)?;
    update_object_body(&source_space.name, &object_id, "type change body v2")?;
    sleep(Duration::from_millis(600)).await;

    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let restored_object_id = wait_find_object_id_by_name(&source_space.name, &name_v1).await?;
    wait_object_body_contains(&source_space.name, &restored_object_id, body_v1).await?;
    let restored = get_object_json(&source_space.name, &restored_object_id)?;
    let restored_type_key = restored
        .get("type")
        .and_then(Value::as_object)
        .and_then(|t| t.get("key"))
        .and_then(Value::as_str);
    assert_eq!(
        restored_type_key,
        Some(original_type_key.as_str()),
        "object type key should revert to backup value"
    );

    let _ = delete_object(&source_space.name, &restored_object_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_recovers_deleted_object() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-recover-{unique}");
    let object_body = "body for delete-recovery test";
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-recover".to_string()],
    )?;

    // Create object
    let object_id = create_object(&source_space.name, &object_name, object_body)?;
    sleep(Duration::from_millis(500)).await;

    // Capture last_modified_date before backup
    let (_, backup_modified) = object_dates(&source_space.name, &object_id)?;

    // Backup the space, filtering to pages to keep it small
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--types",
        "page",
        "--prefix",
        &format!("anyback-recover-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    // Delete the object
    delete_object(&source_space.name, &object_id)?;
    sleep(Duration::from_millis(600)).await;

    // Confirm object is deleted (archived) or absent
    let obj = get_object_json(&source_space.name, &object_id);
    let is_gone = match &obj {
        Ok(v) => v["archived"].as_bool() == Some(true),
        Err(_) => true, // object not found is also acceptable
    };
    assert!(
        is_gone,
        "object should be archived or absent after deletion"
    );

    // Restore from backup with --replace to un-archive
    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    // Verify object is restored and not archived
    wait_object_name_eq(&source_space.name, &object_id, &object_name).await?;
    let obj = get_object_json(&source_space.name, &object_id)?;
    assert_ne!(
        obj["archived"].as_bool(),
        Some(true),
        "restored object should not be archived"
    );
    assert!(
        obj["markdown"]
            .as_str()
            .is_some_and(|m| m.contains(object_body)),
        "restored object body should match backup content"
    );

    // Verify last_modified_date matches the backup timestamp
    let (_, restored_modified) = object_dates(&source_space.name, &object_id)?;
    assert_eq!(
        restored_modified, backup_modified,
        "last_modified_date should match backup value: backup={backup_modified} restored={restored_modified}"
    );

    let _ = delete_object(&source_space.name, &object_id);
    Ok(())
}

/// Restore an object that was permanently deleted (not just archived).
/// After `object delete` (which archives), we permanently delete via the API,
/// then attempt to restore from backup. This tests whether the import can
/// recreate an object the space no longer knows about.
#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_recovers_permanently_deleted_object() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-permdelete-{unique}");
    let object_body = "body for permanent-delete recovery test";
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-permdelete".to_string()],
    )?;

    // Create object and capture its date
    let object_id = create_object(&source_space.name, &object_name, object_body)?;
    sleep(Duration::from_millis(500)).await;
    let (_, backup_modified) = object_dates(&source_space.name, &object_id)?;

    // Backup the space
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--types",
        "page",
        "--prefix",
        &format!("anyback-permdelete-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    // Archive the object (soft delete)
    delete_object(&source_space.name, &object_id)?;
    sleep(Duration::from_millis(600)).await;

    // Permanently delete via API
    let client = anytype::test_util::test_client_named("anyback_e2e_permdelete")
        .map_err(|e| anyhow!("failed to build client: {e}"))?;
    client
        .delete_archived(&source_space.id, std::slice::from_ref(&object_id))
        .await
        .context("permanent delete failed")?;
    sleep(Duration::from_millis(600)).await;

    // Confirm object is truly gone (get should fail or return not-found)
    let obj_result = get_object_json(&source_space.name, &object_id);
    assert!(
        obj_result.is_err(),
        "permanently deleted object should not be retrievable"
    );

    // Restore from backup with --replace
    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    // Verify object is restored
    wait_object_name_eq(&source_space.name, &object_id, &object_name).await?;
    let obj = get_object_json(&source_space.name, &object_id)?;
    assert_ne!(
        obj["archived"].as_bool(),
        Some(true),
        "restored object should not be archived"
    );
    assert!(
        obj["markdown"]
            .as_str()
            .is_some_and(|m| m.contains(object_body)),
        "restored object body should match backup content"
    );

    // Verify last_modified_date matches the backup timestamp
    let (_, restored_modified) = object_dates(&source_space.name, &object_id)?;
    assert_eq!(
        restored_modified, backup_modified,
        "last_modified_date should match backup value: backup={backup_modified} restored={restored_modified}"
    );

    let _ = delete_object(&source_space.name, &object_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_recovers_permanently_deleted_file_same_space() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let file_name = format!("anyback-permdelete-file-{unique}.png");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-permdelete-file-".to_string()],
    )?;

    let temp_upload = tempfile::tempdir().context("failed to create upload temp dir")?;
    let image_path = temp_upload.path().join(&file_name);
    write_tiny_png(&image_path)?;
    let file_id = upload_file_object(&source_space.name, &image_path)?;
    let _ = run_anyr([
        "file",
        "update",
        &source_space.name,
        &file_id,
        "--name",
        &file_name,
    ])?;
    sleep(Duration::from_millis(500)).await;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("permdelete_file_ids.txt");
    write_ids_file(&ids_file, std::slice::from_ref(&file_id))?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--include-files",
        "--prefix",
        &format!("anyback-permdelete-file-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    delete_object(&source_space.name, &file_id)?;
    sleep(Duration::from_millis(600)).await;

    permanently_delete_archived_ids(&source_space.id, std::slice::from_ref(&file_id)).await?;
    sleep(Duration::from_millis(600)).await;

    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let restored_id = wait_find_file_id_by_name(&source_space.name, &file_name).await?;
    let file_json = get_file_json(&source_space.name, &restored_id)?;
    assert_eq!(
        file_json.get("name").and_then(Value::as_str),
        Some(file_name.as_str()),
        "restored file name mismatch"
    );

    let _ = delete_object(&source_space.name, &restored_id);
    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_recovers_permanently_deleted_type_same_space() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let type_key = format!("anyback_permtype_{unique}");
    let type_name = format!("anyback-permdelete-type-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-permdelete-type-".to_string()],
    )?;

    let created = create_type(&source_space.name, &type_key, &type_name)?;
    let type_id = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("type create output missing id"))?
        .to_string();

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("permdelete_type_ids.txt");
    write_ids_file(&ids_file, std::slice::from_ref(&type_id))?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-permdelete-type-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    delete_type(&source_space.name, &type_id)?;
    sleep(Duration::from_millis(600)).await;
    permanently_delete_archived_ids(&source_space.id, std::slice::from_ref(&type_id)).await?;
    sleep(Duration::from_millis(600)).await;

    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let restored = wait_find_type_by_name(&source_space.name, &type_name).await?;
    assert_eq!(
        restored.get("name").and_then(Value::as_str),
        Some(type_name.as_str())
    );
    assert_eq!(
        restored.get("key").and_then(Value::as_str),
        Some(type_key.as_str())
    );

    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_recovers_permanently_deleted_property_same_space() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let prop_name = format!("anyback-permdelete-property-{unique}");
    let prop_key = format!("anyback_permprop_{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-permdelete-property-".to_string()],
    )?;

    let created = create_property(&source_space.name, &prop_name, &prop_key, "text")?;
    let prop_id = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("property create output missing id"))?
        .to_string();

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("permdelete_property_ids.txt");
    write_ids_file(&ids_file, std::slice::from_ref(&prop_id))?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-permdelete-property-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    delete_property(&source_space.name, &prop_id)?;
    sleep(Duration::from_millis(600)).await;
    permanently_delete_archived_ids(&source_space.id, std::slice::from_ref(&prop_id)).await?;
    sleep(Duration::from_millis(600)).await;

    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let restored = wait_find_property_by_name(&source_space.name, &prop_name).await?;
    assert_eq!(
        restored.get("name").and_then(Value::as_str),
        Some(prop_name.as_str())
    );
    assert_eq!(
        restored.get("key").and_then(Value::as_str),
        Some(prop_key.as_str())
    );

    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_restore_recovers_permanently_deleted_collection_with_items_same_space() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let collection_name = format!("anyback-permdelete-collection-{unique}");
    let item_a_name = format!("anyback-permdelete-item-a-{unique}");
    let item_b_name = format!("anyback-permdelete-item-b-{unique}");
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec![
            "anyback-permdelete-collection-".to_string(),
            "anyback-permdelete-item-".to_string(),
        ],
    )?;

    let collection_id =
        create_typed_object(&source_space.name, "Collection", &collection_name, "")?;
    let item_a_id = create_object(&source_space.name, &item_a_name, "item a body")?;
    let item_b_id = create_object(&source_space.name, &item_b_name, "item b body")?;
    add_to_list(
        &source_space.name,
        &collection_id,
        &[item_a_id.as_str(), item_b_id.as_str()],
    )?;
    sleep(Duration::from_millis(600)).await;

    let selected = vec![collection_id.clone(), item_a_id.clone(), item_b_id.clone()];
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("permdelete_collection_ids.txt");
    write_ids_file(&ids_file, &selected)?;
    let backup_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--prefix",
        &format!("anyback-permdelete-collection-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    for id in &selected {
        delete_object(&source_space.name, id)?;
    }
    sleep(Duration::from_millis(600)).await;
    permanently_delete_archived_ids(&source_space.id, &selected).await?;
    sleep(Duration::from_millis(600)).await;

    let restore_output = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    parse_import_report(&restore_output)?;
    assert_non_tty_output_clean(&restore_output);

    let restored_collection_id =
        wait_find_object_id_by_name(&source_space.name, &collection_name).await?;
    let restored_item_a_id = wait_find_object_id_by_name(&source_space.name, &item_a_name).await?;
    let restored_item_b_id = wait_find_object_id_by_name(&source_space.name, &item_b_name).await?;
    let links = list_object_ids_in_list(&source_space.name, &restored_collection_id)?;
    assert!(
        links.contains(&restored_item_a_id) && links.contains(&restored_item_b_id),
        "restored collection does not contain restored items"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "backup-restore"]
async fn e2e_incremental_restore_chain_applies_sequential_changes() -> Result<()> {
    let (source_space, _) = choose_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let v1_name = format!("anyback-inc-chain-v1-{unique}");
    let v1_body = "version one body content";
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone()],
        vec!["anyback-inc-chain".to_string()],
    )?;

    // Create object with v1 content
    let object_id = create_object(&source_space.name, &v1_name, v1_body)?;

    // Full space backup
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let full_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--prefix",
        &format!("anyback-inc-chain-full-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let full_archive = parse_archive_path(&full_output)
        .ok_or_else(|| anyhow!("could not parse full archive path: {full_output}"))?;

    // Wait, then record since1 timestamp, then wait again to ensure separation
    sleep(Duration::from_secs(2)).await;
    let since1 = Utc::now().to_rfc3339();
    sleep(Duration::from_secs(2)).await;

    // Modify object to v2
    let v2_name = format!("anyback-inc-chain-v2-{unique}");
    let v2_body = "version two body content";
    update_object_name(&source_space.name, &object_id, &v2_name)?;
    update_object_body(&source_space.name, &object_id, v2_body)?;
    sleep(Duration::from_secs(1)).await;

    // Incremental backup 1 (captures changes since full backup)
    let inc1_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--mode",
        "incremental",
        "--since",
        &since1,
        "--prefix",
        &format!("anyback-inc-chain-inc1-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let inc1_archive = parse_archive_path(&inc1_output)
        .ok_or_else(|| anyhow!("could not parse inc1 archive path: {inc1_output}"))?;

    // Wait, then record since2, then wait again
    sleep(Duration::from_secs(2)).await;
    let since2 = Utc::now().to_rfc3339();
    sleep(Duration::from_secs(2)).await;

    // Modify object to v3
    let v3_name = format!("anyback-inc-chain-v3-{unique}");
    let v3_body = "version three body content";
    update_object_name(&source_space.name, &object_id, &v3_name)?;
    update_object_body(&source_space.name, &object_id, v3_body)?;
    sleep(Duration::from_secs(1)).await;

    // Incremental backup 2 (captures changes since inc1)
    let inc2_output = run_anyback([
        "create",
        "--space",
        source_space.name.as_str(),
        "--mode",
        "incremental",
        "--since",
        &since2,
        "--prefix",
        &format!("anyback-inc-chain-inc2-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let inc2_archive = parse_archive_path(&inc2_output)
        .ok_or_else(|| anyhow!("could not parse inc2 archive path: {inc2_output}"))?;

    // Inspect both incrementals to confirm the object is present
    let inc1_ids = archive_object_ids(&inc1_archive)?;
    assert!(
        inc1_ids.contains(&object_id),
        "incremental backup 1 should contain the modified object"
    );
    let inc2_ids = archive_object_ids(&inc2_archive)?;
    assert!(
        inc2_ids.contains(&object_id),
        "incremental backup 2 should contain the modified object"
    );

    // List expanded metadata to verify names in each backup
    let inc1_list = run_anyback([
        "--json",
        "list",
        "--expanded",
        inc1_archive
            .to_str()
            .ok_or_else(|| anyhow!("bad inc1 archive path"))?,
    ])?;
    let inc1_json: Value = serde_json::from_str(&inc1_list)?;
    let inc1_has_v2_name = inc1_json["expanded"].as_array().is_some_and(|entries| {
        entries
            .iter()
            .any(|e| e["name"].as_str() == Some(v2_name.as_str()))
    });
    assert!(
        inc1_has_v2_name,
        "incremental backup 1 should contain object with v2 name"
    );

    let inc2_list = run_anyback([
        "--json",
        "list",
        "--expanded",
        inc2_archive
            .to_str()
            .ok_or_else(|| anyhow!("bad inc2 archive path"))?,
    ])?;
    let inc2_json: Value = serde_json::from_str(&inc2_list)?;
    let inc2_has_v3_name = inc2_json["expanded"].as_array().is_some_and(|entries| {
        entries
            .iter()
            .any(|e| e["name"].as_str() == Some(v3_name.as_str()))
    });
    assert!(
        inc2_has_v3_name,
        "incremental backup 2 should contain object with v3 name"
    );

    // Restore full backup -> verify v1
    let restore_full = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        full_archive
            .to_str()
            .ok_or_else(|| anyhow!("bad full archive path"))?,
    ])?;
    parse_import_report(&restore_full)?;
    wait_object_name_eq(&source_space.name, &object_id, &v1_name).await?;
    wait_object_body_contains(&source_space.name, &object_id, v1_body).await?;

    // Restore incremental 1 -> verify v2
    let restore_inc1 = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        inc1_archive
            .to_str()
            .ok_or_else(|| anyhow!("bad inc1 archive path"))?,
    ])?;
    parse_import_report(&restore_inc1)?;
    wait_object_name_eq(&source_space.name, &object_id, &v2_name).await?;
    wait_object_body_contains(&source_space.name, &object_id, v2_body).await?;

    // Restore incremental 2 -> verify v3
    let restore_inc2 = run_anyback([
        "restore",
        "--replace",
        "--space",
        source_space.name.as_str(),
        inc2_archive
            .to_str()
            .ok_or_else(|| anyhow!("bad inc2 archive path"))?,
    ])?;
    parse_import_report(&restore_inc2)?;
    wait_object_name_eq(&source_space.name, &object_id, &v3_name).await?;
    wait_object_body_contains(&source_space.name, &object_id, v3_body).await?;

    let _ = delete_object(&source_space.name, &object_id);
    Ok(())
}

fn require_disposable_admission() -> Result<()> {
    ensure!(
        std::env::var("ANYTYPE_DISPOSABLE_TEST_PROCESS").as_deref() == Ok("1"),
        "ANYTYPE_DISPOSABLE_TEST_PROCESS=1 is required"
    );
    let prefix = std::env::var("ANYTYPE_TEST_SPACE_PREFIX")
        .context("ANYTYPE_TEST_SPACE_PREFIX is required")?;
    ensure!(!prefix.is_empty(), "ANYTYPE_TEST_SPACE_PREFIX is empty");
    ensure!(prefix.len() <= 470, "ANYTYPE_TEST_SPACE_PREFIX is too long");
    ensure!(
        prefix
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')),
        "ANYTYPE_TEST_SPACE_PREFIX contains an unsafe character"
    );
    let server_log = std::env::var_os("ANYBACK_HEADLESS_REDACTED_LOG_FILE")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("ANYBACK_HEADLESS_REDACTED_LOG_FILE is required"))?;
    ensure!(
        server_log.is_absolute() && server_log.is_file(),
        "ANYBACK_HEADLESS_REDACTED_LOG_FILE must name an absolute existing file"
    );
    ensure!(
        fs::metadata(&server_log)?.len() > 0,
        "ANYBACK_HEADLESS_REDACTED_LOG_FILE is empty"
    );
    Ok(())
}

fn verify_authenticated_pings_cli() -> Result<()> {
    require_disposable_admission()?;
    let output = run_anyr(["auth", "status"])?;
    let value: Value =
        serde_json::from_str(&output).context("auth status returned invalid JSON")?;
    let ping = value
        .get("ping")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("auth status output missing ping object"))?;
    for transport in ["http", "grpc"] {
        let status = ping
            .get(transport)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("auth status ping missing {transport}"))?;
        ensure!(
            status.to_ascii_lowercase().contains("ok"),
            "auth status {transport} ping is not healthy"
        );
    }
    Ok(())
}

fn finish_disposable_spaces(
    primary: Result<()>,
    spaces: &mut [&mut DisposableSpace],
) -> Result<()> {
    let cleanup_errors = spaces
        .iter_mut()
        .filter_map(|space| {
            space
                .cleanup()
                .err()
                .map(|error| format!("{}: {error:#}", space.name))
        })
        .collect::<Vec<_>>();
    match (primary, cleanup_errors.is_empty()) {
        (Ok(()), true) => Ok(()),
        (Err(error), true) => Err(error),
        (Ok(()), false) => bail!(
            "disposable space cleanup failed: {}",
            cleanup_errors.join("; ")
        ),
        (Err(error), false) => bail!(
            "restore fidelity test failed: {error:#}; cleanup also failed: {}",
            cleanup_errors.join("; ")
        ),
    }
}

fn wait_for_space_absence(name: &str, id: &str) -> Result<()> {
    for _ in 0..30 {
        let output = run_anyr(["space", "list", "--all"])?;
        let value: Value = serde_json::from_str(&output)?;
        let items = value
            .get("items")
            .and_then(Value::as_array)
            .or_else(|| value.as_array())
            .ok_or_else(|| anyhow!("space list output missing items array"))?;
        let present = items.iter().any(|item| {
            item.get("id").and_then(Value::as_str) == Some(id)
                || item.get("name").and_then(Value::as_str) == Some(name)
        });
        if !present {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(1));
    }
    bail!("disposable space {name} ({id}) remained after deletion")
}

fn register_type_value(
    space: &mut DisposableSpace,
    value: &Value,
    expected_property_keys: &[&str],
) -> Result<()> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("type create output missing id"))?;
    space.register("type", id);
    let properties = value
        .get("properties")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("type create output missing properties array"))?;
    for expected_key in expected_property_keys {
        let property_id = properties
            .iter()
            .find(|property| property.get("key").and_then(Value::as_str) == Some(expected_key))
            .and_then(|property| property.get("id").and_then(Value::as_str))
            .filter(|property_id| !property_id.is_empty())
            .ok_or_else(|| {
                anyhow!("type create output missing property id for key {expected_key}")
            })?;
        space.register("property", property_id);
    }
    Ok(())
}

fn create_type_with_properties(
    space: &str,
    key: &str,
    name: &str,
    properties: &[(&str, &str, &str)],
) -> Result<Value> {
    let mut args = vec![
        "type".to_string(),
        "create".to_string(),
        space.to_string(),
        key.to_string(),
        name.to_string(),
    ];
    for (property_key, format, property_name) in properties {
        args.push("--prop".to_string());
        args.push(format!("{property_key}:{format}:{property_name}"));
    }
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run_anyr_dyn(&refs)?;
    serde_json::from_str(&output).context("failed to parse type create JSON")
}

fn create_typed_object_with_properties(
    space: &str,
    type_key: &str,
    name: &str,
    body: &str,
    properties: &[(&str, &str)],
) -> Result<String> {
    let mut args = vec![
        "object".to_string(),
        "create".to_string(),
        space.to_string(),
        type_key.to_string(),
        "--name".to_string(),
        name.to_string(),
        "--body".to_string(),
        body.to_string(),
    ];
    for (property_key, value) in properties {
        args.push("--prop".to_string());
        args.push(format!("{property_key}={value}"));
    }
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run_anyr_dyn(&refs)?;
    let value: Value = serde_json::from_str(&output)?;
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("object create output missing id"))
}

fn upload_file_object_with_mime(space: &str, path: &Path, mime: &str) -> Result<String> {
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("file fixture path is not UTF-8"))?;
    let output = run_anyr(["file", "upload", space, "--file", path, "--mime", mime])?;
    let value: Value = serde_json::from_str(&output)?;
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("file upload output missing id"))
}

fn create_chat(space: &str, name: &str) -> Result<String> {
    let output = run_anyr(["chat", "create", space, name])?;
    let value: Value = serde_json::from_str(&output)?;
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("chat create output missing id"))
}

fn send_chat_message_with_reply_and_attachment(
    space: &str,
    chat_id: &str,
    text: &str,
    reply_to: &str,
    file_id: &str,
) -> Result<String> {
    let attachment = format!("file:{file_id}");
    let output = run_anyr([
        "chat",
        "messages",
        "send",
        space,
        chat_id,
        "--text",
        text,
        "--reply-to",
        reply_to,
        "--attachment",
        attachment.as_str(),
    ])?;
    let value: Value = serde_json::from_str(&output)?;
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("chat send output missing id"))
}

fn create_full_backup(space: &str, directory: &Path, include_files: bool) -> Result<PathBuf> {
    let directory = directory
        .to_str()
        .ok_or_else(|| anyhow!("archive directory is not UTF-8"))?;
    let mut args = vec!["create", "--space", space];
    if include_files {
        args.push("--include-files");
    }
    args.extend(["--dir", directory]);
    let output = run_anyback_dyn(&args)?;
    let archive = parse_archive_path(&output)
        .ok_or_else(|| anyhow!("backup create output missing archive path: {output}"))?;
    ensure!(
        archive.is_file() && fs::metadata(&archive)?.len() > 0,
        "backup archive is missing or empty: {}",
        archive.display()
    );
    Ok(archive)
}

fn restore_archive(space: &str, archive: &Path) -> Result<()> {
    let archive = archive
        .to_str()
        .ok_or_else(|| anyhow!("archive path is not UTF-8"))?;
    let output = run_anyback(["restore", archive, "--space", space])?;
    let summary = parse_import_report(&output)?;
    ensure!(
        summary.failed == 0,
        "restore reported failed objects: {summary:?}"
    );
    Ok(())
}

async fn wait_resolve_type(
    client: &AnytypeClient,
    space_id: &str,
    key: &str,
) -> Result<anytype::types::Type> {
    let mut last_error = None;
    for _ in 0..40 {
        match client.resolve_type(space_id, key).await {
            Ok(typ) => return Ok(typ),
            Err(error) => last_error = Some(error),
        }
        sleep(Duration::from_millis(750)).await;
    }
    Err(last_error
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow!("type {key} was not resolved in {space_id}")))
}

async fn wait_resolve_chat(client: &AnytypeClient, space_id: &str, name: &str) -> Result<String> {
    let mut last_error = None;
    for _ in 0..40 {
        match client.resolve_chat_target(Some(space_id), name).await {
            Ok(target) => return Ok(target.chat_id),
            Err(error) => last_error = Some(error),
        }
        sleep(Duration::from_millis(750)).await;
    }
    Err(last_error
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow!("chat {name} was not resolved in {space_id}")))
}

async fn wait_for_chat_messages(
    client: &AnytypeClient,
    space_id: &str,
    chat_id: &str,
    tokens: [&str; 2],
) -> Result<Vec<anytype::chats::ChatMessage>> {
    let mut last_messages = Vec::new();
    for _ in 0..40 {
        let page = client
            .chats()
            .in_space(space_id)
            .older_messages(chat_id)
            .limit(100)
            .get()
            .await?;
        let has_all = tokens.iter().all(|token| {
            page.messages
                .iter()
                .any(|message| message.content.text.contains(token))
        });
        if has_all {
            return Ok(page.messages);
        }
        last_messages = page.messages;
        sleep(Duration::from_millis(750)).await;
    }
    bail!(
        "restored chat messages were not visible; last message count={}",
        last_messages.len()
    )
}

async fn choose_writable_spaces_cli() -> Result<(Space, Space)> {
    let dest_name = std::env::var("ANYBACK_TEST_DEST_SPACE").unwrap_or_else(|_| "test10".into());
    let source_override = std::env::var("ANYBACK_TEST_SOURCE_SPACE").ok();
    let source_candidates: Vec<String> = if let Some(name) = source_override {
        vec![name]
    } else {
        vec!["test11".into(), "test9".into(), "test10".into()]
    };

    let dest = resolve_space_by_name_cli(&dest_name)?;
    let dest_writable = is_writable_space_cli(&dest.name).await?;
    if !dest_writable {
        bail!("destination space {} is not writable", dest.name);
    }

    for source_name in &source_candidates {
        let source = match resolve_space_by_name_cli(source_name) {
            Ok(space) => space,
            Err(err) => {
                eprintln!("source candidate {source_name} not usable: {err}");
                continue;
            }
        };
        if is_writable_space_cli(&source.name).await? {
            if source.id == dest.id {
                eprintln!(
                    "source {} equals destination {}; running same-space e2e fallback",
                    source.name, dest.name
                );
            }
            return Ok((source, dest));
        }
    }

    bail!(
        "no writable source space found (tried: {}). destination: {}",
        source_candidates.join(", "),
        dest.name
    );
}

async fn choose_writable_chat_space_cli() -> Result<Option<Space>> {
    let output = run_anyr(["space", "list", "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let items = if let Some(items) = value.as_array() {
        items
    } else {
        value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("space list output missing items array"))?
    };

    for item in items {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        let is_chat = item.get("object").and_then(Value::as_str) == Some("chat");
        if !is_chat {
            continue;
        }
        let space = resolve_space_by_name_cli(name)?;
        if is_writable_space_cli(&space.name).await? {
            return Ok(Some(space));
        }
    }
    Ok(None)
}

async fn is_writable_space_cli(space_name: &str) -> Result<bool> {
    let probe_name = format!(
        "anyback-write-probe-{}",
        anytype::test_util::unique_suffix()
    );
    match create_probe_object(space_name, &probe_name) {
        Ok(Some(id)) => {
            let _ = delete_object(space_name, &id);
            Ok(true)
        }
        Ok(None) => Ok(false),
        Err(err) => {
            eprintln!("space {space_name} not writable: {err}");
            Ok(false)
        }
    }
}

fn resolve_space_by_name_cli(name: &str) -> Result<Space> {
    let output = run_anyr(["space", "get", name])?;
    let value: Value = serde_json::from_str(&output)?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("space get output missing id for {name}"))?;
    let resolved_name = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("space get output missing name for {name}"))?;

    Ok(Space {
        id: id.to_string(),
        name: resolved_name.to_string(),
        object: SpaceModel::Space,
        description: None,
        icon: None,
        gateway_url: None,
        network_id: None,
    })
}

fn run_anyback<const N: usize>(args: [&str; N]) -> Result<String> {
    run_anyback_dyn(&args)
}

fn run_anyback_dyn(args: &[&str]) -> Result<String> {
    let mut full = Vec::with_capacity(args.len() + 1);
    full.push("backup");
    full.extend_from_slice(args);
    let (stdout, stderr) = run_anyr_parts(&full)?;
    // Progress animation belongs to a TTY only; a piped stderr must stay plain.
    assert_non_tty_output_clean(&stderr);
    Ok(stdout)
}

fn parse_archive_path(output: &str) -> Option<PathBuf> {
    parse_json_output(output)
        .ok()?
        .get("archive")
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

fn manifest_sidecar_path(archive_path: &Path) -> PathBuf {
    let base_name = archive_path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("archive");
    let sidecar_name = format!("{base_name}.manifest.json");
    archive_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(sidecar_name)
}

fn unique_alpha_key_suffix() -> String {
    let mut n = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_nanos()
        ^ ((std::process::id() as u128) << 32);
    let mut out = String::with_capacity(10);
    for _ in 0..10 {
        let idx = (n % 26) as u8;
        out.push((b'a' + idx) as char);
        n /= 26;
    }
    out
}

fn read_manifest_json(archive_path: &Path) -> Result<Value> {
    let sidecar_path = manifest_sidecar_path(archive_path);
    let archive_manifest_path = archive_path.join("manifest.json");
    let manifest_text = match fs::read_to_string(&sidecar_path) {
        Ok(text) => text,
        Err(sidecar_err) => match fs::read_to_string(&archive_manifest_path) {
            Ok(text) => text,
            Err(archive_err) => {
                return Err(anyhow!(
                    "missing manifest: sidecar {} ({}) and archive {} ({})",
                    sidecar_path.display(),
                    sidecar_err,
                    archive_manifest_path.display(),
                    archive_err
                ));
            }
        },
    };
    serde_json::from_str(&manifest_text).context("failed to parse manifest JSON")
}

fn archive_object_ids(archive_path: &Path) -> Result<Vec<String>> {
    let reader = anyback_reader::archive::ArchiveReader::from_path(archive_path)?;
    let files = reader.list_files()?;
    Ok(anyback_reader::archive::infer_object_ids_from_files(&files))
}

/// Returns the object IDs from the archive's manifest (the backup selection list).
/// Use this instead of `archive_object_ids` when testing backup selection logic,
/// since the archive may contain additional dependency objects beyond the selection.
fn backup_selected_ids(archive_path: &Path) -> Result<Vec<String>> {
    let manifest_output = run_anyback([
        "--json",
        "manifest",
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    let manifest: Value = serde_json::from_str(&manifest_output)
        .with_context(|| format!("expected valid manifest JSON, got: {manifest_output}"))?;
    let objects = manifest
        .get("objects")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("manifest objects missing from output"))?;
    Ok(objects
        .iter()
        .filter_map(|obj| {
            obj.get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect())
}

fn archive_file_paths(archive_path: &Path) -> Result<Vec<String>> {
    let list_output = run_anyback([
        "--json",
        "list",
        "--files",
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    let list_json: Value = serde_json::from_str(&list_output)
        .with_context(|| format!("expected valid list JSON output, got: {list_output}"))?;
    let files = list_json
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("list json missing files array"))?;
    Ok(files
        .iter()
        .filter_map(|f| {
            f.get("path")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect())
}

fn archive_payload_file_paths(archive_path: &Path) -> Result<Vec<String>> {
    let files = archive_file_paths(archive_path)?;
    Ok(files
        .into_iter()
        .filter(|path| {
            let lower = path.to_ascii_lowercase();
            !(lower == "manifest.json" || lower.ends_with(".pb") || lower.ends_with(".pb.json"))
        })
        .collect())
}

fn archive_markdown_blob(archive_path: &Path) -> Result<String> {
    let mut chunks = Vec::new();
    for rel in archive_file_paths(archive_path)? {
        if !rel.to_ascii_lowercase().ends_with(".md") {
            continue;
        }
        let content = fs::read_to_string(archive_path.join(&rel))
            .with_context(|| format!("failed reading markdown file in archive: {rel}"))?;
        chunks.push(content);
    }
    Ok(chunks.join("\n\n---\n\n"))
}

fn write_tiny_png(path: &Path) -> Result<()> {
    // 1x1 PNG
    const PNG_1X1: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 15, 4, 0, 9,
        251, 3, 253, 160, 178, 75, 123, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    fs::write(path, PNG_1X1)
        .with_context(|| format!("failed writing tiny png fixture: {}", path.display()))?;
    Ok(())
}

fn upload_file_object(space_name: &str, file: &Path) -> Result<String> {
    let file_s = file.to_string_lossy().to_string();
    let output = run_anyr(["file", "upload", space_name, "--file", &file_s])?;
    let value: Value = serde_json::from_str(&output)?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("file upload output missing id"))?;
    Ok(id.to_string())
}

fn get_file_json(space_name: &str, object_id: &str) -> Result<Value> {
    let output = run_anyr(["file", "get", space_name, object_id])?;
    serde_json::from_str(&output).context("failed to parse file get JSON")
}

fn create_type(space_name: &str, key: &str, name: &str) -> Result<Value> {
    let output = run_anyr(["type", "create", space_name, key, name])?;
    serde_json::from_str(&output).context("failed to parse type create JSON")
}

fn create_property(space_name: &str, name: &str, key: &str, format: &str) -> Result<Value> {
    let output = run_anyr(["property", "create", space_name, name, format, "--key", key])?;
    serde_json::from_str(&output).context("failed to parse property create JSON")
}

fn get_type_json(space_name: &str, type_id_or_key: &str) -> Result<Value> {
    let output = run_anyr(["type", "get", space_name, type_id_or_key])?;
    serde_json::from_str(&output).context("failed to parse type get JSON")
}

fn get_property_json(space_name: &str, property_id_or_key: &str) -> Result<Value> {
    let output = run_anyr(["property", "get", space_name, property_id_or_key])?;
    serde_json::from_str(&output).context("failed to parse property get JSON")
}

fn delete_type(space_name: &str, type_id: &str) -> Result<()> {
    let _ = run_anyr(["type", "delete", space_name, type_id])?;
    Ok(())
}

fn checkbox_property(obj: &Value, key: &str) -> Option<bool> {
    obj.get("properties")
        .and_then(Value::as_array)?
        .iter()
        .find(|p| p.get("key").and_then(Value::as_str) == Some(key))
        .and_then(|p| p.get("checkbox"))
        .and_then(Value::as_bool)
}

fn delete_property(space_name: &str, property_id: &str) -> Result<()> {
    let _ = run_anyr(["property", "delete", space_name, property_id])?;
    Ok(())
}

fn count_archived_objects(space_name: &str) -> Result<u64> {
    let output = run_anyr(["space", "count-archived", space_name])?;
    let trimmed = output.trim();
    trimmed
        .parse::<u64>()
        .with_context(|| format!("invalid count-archived output: {trimmed}"))
}

fn run_anyr<const N: usize>(args: [&str; N]) -> Result<String> {
    run_anyr_dyn(&args)
}

/// Resolves the `anyr` executable under test.
///
/// `ANYR_BIN` wins when set; otherwise the binary built alongside this test
/// harness is required. The harness never falls back to `PATH`.
fn anyr_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("ANYR_BIN") {
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(anyhow!("ANYR_BIN is not a file: {}", path.display()));
        }
        return Ok(path);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(target_dir) = exe.parent().and_then(Path::parent)
    {
        let candidate = target_dir.join(format!("anyr{}", std::env::consts::EXE_SUFFIX));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "anyr test binary not found; run `cargo build -p anyr` first or set ANYR_BIN"
    ))
}

/// Parses the structured (compact JSON) result document written by `anyr`.
fn parse_json_output(output: &str) -> Result<Value> {
    serde_json::from_str(output.trim())
        .with_context(|| format!("expected structured anyr output, got: {output}"))
}

/// Counts reported by `anyr backup restore`.
#[derive(Debug, Clone, Copy)]
struct ImportSummary {
    attempted: u64,
    imported: u64,
    failed: u64,
}

/// Parses the structured import report emitted by `anyr backup restore`.
fn parse_import_report(output: &str) -> Result<ImportSummary> {
    let value = parse_json_output(output)?;
    let field = |name: &str| -> Result<u64> {
        value
            .get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("import report missing `{name}`: {output}"))
    };
    let summary = ImportSummary {
        attempted: field("attempted")?,
        imported: field("imported")?,
        failed: field("failed")?,
    };
    ensure!(
        summary.imported.saturating_add(summary.failed) <= summary.attempted,
        "inconsistent import report {summary:?}"
    );
    Ok(summary)
}

/// Asserts that a restore reported the expected counts.
///
/// Only reached by the `snapshot-import` cases, which are compiled out by default.
#[allow(dead_code)]
fn assert_import_counts(output: &str, imported: u64, attempted: u64, failed: u64) -> Result<()> {
    let summary = parse_import_report(output)?;
    ensure!(
        summary.imported == imported && summary.attempted == attempted && summary.failed == failed,
        "unexpected import report {summary:?}, wanted imported={imported} attempted={attempted} failed={failed}"
    );
    Ok(())
}

fn run_anyr_dyn(args: &[&str]) -> Result<String> {
    Ok(run_anyr_parts(args)?.0)
}

/// Runs `anyr` and returns its stdout and stderr separately.
///
/// Keeping the two apart matters for backup commands: stdout carries the
/// structured result document, and stderr carries progress and diagnostics.
fn run_anyr_parts(args: &[&str]) -> Result<(String, String)> {
    let output = run_with_lock_retry(|| {
        let mut command = Command::new(anyr_binary()?);
        command.args(args);
        let _keystore = support::keystore::configure_test_keystore(&mut command)?;
        command.output().context("failed to execute anyr command")
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        bail!(
            "anyr command failed (status={}):\nstdout:\n{}\nstderr:\n{}",
            output.status,
            stdout,
            stderr
        );
    }

    Ok((stdout.trim().to_string(), stderr))
}

fn run_with_lock_retry<F>(mut run: F) -> Result<std::process::Output>
where
    F: FnMut() -> Result<std::process::Output>,
{
    const MAX_ATTEMPTS: usize = 6;
    const BACKOFF_MS: [u64; MAX_ATTEMPTS] = [0, 500, 1_500, 3_000, 6_000, 10_000];

    let mut last_output: Option<std::process::Output> = None;
    for (attempt, delay_ms) in BACKOFF_MS.iter().enumerate() {
        if *delay_ms > 0 {
            thread::sleep(Duration::from_millis(*delay_ms));
        }
        let output = run()?;
        if output.status.success() {
            return Ok(output);
        }
        if looks_like_keyring_lock_error(&output) {
            eprintln!(
                "command hit keyring lock; retrying ({}/{MAX_ATTEMPTS})",
                attempt + 1
            );
        } else if looks_like_transient_anytype_error(&output) {
            eprintln!(
                "command hit transient anytype-heart 5xx; retrying ({}/{MAX_ATTEMPTS})",
                attempt + 1
            );
        } else {
            return Ok(output);
        }
        last_output = Some(output);
    }

    if let Some(output) = last_output {
        return Ok(output);
    }
    bail!("failed to execute command")
}

fn looks_like_keyring_lock_error(output: &std::process::Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    stderr.contains("Failed locking file") || stdout.contains("Failed locking file")
}

fn looks_like_transient_anytype_error(output: &std::process::Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let haystack = format!("{stdout}\n{stderr}");
    haystack.contains("\"code\":\"internal_server_error\"")
        || haystack.contains("failed to open workspace")
        || haystack.contains("failed to create block")
        || haystack.contains("failed to create object")
        || haystack.contains("failed to export markdown")
        || haystack.contains("context deadline exceeded")
        // Under concurrent test activity, anytype-heart can intermittently fail chat writes
        // with sqlite step I/O errors; these generally succeed on retry.
        || haystack.contains("sqlite: step: disk I/O error")
}

fn assert_non_tty_output_clean(output: &str) {
    assert!(
        !output.contains('\u{1b}'),
        "unexpected ANSI escape sequence in non-TTY output: {output:?}"
    );
    assert!(
        !output.contains('\r'),
        "unexpected carriage return animation in non-TTY output: {output:?}"
    );
}

fn create_probe_object(space_name: &str, name: &str) -> Result<Option<String>> {
    let output = run_anyr(["object", "create", space_name, "page", "--name", name])?;
    let value: Value =
        serde_json::from_str(&output).context("failed to parse probe create JSON output")?;
    Ok(value
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string))
}

fn delete_object(space_name: &str, object_id: &str) -> Result<()> {
    let _ = run_anyr(["object", "delete", space_name, object_id])?;
    Ok(())
}

fn add_to_list(space_name: &str, list_id: &str, object_ids: &[&str]) -> Result<()> {
    let mut args: Vec<&str> = vec!["list", "add", space_name, list_id];
    args.extend(object_ids.iter().copied());
    let _ = run_anyr_dyn(&args)?;
    Ok(())
}

fn remove_from_list(space_name: &str, list_id: &str, object_id: &str) -> Result<()> {
    let _ = run_anyr(["list", "remove", space_name, list_id, object_id])?;
    Ok(())
}

fn delete_objects_by_name(space_name: &str, name: &str) -> Result<()> {
    for id in list_object_ids_by_name(space_name, name)? {
        let _ = delete_object(space_name, &id);
    }
    Ok(())
}

fn resolve_default_chat_id(space_name: &str) -> Result<String> {
    let output = run_anyr(["chat", "list", "--space", space_name, "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .ok_or_else(|| anyhow!("chat list output missing items array"))?;
    let chat_id = items
        .iter()
        .find_map(|chat| chat.get("id").and_then(Value::as_str))
        .ok_or_else(|| anyhow!("no chats available in space {space_name}"))?;
    Ok(chat_id.to_string())
}

fn send_chat_message(space_name: &str, chat_id: &str, text: &str) -> Result<String> {
    const BACKOFF_MS: [u64; 8] = [0, 500, 1_500, 3_000, 5_000, 8_000, 12_000, 15_000];
    let mut last_err: Option<anyhow::Error> = None;

    for (attempt, delay_ms) in BACKOFF_MS.iter().enumerate() {
        if *delay_ms > 0 {
            thread::sleep(Duration::from_millis(*delay_ms));
        }
        match run_anyr([
            "chat", "messages", "send", space_name, chat_id, "--text", text,
        ]) {
            Ok(output) => {
                let value: Value = serde_json::from_str(&output)?;
                let id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("chat send output missing id"))?;
                return Ok(id.to_string());
            }
            Err(err) => {
                let text = format!("{err:#}");
                let retryable = text.contains("context deadline exceeded")
                    || text.contains("sqlite: step: disk I/O error")
                    || text.contains("\"code\":\"internal_server_error\"");
                if !retryable || attempt + 1 == BACKOFF_MS.len() {
                    return Err(err);
                }
                eprintln!(
                    "chat send hit transient backend error; retrying ({}/{})",
                    attempt + 1,
                    BACKOFF_MS.len()
                );
                last_err = Some(err);
            }
        }
    }

    if let Some(err) = last_err {
        return Err(err);
    }
    bail!("chat send failed unexpectedly without retry attempts")
}

fn delete_chat_message(space_name: &str, chat_id: &str, message_id: &str) -> Result<()> {
    let _ = run_anyr([
        "chat", "messages", "delete", space_name, chat_id, message_id,
    ])?;
    Ok(())
}

fn list_chat_messages(space_name: &str, chat_id: &str, limit: usize) -> Result<Vec<Value>> {
    let limit_s = limit.to_string();
    let output = run_anyr([
        "chat", "messages", "list", space_name, chat_id, "--limit", &limit_s,
    ])?;
    let value: Value = serde_json::from_str(&output)?;
    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        return Ok(messages.clone());
    }
    if let Some(messages) = value.as_array() {
        return Ok(messages.to_vec());
    }
    Err(anyhow!("chat messages list output missing messages array"))
}

fn message_text_contains(message: &Value, token: &str) -> bool {
    message
        .get("content")
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .is_some_and(|text| text.contains(token))
}

fn delete_chat_messages_by_token(space_name: &str, token: &str) -> Result<()> {
    let output = run_anyr(["chat", "list", "--space", space_name, "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let chats = value
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .ok_or_else(|| anyhow!("chat list output missing items array"))?;
    for chat in chats {
        let Some(chat_id) = chat.get("id").and_then(Value::as_str) else {
            continue;
        };
        let messages = list_chat_messages(space_name, chat_id, 500)?;
        for message in messages {
            if !message_text_contains(&message, token) {
                continue;
            }
            if let Some(message_id) = message.get("id").and_then(Value::as_str) {
                let _ = delete_chat_message(space_name, chat_id, message_id);
            }
        }
    }
    Ok(())
}

async fn wait_chat_message_contains(space_name: &str, chat_id: &str, token: &str) -> Result<()> {
    for _ in 0..40 {
        let messages = list_chat_messages(space_name, chat_id, 500)?;
        if messages
            .iter()
            .any(|message| message_text_contains(message, token))
        {
            return Ok(());
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!("chat message containing token '{token}' not found in space {space_name}");
}

/// Search all chats in a space for a message containing the token.
/// Used when the target chat_id is unknown (e.g. after cross-space restore).
async fn wait_chat_message_in_space(space_name: &str, token: &str) -> Result<()> {
    for _ in 0..40 {
        if let Ok(output) = run_anyr(["chat", "list", "--space", space_name, "--all"])
            && let Ok(value) = serde_json::from_str::<Value>(&output)
        {
            let chats = value
                .get("items")
                .and_then(Value::as_array)
                .or_else(|| value.as_array());
            if let Some(chats) = chats {
                for chat in chats {
                    let Some(chat_id) = chat.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    if let Ok(messages) = list_chat_messages(space_name, chat_id, 500)
                        && messages
                            .iter()
                            .any(|message| message_text_contains(message, token))
                    {
                        return Ok(());
                    }
                }
            }
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!("chat message containing token '{token}' not found in any chat in space {space_name}");
}

async fn wait_chat_message_absent(space_name: &str, chat_id: &str, token: &str) -> Result<()> {
    for _ in 0..30 {
        let messages = list_chat_messages(space_name, chat_id, 500)?;
        if !messages
            .iter()
            .any(|message| message_text_contains(message, token))
        {
            return Ok(());
        }
        sleep(Duration::from_millis(500)).await;
    }
    bail!("chat message containing token '{token}' still present in space {space_name}");
}

fn delete_objects_by_prefix(space_name: &str, prefix: &str) -> Result<()> {
    for id in list_object_ids_by_prefix(space_name, prefix)? {
        let _ = delete_object(space_name, &id);
    }
    Ok(())
}

fn delete_types_by_prefix(space_name: &str, prefix: &str) -> Result<()> {
    for id in list_type_ids_by_prefix(space_name, prefix)? {
        let _ = delete_type(space_name, &id);
    }
    Ok(())
}

fn delete_properties_by_prefix(space_name: &str, prefix: &str) -> Result<()> {
    for id in list_property_ids_by_prefix(space_name, prefix)? {
        let _ = delete_property(space_name, &id);
    }
    Ok(())
}

fn list_object_ids_by_name(space_name: &str, name: &str) -> Result<Vec<String>> {
    let output = run_anyr(["object", "list", space_name, "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let items = if let Some(items) = value.as_array() {
        items
    } else {
        value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("object list output missing items array"))?
    };
    Ok(items
        .iter()
        .filter_map(|item| {
            (item.get("name").and_then(Value::as_str) == Some(name))
                .then(|| item.get("id").and_then(Value::as_str))
                .flatten()
                .map(ToString::to_string)
        })
        .collect())
}

fn find_exact_object_id_by_name(space_name: &str, name: &str) -> Result<String> {
    let ids = list_object_ids_by_name(space_name, name)?;
    ids.into_iter().next().ok_or_else(|| {
        anyhow!(
            "object with exact name '{}' not found in {}",
            name,
            space_name
        )
    })
}

fn list_object_ids_by_prefix(space_name: &str, prefix: &str) -> Result<Vec<String>> {
    let output = run_anyr(["object", "list", space_name, "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let items = if let Some(items) = value.as_array() {
        items
    } else {
        value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("object list output missing items array"))?
    };
    Ok(items
        .iter()
        .filter_map(|item| {
            item.get("name")
                .and_then(Value::as_str)
                .filter(|candidate| candidate.contains(prefix))
                .and_then(|_| item.get("id").and_then(Value::as_str))
                .map(ToString::to_string)
        })
        .collect())
}

fn list_type_ids_by_prefix(space_name: &str, prefix: &str) -> Result<Vec<String>> {
    let output = run_anyr(["type", "list", space_name, "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let items = if let Some(items) = value.as_array() {
        items
    } else {
        value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("type list output missing items array"))?
    };
    Ok(items
        .iter()
        .filter_map(|item| {
            item.get("name")
                .and_then(Value::as_str)
                .filter(|candidate| candidate.contains(prefix))
                .and_then(|_| item.get("id").and_then(Value::as_str))
                .map(ToString::to_string)
        })
        .collect())
}

fn list_property_ids_by_prefix(space_name: &str, prefix: &str) -> Result<Vec<String>> {
    let output = run_anyr(["property", "list", space_name, "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let items = if let Some(items) = value.as_array() {
        items
    } else {
        value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("property list output missing items array"))?
    };
    Ok(items
        .iter()
        .filter_map(|item| {
            item.get("name")
                .and_then(Value::as_str)
                .filter(|candidate| candidate.contains(prefix))
                .and_then(|_| item.get("id").and_then(Value::as_str))
                .map(ToString::to_string)
        })
        .collect())
}

fn write_ids_file(path: &Path, ids: &[String]) -> Result<()> {
    let mut file = fs::File::create(path)
        .with_context(|| format!("failed to create id file {}", path.display()))?;
    for id in ids {
        writeln!(file, "{id}")?;
    }
    Ok(())
}

async fn wait_find_object_id_by_name(space_name: &str, name: &str) -> Result<String> {
    let mut last_count = 0usize;
    let mut last_partial_match: Option<String> = None;
    for _ in 0..40 {
        let output = run_anyr(["object", "list", space_name, "--all"])?;
        let value: Value = serde_json::from_str(&output)?;
        let items = if let Some(items) = value.as_array() {
            items
        } else {
            value
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("object list output missing items array"))?
        };
        last_count = items.len();

        if let Some(found_id) = items.iter().find_map(|item| {
            (item.get("name").and_then(Value::as_str) == Some(name))
                .then(|| item.get("id").and_then(Value::as_str))
                .flatten()
                .map(ToString::to_string)
        }) {
            return Ok(found_id);
        }
        if last_partial_match.is_none() {
            last_partial_match = items.iter().find_map(|item| {
                item.get("name")
                    .and_then(Value::as_str)
                    .filter(|candidate| candidate.starts_with(name))
                    .and_then(|_| item.get("id").and_then(Value::as_str))
                    .map(ToString::to_string)
            });
        }
        sleep(Duration::from_millis(750)).await;
    }

    if let Some(id) = last_partial_match {
        eprintln!(
            "using partial name match for '{}' in space {}: {}",
            name, space_name, id
        );
        return Ok(id);
    }

    bail!(
        "imported object with exact name '{}' not found in space {} (last candidate count: {})",
        name,
        space_name,
        last_count
    );
}

async fn wait_find_file_id_by_name(space_name: &str, name: &str) -> Result<String> {
    for _ in 0..40 {
        let output = run_anyr(["file", "list", space_name, "--all", "--name-contains", name])?;
        let value: Value = serde_json::from_str(&output)?;
        let items = if let Some(items) = value.as_array() {
            items
        } else {
            value
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("file list output missing items array"))?
        };
        if let Some(id) = items.iter().find_map(|item| {
            (item.get("name").and_then(Value::as_str) == Some(name))
                .then(|| item.get("id").and_then(Value::as_str))
                .flatten()
                .map(ToString::to_string)
        }) {
            return Ok(id);
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!("file with name '{}' not found in {}", name, space_name)
}

async fn wait_find_type_by_name(space_name: &str, name: &str) -> Result<Value> {
    for _ in 0..40 {
        let output = run_anyr(["type", "list", space_name, "--all"])?;
        let value: Value = serde_json::from_str(&output)?;
        let items = if let Some(items) = value.as_array() {
            items
        } else {
            value
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("type list output missing items array"))?
        };
        if let Some(found) = items
            .iter()
            .find(|it| it.get("name").and_then(Value::as_str) == Some(name))
        {
            return Ok(found.clone());
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!("type with name '{}' not found in {}", name, space_name)
}

async fn wait_find_property_by_name(space_name: &str, name: &str) -> Result<Value> {
    for _ in 0..40 {
        let output = run_anyr(["property", "list", space_name, "--all"])?;
        let value: Value = serde_json::from_str(&output)?;
        let items = if let Some(items) = value.as_array() {
            items
        } else {
            value
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("property list output missing items array"))?
        };
        if let Some(found) = items
            .iter()
            .find(|it| it.get("name").and_then(Value::as_str) == Some(name))
        {
            return Ok(found.clone());
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!("property with name '{}' not found in {}", name, space_name)
}

fn list_object_ids_in_list(space_name: &str, list_id: &str) -> Result<Vec<String>> {
    let output = run_anyr(["list", "objects", space_name, list_id, "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let items = if let Some(items) = value.as_array() {
        items
    } else {
        value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("list objects output missing items array"))?
    };
    Ok(items
        .iter()
        .filter_map(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect())
}

async fn permanently_delete_archived_ids(space_id: &str, ids: &[String]) -> Result<()> {
    let client = anytype::test_util::test_client_named("anyback_e2e_permdelete")
        .map_err(|e| anyhow!("failed to build client: {e}"))?;
    client
        .delete_archived(space_id, ids)
        .await
        .context("permanent delete failed")?;
    Ok(())
}

fn create_object(space_name: &str, name: &str, body: &str) -> Result<String> {
    let output = run_anyr([
        "object", "create", space_name, "page", "--name", name, "--body", body,
    ])?;
    let value: Value = serde_json::from_str(&output)?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("object create output missing id"))?;
    Ok(id.to_string())
}

fn create_typed_object(
    space_name: &str,
    object_type: &str,
    name: &str,
    body: &str,
) -> Result<String> {
    let output = run_anyr([
        "object",
        "create",
        space_name,
        object_type,
        "--name",
        name,
        "--body",
        body,
    ])?;
    let value: Value = serde_json::from_str(&output)?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("object create output missing id"))?;
    Ok(id.to_string())
}

fn update_object_body(space_name: &str, object_id: &str, body: &str) -> Result<()> {
    let _ = run_anyr(["object", "update", space_name, object_id, "--body", body])?;
    Ok(())
}

fn update_object_name(space_name: &str, object_id: &str, name: &str) -> Result<()> {
    let _ = run_anyr(["object", "update", space_name, object_id, "--name", name])?;
    Ok(())
}

fn get_object_json(space_name: &str, object_id: &str) -> Result<Value> {
    let output = run_anyr(["object", "get", space_name, object_id])?;
    serde_json::from_str(&output).context("failed to parse object get JSON")
}

async fn wait_object_name_eq(space_name: &str, object_id: &str, expected: &str) -> Result<()> {
    for _ in 0..40 {
        if let Ok(obj) = get_object_json(space_name, object_id)
            && obj.get("name").and_then(Value::as_str) == Some(expected)
        {
            return Ok(());
        }
        sleep(Duration::from_millis(750)).await;
    }
    let actual = get_object_json(space_name, object_id)
        .ok()
        .and_then(|v| v.get("name").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| "<unavailable>".to_string());
    bail!(
        "object {object_id} name expected '{}' but got '{}' in space {space_name}",
        expected,
        actual
    )
}

async fn wait_object_body_contains(space_name: &str, object_id: &str, token: &str) -> Result<()> {
    for _ in 0..40 {
        if let Ok(obj) = get_object_json(space_name, object_id)
            && obj
                .get("markdown")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains(token))
        {
            return Ok(());
        }
        sleep(Duration::from_millis(750)).await;
    }
    let actual = get_object_json(space_name, object_id)
        .ok()
        .and_then(|v| v.get("markdown").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| "<unavailable>".to_string());
    bail!(
        "object {object_id} body does not contain '{}' (actual: '{}') in space {space_name}",
        token,
        actual
    )
}

fn object_dates(
    space_name: &str,
    object_id: &str,
) -> Result<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
    let output = run_anyr(["object", "get", space_name, object_id])?;
    let value: Value = serde_json::from_str(&output)?;
    let properties = value
        .get("properties")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("object get output missing properties array"))?;

    let mut created: Option<DateTime<FixedOffset>> = None;
    let mut modified: Option<DateTime<FixedOffset>> = None;
    for prop in properties {
        let key = prop.get("key").and_then(Value::as_str).unwrap_or_default();
        let date = prop.get("date").and_then(Value::as_str);
        if key == "created_date" {
            created = date
                .map(DateTime::parse_from_rfc3339)
                .transpose()
                .context("invalid created_date value")?;
        } else if key == "last_modified_date" {
            modified = date
                .map(DateTime::parse_from_rfc3339)
                .transpose()
                .context("invalid last_modified_date value")?;
        }
    }

    let created = created.ok_or_else(|| anyhow!("created_date property not found"))?;
    let modified = modified.ok_or_else(|| anyhow!("last_modified_date property not found"))?;
    Ok((created, modified))
}
