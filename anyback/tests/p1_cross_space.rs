use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, FixedOffset};
use serde_json::Value;
use tokio::{sync::Mutex as AsyncMutex, time::sleep};

struct P1CleanupGuard {
    scopes: Vec<P1CleanupScope>,
}

struct P1CleanupScope {
    space: String,
    object_name_prefixes: Vec<String>,
    type_name_prefixes: Vec<String>,
    property_name_prefixes: Vec<String>,
    existing_protobuf_import_collections: HashSet<String>,
}

impl P1CleanupGuard {
    fn new(spaces: Vec<String>) -> Result<Self> {
        let mut unique_spaces = HashSet::new();
        let mut scopes = Vec::new();
        for space in spaces {
            if !unique_spaces.insert(space.clone()) {
                continue;
            }
            scopes.push(P1CleanupScope {
                space: space.clone(),
                object_name_prefixes: vec!["p1-cross-".to_string(), "p1-key-char-".to_string()],
                type_name_prefixes: vec![
                    "P1 Type ".to_string(),
                    "P1 CustomType ".to_string(),
                    "p1-key-char-type-".to_string(),
                ],
                property_name_prefixes: vec![
                    "P1 Prop ".to_string(),
                    "p1-key-char-prop-".to_string(),
                ],
                existing_protobuf_import_collections: list_protobuf_import_collection_ids(&space)?,
            });
        }
        Ok(Self { scopes })
    }
}

impl Drop for P1CleanupGuard {
    fn drop(&mut self) {
        for scope in &self.scopes {
            for prefix in &scope.object_name_prefixes {
                let _ = delete_objects_by_prefix(&scope.space, prefix);
            }
            for prefix in &scope.type_name_prefixes {
                let _ = delete_types_by_name_prefix(&scope.space, prefix);
            }
            for prefix in &scope.property_name_prefixes {
                let _ = delete_properties_by_name_prefix(&scope.space, prefix);
            }
            if let Ok(current) = list_protobuf_import_collection_ids(&scope.space) {
                for id in current.difference(&scope.existing_protobuf_import_collections) {
                    let _ = delete_object(&scope.space, id);
                }
            }
        }
    }
}

#[tokio::test]
async fn p1_restore_non_archived_object_between_spaces_preserves_fields() -> Result<()> {
    let _guard = test_lock().lock().await;
    let (source_space, dest_space) = choose_two_distinct_writable_spaces_cli().await?;
    let _cleanup = P1CleanupGuard::new(vec![source_space.clone(), dest_space.clone()])?;
    let token = uniq();
    let object_name = format!("p1-cross-object-{token}");
    let object_body = format!("p1 body {token}");

    let source_id = create_object(&source_space, &object_name, &object_body)?;
    let (source_created, source_modified) = object_dates(&source_space, &source_id)?;

    let archive_path = backup_selected(
        &source_space,
        std::slice::from_ref(&source_id),
        false,
        "p1-cross-obj",
    )?;
    let selected = backup_selected_ids(&archive_path)?;
    assert!(
        selected.contains(&source_id),
        "backup manifest missing selected object id"
    );

    restore_archive(&dest_space, &archive_path)?;

    let restored_id = wait_find_object_id_by_name(&dest_space, &object_name).await?;
    let restored = get_object_json(&dest_space, &restored_id)?;
    assert_eq!(restored["name"].as_str(), Some(object_name.as_str()));
    wait_object_body_contains_like(&dest_space, &restored_id, &object_body).await?;

    let (restored_created, restored_modified) = object_dates(&dest_space, &restored_id)?;
    assert_eq!(restored_created, source_created, "createdDate mismatch");
    assert_eq!(
        restored_modified, source_modified,
        "lastModifiedDate mismatch"
    );
    Ok(())
}

#[tokio::test]
async fn p1_restore_non_archived_image_between_spaces_preserves_fields() -> Result<()> {
    let _guard = test_lock().lock().await;
    let (source_space, dest_space) = choose_two_distinct_writable_spaces_cli().await?;
    let _cleanup = P1CleanupGuard::new(vec![source_space.clone(), dest_space.clone()])?;
    let token = uniq();
    let file_name = format!("p1-cross-image-{token}.png");
    let lookup_token = format!("p1-cross-image-{token}");

    let tmp = tempfile::tempdir()?;
    let image_path = tmp.path().join(&file_name);
    write_tiny_png(&image_path, &token)?;
    let source_id = upload_file_object(&source_space, &image_path, Some("image"))?;
    update_file_name(&source_space, &source_id, &file_name)?;
    let source = get_file_json(&source_space, &source_id)?;
    let source_file_search_ids =
        file_list_ids_by_token(&source_space, &lookup_token, Some("image"))?;
    assert!(
        source_file_search_ids.iter().any(|id| id == &source_id),
        "source file search did not find uploaded image id by token: source_id={} token={} ids={:?}",
        source_id,
        lookup_token,
        source_file_search_ids
    );
    let source_object_matches = list_object_names_containing(&source_space, &lookup_token)?;

    let archive_path = backup_selected(
        &source_space,
        std::slice::from_ref(&source_id),
        true,
        "p1-cross-image",
    )?;
    let selected = backup_selected_ids(&archive_path)?;
    assert!(
        selected.contains(&source_id),
        "backup manifest missing selected file id"
    );
    let source_manifest_entry = backup_manifest_object(&archive_path, &source_id)
        .ok()
        .flatten();
    let dest_before_ids = list_object_ids(&dest_space).unwrap_or_default();

    let restore_debug = restore_archive(&dest_space, &archive_path)?;

    let restored_id = match wait_find_file_id_by_token(&dest_space, &lookup_token, Some("image"))
        .await
    {
        Ok(id) => id,
        Err(first_err) => {
            sleep(Duration::from_secs(2)).await;
            if let Ok(ids_after_wait) =
                file_list_ids_by_token(&dest_space, &lookup_token, Some("image"))
            {
                if let Some(id_after_wait) = ids_after_wait.first() {
                    id_after_wait.clone()
                } else {
                    let dest_after_ids = list_object_ids(&dest_space).unwrap_or_default();
                    let new_ids: Vec<String> = dest_after_ids
                        .iter()
                        .filter(|id| !dest_before_ids.contains(*id))
                        .cloned()
                        .collect();
                    let new_obj_summaries = summarize_object_ids(&dest_space, &new_ids);
                    let file_search = run_anyr([
                        "file",
                        "list",
                        &dest_space,
                        "--all",
                        "--name-contains",
                        &lookup_token,
                        "--file-type",
                        "image",
                    ])
                    .unwrap_or_else(|e| format!("file search failed: {e:#}"));
                    let object_matches = list_object_names_containing(&dest_space, &lookup_token)
                        .unwrap_or_else(|e| vec![format!("object list failed: {e:#}")]);
                    let file_search_after_wait = run_anyr([
                        "file",
                        "list",
                        &dest_space,
                        "--all",
                        "--name-contains",
                        &lookup_token,
                        "--file-type",
                        "image",
                    ])
                    .unwrap_or_else(|e| format!("file search failed: {e:#}"));
                    let object_matches_after_wait =
                        list_object_names_containing(&dest_space, &lookup_token)
                            .unwrap_or_else(|e| vec![format!("object list failed: {e:#}")]);
                    let src_file_get = run_anyr_raw(&["file", "get", &dest_space, &source_id])?;
                    let src_obj_get = run_anyr_raw(&["object", "get", &dest_space, &source_id])?;
                    bail!(
                        "file token '{}' not found in space {} after retry: initial_err={}\nsource_file_id={}\nsource_file_search_ids={:?}\nsource_object_name_matches={:?}\nbackup_selected_ids={:?}\nsource_manifest_entry={}\nrestore_debug={}\nfile_search_initial={}\nobject_name_matches_initial={:?}\nfile_search_after_wait_2s={}\nobject_name_matches_after_wait_2s={:?}\nnew_object_ids_after_restore={:?}\nnew_object_summaries={:?}\nsource_id_file_get_status={:?}\nsource_id_file_get_stderr={}\nsource_id_object_get_status={:?}\nsource_id_object_get_stderr={}",
                        lookup_token,
                        dest_space,
                        first_err,
                        source_id,
                        source_file_search_ids,
                        source_object_matches,
                        selected,
                        source_manifest_entry
                            .as_ref()
                            .map(Value::to_string)
                            .unwrap_or_else(|| "null".to_string()),
                        restore_debug,
                        file_search,
                        object_matches,
                        file_search_after_wait,
                        object_matches_after_wait,
                        new_ids,
                        new_obj_summaries,
                        src_file_get.status.code(),
                        String::from_utf8_lossy(&src_file_get.stderr),
                        src_obj_get.status.code(),
                        String::from_utf8_lossy(&src_obj_get.stderr),
                    );
                }
            } else {
                let dest_after_ids = list_object_ids(&dest_space).unwrap_or_default();
                let new_ids: Vec<String> = dest_after_ids
                    .iter()
                    .filter(|id| !dest_before_ids.contains(*id))
                    .cloned()
                    .collect();
                let new_obj_summaries = summarize_object_ids(&dest_space, &new_ids);
                let file_search = run_anyr([
                    "file",
                    "list",
                    &dest_space,
                    "--all",
                    "--name-contains",
                    &lookup_token,
                    "--file-type",
                    "image",
                ])
                .unwrap_or_else(|e| format!("file search failed: {e:#}"));
                let object_matches = list_object_names_containing(&dest_space, &lookup_token)
                    .unwrap_or_else(|e| vec![format!("object list failed: {e:#}")]);
                let src_file_get = run_anyr_raw(&["file", "get", &dest_space, &source_id])?;
                let src_obj_get = run_anyr_raw(&["object", "get", &dest_space, &source_id])?;
                bail!(
                    "file token '{}' not found in space {} after retry; retry file search command failed: initial_err={}\nsource_file_id={}\nsource_file_search_ids={:?}\nsource_object_name_matches={:?}\nbackup_selected_ids={:?}\nsource_manifest_entry={}\nrestore_debug={}\nfile_search_initial={}\nobject_name_matches_initial={:?}\nnew_object_ids_after_restore={:?}\nnew_object_summaries={:?}\nsource_id_file_get_status={:?}\nsource_id_file_get_stderr={}\nsource_id_object_get_status={:?}\nsource_id_object_get_stderr={}",
                    lookup_token,
                    dest_space,
                    first_err,
                    source_id,
                    source_file_search_ids,
                    source_object_matches,
                    selected,
                    source_manifest_entry
                        .as_ref()
                        .map(Value::to_string)
                        .unwrap_or_else(|| "null".to_string()),
                    restore_debug,
                    file_search,
                    object_matches,
                    new_ids,
                    new_obj_summaries,
                    src_file_get.status.code(),
                    String::from_utf8_lossy(&src_file_get.stderr),
                    src_obj_get.status.code(),
                    String::from_utf8_lossy(&src_obj_get.stderr),
                );
            }
        }
    };
    let restored = get_file_json(&dest_space, &restored_id)?;

    assert_eq!(
        restored["name"].as_str(),
        source["name"].as_str(),
        "file name mismatch"
    );
    assert_eq!(
        file_detail_i64(&restored, "createdDate"),
        file_detail_i64(&source, "createdDate"),
        "createdDate mismatch"
    );
    // the difference must be <= 3 seconds
    let time_diff = i64::abs(
        file_detail_i64(&restored, "lastModifiedDate").unwrap()
            - file_detail_i64(&source, "lastModifiedDate").unwrap(),
    );
    assert!(
        time_diff <= 3,
        "lastModifiedDate drift {time_diff} is 3 sec or less"
    );
    assert_eq!(
        file_detail_i64(&restored, "widthInPixels"),
        file_detail_i64(&source, "widthInPixels"),
        "width metadata mismatch"
    );
    assert_eq!(
        file_detail_i64(&restored, "heightInPixels"),
        file_detail_i64(&source, "heightInPixels"),
        "height metadata mismatch"
    );
    Ok(())
}

#[tokio::test]
async fn p1_restore_non_archived_pdf_between_spaces_preserves_fields() -> Result<()> {
    let _guard = test_lock().lock().await;
    let (source_space, dest_space) = choose_two_distinct_writable_spaces_cli().await?;
    let _cleanup = P1CleanupGuard::new(vec![source_space.clone(), dest_space.clone()])?;
    let token = uniq();
    let file_name = format!("p1-cross-pdf-{token}.pdf");
    let lookup_token = format!("p1-cross-pdf-{token}");

    let tmp = tempfile::tempdir()?;
    let pdf_path = tmp.path().join(&file_name);
    write_tiny_pdf(&pdf_path, &token)?;
    let source_id = upload_file_object(&source_space, &pdf_path, Some("pdf"))?;
    update_file_name(&source_space, &source_id, &file_name)?;
    let source = get_file_json(&source_space, &source_id)?;
    let source_file_search_ids = file_list_ids_by_token(&source_space, &lookup_token, Some("pdf"))?;
    assert!(
        source_file_search_ids.iter().any(|id| id == &source_id),
        "source file search did not find uploaded pdf id by token: source_id={} token={} ids={:?}",
        source_id,
        lookup_token,
        source_file_search_ids
    );
    let source_object_matches = list_object_names_containing(&source_space, &lookup_token)?;

    let archive_path = backup_selected(
        &source_space,
        std::slice::from_ref(&source_id),
        true,
        "p1-cross-pdf",
    )?;
    let selected = backup_selected_ids(&archive_path)?;
    assert!(
        selected.contains(&source_id),
        "backup manifest missing selected file id"
    );
    let source_manifest_entry = backup_manifest_object(&archive_path, &source_id)
        .ok()
        .flatten();
    let dest_before_ids = list_object_ids(&dest_space).unwrap_or_default();

    let restore_debug = restore_archive(&dest_space, &archive_path)?;

    let restored_id = match wait_find_file_id_by_token(&dest_space, &lookup_token, Some("pdf"))
        .await
    {
        Ok(id) => id,
        Err(first_err) => {
            sleep(Duration::from_secs(2)).await;
            if let Ok(ids_after_wait) =
                file_list_ids_by_token(&dest_space, &lookup_token, Some("pdf"))
            {
                if let Some(id_after_wait) = ids_after_wait.first() {
                    id_after_wait.clone()
                } else {
                    let dest_after_ids = list_object_ids(&dest_space).unwrap_or_default();
                    let new_ids: Vec<String> = dest_after_ids
                        .iter()
                        .filter(|id| !dest_before_ids.contains(*id))
                        .cloned()
                        .collect();
                    let new_obj_summaries = summarize_object_ids(&dest_space, &new_ids);
                    let file_search = run_anyr([
                        "file",
                        "list",
                        &dest_space,
                        "--all",
                        "--name-contains",
                        &lookup_token,
                        "--file-type",
                        "pdf",
                    ])
                    .unwrap_or_else(|e| format!("file search failed: {e:#}"));
                    let object_matches = list_object_names_containing(&dest_space, &lookup_token)
                        .unwrap_or_else(|e| vec![format!("object list failed: {e:#}")]);
                    let file_search_after_wait = run_anyr([
                        "file",
                        "list",
                        &dest_space,
                        "--all",
                        "--name-contains",
                        &lookup_token,
                        "--file-type",
                        "pdf",
                    ])
                    .unwrap_or_else(|e| format!("file search failed: {e:#}"));
                    let object_matches_after_wait =
                        list_object_names_containing(&dest_space, &lookup_token)
                            .unwrap_or_else(|e| vec![format!("object list failed: {e:#}")]);
                    let src_file_get = run_anyr_raw(&["file", "get", &dest_space, &source_id])?;
                    let src_obj_get = run_anyr_raw(&["object", "get", &dest_space, &source_id])?;
                    bail!(
                        "file token '{}' not found in space {} after retry: initial_err={}\nsource_file_id={}\nsource_file_search_ids={:?}\nsource_object_name_matches={:?}\nbackup_selected_ids={:?}\nsource_manifest_entry={}\nrestore_debug={}\nfile_search_initial={}\nobject_name_matches_initial={:?}\nfile_search_after_wait_2s={}\nobject_name_matches_after_wait_2s={:?}\nnew_object_ids_after_restore={:?}\nnew_object_summaries={:?}\nsource_id_file_get_status={:?}\nsource_id_file_get_stderr={}\nsource_id_object_get_status={:?}\nsource_id_object_get_stderr={}",
                        lookup_token,
                        dest_space,
                        first_err,
                        source_id,
                        source_file_search_ids,
                        source_object_matches,
                        selected,
                        source_manifest_entry
                            .as_ref()
                            .map(Value::to_string)
                            .unwrap_or_else(|| "null".to_string()),
                        restore_debug,
                        file_search,
                        object_matches,
                        file_search_after_wait,
                        object_matches_after_wait,
                        new_ids,
                        new_obj_summaries,
                        src_file_get.status.code(),
                        String::from_utf8_lossy(&src_file_get.stderr),
                        src_obj_get.status.code(),
                        String::from_utf8_lossy(&src_obj_get.stderr),
                    );
                }
            } else {
                let dest_after_ids = list_object_ids(&dest_space).unwrap_or_default();
                let new_ids: Vec<String> = dest_after_ids
                    .iter()
                    .filter(|id| !dest_before_ids.contains(*id))
                    .cloned()
                    .collect();
                let new_obj_summaries = summarize_object_ids(&dest_space, &new_ids);
                let file_search = run_anyr([
                    "file",
                    "list",
                    &dest_space,
                    "--all",
                    "--name-contains",
                    &lookup_token,
                    "--file-type",
                    "pdf",
                ])
                .unwrap_or_else(|e| format!("file search failed: {e:#}"));
                let object_matches = list_object_names_containing(&dest_space, &lookup_token)
                    .unwrap_or_else(|e| vec![format!("object list failed: {e:#}")]);
                let src_file_get = run_anyr_raw(&["file", "get", &dest_space, &source_id])?;
                let src_obj_get = run_anyr_raw(&["object", "get", &dest_space, &source_id])?;
                bail!(
                    "file token '{}' not found in space {} after retry; retry file search command failed: initial_err={}\nsource_file_id={}\nsource_file_search_ids={:?}\nsource_object_name_matches={:?}\nbackup_selected_ids={:?}\nsource_manifest_entry={}\nrestore_debug={}\nfile_search_initial={}\nobject_name_matches_initial={:?}\nnew_object_ids_after_restore={:?}\nnew_object_summaries={:?}\nsource_id_file_get_status={:?}\nsource_id_file_get_stderr={}\nsource_id_object_get_status={:?}\nsource_id_object_get_stderr={}",
                    lookup_token,
                    dest_space,
                    first_err,
                    source_id,
                    source_file_search_ids,
                    source_object_matches,
                    selected,
                    source_manifest_entry
                        .as_ref()
                        .map(Value::to_string)
                        .unwrap_or_else(|| "null".to_string()),
                    restore_debug,
                    file_search,
                    object_matches,
                    new_ids,
                    new_obj_summaries,
                    src_file_get.status.code(),
                    String::from_utf8_lossy(&src_file_get.stderr),
                    src_obj_get.status.code(),
                    String::from_utf8_lossy(&src_obj_get.stderr),
                );
            }
        }
    };
    let restored = get_file_json(&dest_space, &restored_id)?;

    assert_eq!(
        restored["name"].as_str(),
        source["name"].as_str(),
        "pdf name mismatch"
    );
    assert_eq!(
        file_detail_i64(&restored, "createdDate"),
        file_detail_i64(&source, "createdDate"),
        "createdDate mismatch"
    );
    assert_eq!(
        file_detail_i64(&restored, "lastModifiedDate"),
        file_detail_i64(&source, "lastModifiedDate"),
        "lastModifiedDate mismatch"
    );
    assert_eq!(
        restored["mime"].as_str(),
        source["mime"].as_str(),
        "mime mismatch"
    );
    Ok(())
}

#[tokio::test]
async fn p1_restore_type_object_between_spaces_preserves_fields() -> Result<()> {
    let _guard = test_lock().lock().await;
    let (source_space, dest_space) = choose_two_distinct_writable_spaces_cli().await?;
    let _cleanup = P1CleanupGuard::new(vec![source_space.clone(), dest_space.clone()])?;
    let token = uniq();
    let type_key = format!("p_1_type_{token}");
    let type_name = format!("P1 Type {token}");

    let type_obj = create_type(&source_space, &type_key, &type_name)?;
    let source_id = type_obj
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("type create missing id"))?
        .to_string();

    let archive_path = backup_selected(
        &source_space,
        std::slice::from_ref(&source_id),
        false,
        "p1-cross-type",
    )?;
    let selected = backup_selected_ids(&archive_path)?;
    assert!(
        selected.contains(&source_id),
        "backup manifest missing selected type id"
    );

    restore_archive(&dest_space, &archive_path)?;

    let restored = match wait_get_type(&dest_space, &type_key).await {
        Ok(v) => v,
        Err(_) => {
            let by_name = wait_find_type_by_name(&dest_space, &type_name).await?;
            let actual_key = by_name
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("<missing>");
            bail!(
                "type restored with unexpected key: expected '{}' got '{}' (name='{}')",
                type_key,
                actual_key,
                type_name
            );
        }
    };
    assert_eq!(restored["key"].as_str(), Some(type_key.as_str()));
    assert_eq!(restored["name"].as_str(), Some(type_name.as_str()));
    assert_eq!(restored["layout"].as_str(), Some("basic"));
    Ok(())
}

#[tokio::test]
async fn p1_restore_property_object_between_spaces_preserves_fields() -> Result<()> {
    let _guard = test_lock().lock().await;
    let (source_space, dest_space) = choose_two_distinct_writable_spaces_cli().await?;
    let _cleanup = P1CleanupGuard::new(vec![source_space.clone(), dest_space.clone()])?;
    let token = uniq();
    let prop_key = format!("p_1_prop_{token}");
    let prop_name = format!("P1 Prop {token}");

    let created = create_property(&source_space, &prop_name, &prop_key, "text")?;
    let source_id = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("property create missing id"))?
        .to_string();

    let archive_path = backup_selected(
        &source_space,
        std::slice::from_ref(&source_id),
        false,
        "p1-cross-prop",
    )?;
    let selected = backup_selected_ids(&archive_path)?;
    assert!(
        selected.contains(&source_id),
        "backup manifest missing selected property id"
    );

    restore_archive(&dest_space, &archive_path)?;

    let restored = match wait_get_property(&dest_space, &prop_key).await {
        Ok(v) => v,
        Err(_) => {
            let by_name = wait_find_property_by_name(&dest_space, &prop_name).await?;
            let actual_key = by_name
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("<missing>");
            bail!(
                "property restored with unexpected key: expected '{}' got '{}' (name='{}')",
                prop_key,
                actual_key,
                prop_name
            );
        }
    };
    assert_eq!(restored["key"].as_str(), Some(prop_key.as_str()));
    assert_eq!(restored["name"].as_str(), Some(prop_name.as_str()));
    assert_eq!(restored["format"].as_str(), Some("text"));
    Ok(())
}

#[tokio::test]
async fn p1_characterize_type_property_key_normalization_patterns() -> Result<()> {
    // Root cause in anytype-heart:
    // core/api/util/key.go -> ToTypeApiKey / ToPropertyApiKey -> strcase.ToSnake(...)
    // This captures current key normalization behavior so tests can avoid unstable key patterns.
    let _guard = test_lock().lock().await;
    let (source_space, dest_space) = choose_two_distinct_writable_spaces_cli().await?;
    let _cleanup = P1CleanupGuard::new(vec![source_space.clone(), dest_space.clone()])?;
    let suffix = alpha_suffix();

    let cases: [(&str, &str); 10] = [
        ("alpha", "alpha"),
        ("p1_type", "p_1_type"),
        ("p_1_type", "p_1_type"),
        ("a1", "a_1"),
        ("a1b", "a_1_b"),
        ("ab1", "ab_1"),
        ("ab_1", "ab_1"),
        ("ab1_cd2", "ab_1_cd_2"),
        ("p12", "p_12"),
        ("p123_abc", "p_123_abc"),
    ];

    let mut type_ids = Vec::with_capacity(cases.len());
    let mut prop_ids = Vec::with_capacity(cases.len());
    let mut expected_type_keys = Vec::with_capacity(cases.len());
    let mut expected_prop_keys = Vec::with_capacity(cases.len());

    for (idx, (input_base, expected_base)) in cases.iter().enumerate() {
        let idx_ch = ((idx % 26) as u8 + b'a') as char;
        let case_suffix = format!("{suffix}{idx_ch}");
        let input_key = format!("{input_base}_{case_suffix}");
        let expected_key = format!("{expected_base}_{case_suffix}");

        let type_name = format!("p1-key-char-type-{idx}-{suffix}");
        let created_type = create_type(&source_space, &input_key, &type_name)?;
        let created_type_id = created_type
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("type create missing id"))?
            .to_string();
        let created_type_key = created_type
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        assert_eq!(
            created_type_key, expected_key,
            "source type key normalization mismatch for input '{}'",
            input_key
        );
        type_ids.push(created_type_id);
        expected_type_keys.push((type_name, input_key.clone(), expected_key.clone()));

        let prop_name = format!("p1-key-char-prop-{idx}-{suffix}");
        let created_prop = create_property(&source_space, &prop_name, &input_key, "text")?;
        let created_prop_id = created_prop
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("property create missing id"))?
            .to_string();
        let created_prop_key = created_prop
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        assert_eq!(
            created_prop_key, expected_key,
            "source property key normalization mismatch for input '{}'",
            input_key
        );
        prop_ids.push(created_prop_id);
        expected_prop_keys.push((prop_name, input_key, expected_key));
    }

    let mut selected_ids = Vec::with_capacity(type_ids.len() + prop_ids.len());
    selected_ids.extend(type_ids.iter().cloned());
    selected_ids.extend(prop_ids.iter().cloned());

    let archive_path = backup_selected(
        &source_space,
        &selected_ids,
        false,
        "p1-key-characterization",
    )?;
    let selected = backup_selected_ids(&archive_path)?;
    for id in &selected_ids {
        assert!(
            selected.contains(id),
            "backup manifest missing selected schema id '{}'",
            id
        );
    }

    restore_archive(&dest_space, &archive_path)?;

    for (name, input_key, expected_key) in expected_type_keys {
        let restored = wait_find_type_by_name(&dest_space, &name).await?;
        let actual = restored
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        assert_eq!(
            actual, expected_key,
            "restored type key mismatch for input '{}' (type name '{}')",
            input_key, name
        );
    }
    for (name, input_key, expected_key) in expected_prop_keys {
        let restored = wait_find_property_by_name(&dest_space, &name).await?;
        let actual = restored
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        assert_eq!(
            actual, expected_key,
            "restored property key mismatch for input '{}' (property name '{}')",
            input_key, name
        );
    }
    Ok(())
}

#[tokio::test]
async fn p1_restore_collection_and_items_between_spaces_preserves_fields() -> Result<()> {
    let _guard = test_lock().lock().await;
    let (source_space, dest_space) = choose_two_distinct_writable_spaces_cli().await?;
    let _cleanup = P1CleanupGuard::new(vec![source_space.clone(), dest_space.clone()])?;
    let token = uniq();
    let collection_name = format!("p1-cross-collection-{token}");
    let item_a_name = format!("p1-cross-item-a-{token}");
    let item_b_name = format!("p1-cross-item-b-{token}");

    let collection_id = create_typed_object(&source_space, "Collection", &collection_name, "")?;
    let item_a_id = create_object(&source_space, &item_a_name, &format!("body a {token}"))?;
    let item_b_id = create_object(&source_space, &item_b_name, &format!("body b {token}"))?;
    add_to_list(
        &source_space,
        &collection_id,
        &[item_a_id.as_str(), item_b_id.as_str()],
    )?;
    let source_links = object_links(&source_space, &collection_id)?;
    assert!(
        source_links.contains(&item_a_id) && source_links.contains(&item_b_id),
        "source collection missing expected item links"
    );

    let archive_path = backup_selected(
        &source_space,
        &[collection_id.clone(), item_a_id.clone(), item_b_id.clone()],
        false,
        "p1-cross-collection",
    )?;
    restore_archive(&dest_space, &archive_path)?;

    let restored_collection_id = wait_find_object_id_by_name(&dest_space, &collection_name).await?;
    let restored_item_a_id = wait_find_object_id_by_name(&dest_space, &item_a_name).await?;
    let restored_item_b_id = wait_find_object_id_by_name(&dest_space, &item_b_name).await?;
    let restored_links = object_links(&dest_space, &restored_collection_id)?;
    assert!(
        restored_links.contains(&restored_item_a_id)
            && restored_links.contains(&restored_item_b_id),
        "restored collection missing contained item links"
    );
    Ok(())
}

#[tokio::test]
async fn p1_restore_custom_type_object_between_spaces_preserves_fields() -> Result<()> {
    let _guard = test_lock().lock().await;
    let (source_space, dest_space) = choose_two_distinct_writable_spaces_cli().await?;
    let _cleanup = P1CleanupGuard::new(vec![source_space.clone(), dest_space.clone()])?;
    let token = uniq();
    let type_key = format!("pcustomabc_{}", uniq_num8());
    let type_name = format!("P1 CustomType {token}");
    let object_name = format!("p1-cross-custom-object-{token}");
    let object_body = format!("custom body {token}");

    let type_obj = create_type(&source_space, &type_key, &type_name)?;
    let type_id = type_obj
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("custom type create missing id"))?
        .to_string();
    let source_type_key = type_obj
        .get("key")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            get_type(&source_space, &type_id).ok().and_then(|v| {
                v.get("key")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
        })
        .ok_or_else(|| anyhow!("custom type create missing key"))?;
    let _ = wait_get_type(&source_space, &source_type_key).await?;
    let source_obj_id = create_object(&source_space, &object_name, &object_body)?;
    if let Err(err) = update_object_type(&source_space, &source_obj_id, &source_type_key) {
        eprintln!(
            "custom type update by key failed ({}), retrying by name '{}'",
            err, type_name
        );
        update_object_type(&source_space, &source_obj_id, &type_name)?;
    }

    let archive_path = backup_selected(
        &source_space,
        &[type_id.clone(), source_obj_id.clone()],
        false,
        "p1-cross-custom-type",
    )?;
    let restore_debug = restore_archive(&dest_space, &archive_path)?;

    let restored_obj_id = wait_find_object_id_by_name(&dest_space, &object_name).await?;
    wait_object_body_contains_like(&dest_space, &restored_obj_id, &object_body).await?;
    let restored_obj = wait_object_has_type_name_like(&dest_space, &restored_obj_id, &type_name)
        .await
        .unwrap_or_else(|_| get_object_json(&dest_space, &restored_obj_id).unwrap_or(Value::Null));
    let restored_type_by_name = wait_find_type_by_name(&dest_space, &type_name).await.ok();
    let restored_type = restored_obj
        .get("type")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            anyhow!(
                "restored object missing type relation (restore_debug={}, type_by_name={})",
                restore_debug,
                restored_type_by_name
                    .as_ref()
                    .map(Value::to_string)
                    .unwrap_or_else(|| "<none>".to_string())
            )
        })?;
    if restored_type.get("name").and_then(Value::as_str) == Some(type_name.as_str()) {
        return Ok(());
    }
    let restored_type_key = restored_type
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("restored object type missing key and name mismatch"))?;
    let restored_type_obj = get_type(&dest_space, restored_type_key)?;
    assert_eq!(
        restored_type_obj.get("name").and_then(Value::as_str),
        Some(type_name.as_str()),
        "restored object custom type does not match source custom type (restore_debug={})",
        restore_debug
    );
    Ok(())
}

fn test_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

fn uniq() -> String {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_micros();
    format!("{}{}", std::process::id(), micros)
}

fn uniq_num8() -> String {
    let v = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_micros()
        % 100_000_000;
    format!("{v:08}")
}

fn alpha_suffix() -> String {
    let mut n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_nanos()
        ^ ((std::process::id() as u128) << 32);
    let mut out = String::with_capacity(8);
    for _ in 0..8 {
        let idx = (n % 26) as u8;
        out.push((b'a' + idx) as char);
        n /= 26;
    }
    out
}

async fn choose_two_distinct_writable_spaces_cli() -> Result<(String, String)> {
    let candidates = ["test10", "test11"];
    let mut writable = Vec::new();
    for candidate in candidates {
        if is_writable_space_cli(candidate).await? {
            writable.push(candidate.to_string());
        }
    }
    if writable.len() < 2 {
        bail!("need two writable spaces among test10,test11");
    }
    Ok((writable[0].clone(), writable[1].clone()))
}

async fn is_writable_space_cli(space_name: &str) -> Result<bool> {
    let probe_name = format!("p1-write-probe-{}", uniq());
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

fn backup_selected(
    space: &str,
    ids: &[String],
    include_files: bool,
    prefix: &str,
) -> Result<PathBuf> {
    let base_dir = std::env::temp_dir().join(format!("anyback-p1-{prefix}-{}", uniq()));
    fs::create_dir_all(&base_dir)
        .with_context(|| format!("failed to create temp dir {}", base_dir.display()))?;

    let ids_file = base_dir.join("ids.txt");
    write_ids_file(&ids_file, ids)?;

    let mut args = vec![
        "backup".to_string(),
        "--space".to_string(),
        space.to_string(),
        "--objects".to_string(),
        ids_file.display().to_string(),
    ];
    if include_files {
        args.push("--include-files".to_string());
    }
    args.extend([
        "--prefix".to_string(),
        format!("{prefix}-{}", uniq()),
        "--dir".to_string(),
        base_dir.display().to_string(),
    ]);

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run_anyback_dyn(&arg_refs)?;
    parse_archive_path(&output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {output}"))
}

fn restore_archive(space: &str, archive: &Path) -> Result<String> {
    let log_path = std::env::temp_dir().join(format!("anyback-p1-restore-log-{}.json", uniq()));
    let archive_s = archive
        .to_str()
        .ok_or_else(|| anyhow!("bad archive path"))?;
    let log_s = log_path
        .to_str()
        .ok_or_else(|| anyhow!("bad restore log path"))?
        .to_string();
    let args = [
        "--json",
        "restore",
        "--space",
        space,
        "--log",
        log_s.as_str(),
        archive_s,
    ];
    let stdout = run_anyback_dyn(&args)?;
    let log_json = fs::read_to_string(&log_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    Ok(format!("cli={} log={}", stdout, log_json))
}

fn create_probe_object(space_name: &str, name: &str) -> Result<Option<String>> {
    let output = run_anyr(["object", "create", space_name, "page", "--name", name])?;
    let value: Value = serde_json::from_str(&output)?;
    Ok(value
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string))
}

fn create_object(space_name: &str, name: &str, body: &str) -> Result<String> {
    let output = run_anyr([
        "object", "create", space_name, "page", "--name", name, "--body", body,
    ])?;
    let value: Value = serde_json::from_str(&output)?;
    value
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("object create output missing id"))
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
    value
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("object create output missing id"))
}

fn create_type(space_name: &str, key: &str, name: &str) -> Result<Value> {
    let output = run_anyr(["type", "create", space_name, key, name])?;
    serde_json::from_str(&output).context("failed to parse type create JSON")
}

fn update_object_type(space_name: &str, object_id: &str, type_key: &str) -> Result<()> {
    let _ = run_anyr([
        "object", "update", space_name, object_id, "--type", type_key,
    ])?;
    Ok(())
}

fn get_type(space_name: &str, key_or_id: &str) -> Result<Value> {
    let output = run_anyr(["type", "get", space_name, key_or_id])?;
    serde_json::from_str(&output).context("failed to parse type get JSON")
}

fn create_property(space_name: &str, name: &str, key: &str, format: &str) -> Result<Value> {
    let output = run_anyr(["property", "create", space_name, name, format, "--key", key])?;
    serde_json::from_str(&output).context("failed to parse property create JSON")
}

fn get_property(space_name: &str, key_or_id: &str) -> Result<Value> {
    let output = run_anyr(["property", "get", space_name, key_or_id])?;
    serde_json::from_str(&output).context("failed to parse property get JSON")
}

fn upload_file_object(space_name: &str, file: &Path, file_type: Option<&str>) -> Result<String> {
    let file_s = file.to_string_lossy().to_string();
    let output = if let Some(ft) = file_type {
        run_anyr([
            "file",
            "upload",
            space_name,
            "--file",
            &file_s,
            "--file-type",
            ft,
        ])?
    } else {
        run_anyr(["file", "upload", space_name, "--file", &file_s])?
    };
    let value: Value = serde_json::from_str(&output)?;
    value
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("file upload output missing id"))
}

fn update_file_name(space_name: &str, object_id: &str, name: &str) -> Result<()> {
    let _ = run_anyr(["file", "update", space_name, object_id, "--name", name])?;
    Ok(())
}

fn get_file_json(space_name: &str, object_id: &str) -> Result<Value> {
    let output = run_anyr(["file", "get", space_name, object_id])?;
    serde_json::from_str(&output).context("failed to parse file get JSON")
}

fn file_detail_i64(file: &Value, key: &str) -> Option<i64> {
    file.get("details")
        .and_then(Value::as_object)
        .and_then(|m| m.get(key))
        .and_then(Value::as_f64)
        .map(|v| v as i64)
}

async fn wait_find_object_id_by_name(space_name: &str, name: &str) -> Result<String> {
    for _ in 0..40 {
        let output = run_anyr(["object", "list", space_name, "--all"])?;
        let value: Value = serde_json::from_str(&output)?;
        let items = if let Some(items) = value.as_array() {
            items
        } else {
            value
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("object list output missing items"))?
        };
        if let Some(found) = items.iter().find_map(|item| {
            (item.get("name").and_then(Value::as_str) == Some(name))
                .then(|| item.get("id").and_then(Value::as_str))
                .flatten()
                .map(ToString::to_string)
        }) {
            return Ok(found);
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!("object name '{}' not found in space {}", name, space_name)
}

async fn wait_find_file_id_by_token(
    space_name: &str,
    token: &str,
    file_type: Option<&str>,
) -> Result<String> {
    for _ in 0..40 {
        let ids = file_list_ids_by_token(space_name, token, file_type)?;
        if let Some(found) = ids.into_iter().next() {
            return Ok(found);
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!("file token '{}' not found in space {}", token, space_name)
}

fn file_list_ids_by_token(
    space_name: &str,
    token: &str,
    file_type: Option<&str>,
) -> Result<Vec<String>> {
    let output = if let Some(file_type) = file_type {
        run_anyr([
            "file",
            "list",
            space_name,
            "--all",
            "--name-contains",
            token,
            "--file-type",
            file_type,
        ])?
    } else {
        run_anyr([
            "file",
            "list",
            space_name,
            "--all",
            "--name-contains",
            token,
        ])?
    };
    let value: Value = serde_json::from_str(&output)?;
    let items = if let Some(items) = value.as_array() {
        items
    } else {
        value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("file list output missing items"))?
    };
    Ok(items
        .iter()
        .filter_map(|it| it.get("id").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect())
}

fn get_object_json(space_name: &str, object_id: &str) -> Result<Value> {
    let output = run_anyr(["object", "get", space_name, object_id])?;
    serde_json::from_str(&output).context("failed to parse object get JSON")
}

fn object_dates(
    space_name: &str,
    object_id: &str,
) -> Result<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
    let value = get_object_json(space_name, object_id)?;
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
            created = date.map(DateTime::parse_from_rfc3339).transpose()?;
        } else if key == "last_modified_date" {
            modified = date.map(DateTime::parse_from_rfc3339).transpose()?;
        }
    }
    Ok((
        created.ok_or_else(|| anyhow!("created_date not found"))?,
        modified.ok_or_else(|| anyhow!("last_modified_date not found"))?,
    ))
}

fn object_links(space_name: &str, object_id: &str) -> Result<Vec<String>> {
    let value = get_object_json(space_name, object_id)?;
    let props = value
        .get("properties")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("object get missing properties"))?;
    let links = props
        .iter()
        .find(|p| p.get("key").and_then(Value::as_str) == Some("links"))
        .and_then(|p| p.get("objects"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("object links relation missing"))?;
    Ok(links
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect())
}

fn add_to_list(space_name: &str, list_id: &str, object_ids: &[&str]) -> Result<()> {
    let mut args: Vec<&str> = vec!["list", "add", space_name, list_id];
    args.extend(object_ids.iter().copied());
    let _ = run_anyr_dyn(&args)?;
    Ok(())
}

fn write_tiny_png(path: &Path, salt: &str) -> Result<()> {
    const PNG_1X1: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 15, 4, 0, 9,
        251, 3, 253, 160, 178, 75, 123, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    // Keep PNG fully valid and make payload unique per run by inserting an
    // ancillary tEXt chunk before IEND (instead of appending trailing bytes).
    let mut bytes = PNG_1X1.to_vec();
    let iend_len = 12usize; // length(4) + type(4) + crc(4), with zero-length data
    if bytes.len() < iend_len {
        bail!("invalid tiny png fixture: missing IEND chunk");
    }
    let iend_start = bytes.len() - iend_len;
    if &bytes[iend_start + 4..iend_start + 8] != b"IEND" {
        bail!("invalid tiny png fixture: IEND chunk not found at expected location");
    }

    let mut text_data = Vec::with_capacity(8 + 1 + salt.len());
    text_data.extend_from_slice(b"anyback");
    text_data.push(0);
    text_data.extend_from_slice(salt.as_bytes());

    let mut chunk = Vec::with_capacity(12 + text_data.len());
    chunk.extend_from_slice(&(text_data.len() as u32).to_be_bytes());
    chunk.extend_from_slice(b"tEXt");
    chunk.extend_from_slice(&text_data);
    let crc = crc32_png(b"tEXt", &text_data);
    chunk.extend_from_slice(&crc.to_be_bytes());

    bytes.splice(iend_start..iend_start, chunk);
    fs::write(path, bytes)?;
    Ok(())
}

fn crc32_png(chunk_type: &[u8; 4], data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in chunk_type.iter().chain(data.iter()) {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg() & 0xEDB8_8320;
            crc = (crc >> 1) ^ mask;
        }
    }
    !crc
}

fn write_tiny_pdf(path: &Path, marker: &str) -> Result<()> {
    let pdf = format!(
        "%PDF-1.1\n1 0 obj<< /Type /Catalog /Pages 2 0 R >>endobj\n2 0 obj<< /Type /Pages /Kids [3 0 R] /Count 1 >>endobj\n3 0 obj<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>endobj\n% marker: {marker}\ntrailer<< /Root 1 0 R >>\n%%EOF\n"
    );
    fs::write(path, pdf.as_bytes())?;
    Ok(())
}

fn write_ids_file(path: &Path, ids: &[String]) -> Result<()> {
    let mut text = String::new();
    for id in ids {
        text.push_str(id);
        text.push('\n');
    }
    fs::write(path, text)?;
    Ok(())
}

fn parse_archive_path(output: &str) -> Option<PathBuf> {
    output
        .lines()
        .find_map(|line| line.strip_prefix("archive="))
        .and_then(|rest| rest.split_whitespace().next())
        .map(PathBuf::from)
}

fn backup_selected_ids(archive_path: &Path) -> Result<Vec<String>> {
    let manifest_output = run_anyback([
        "--json",
        "manifest",
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    let manifest: Value = serde_json::from_str(&manifest_output)?;
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

fn backup_manifest_object(archive_path: &Path, object_id: &str) -> Result<Option<Value>> {
    let manifest_output = run_anyback([
        "--json",
        "manifest",
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    let manifest: Value = serde_json::from_str(&manifest_output)?;
    let objects = manifest
        .get("objects")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("manifest objects missing from output"))?;
    Ok(objects
        .iter()
        .find(|obj| obj.get("id").and_then(Value::as_str) == Some(object_id))
        .cloned())
}

fn delete_object(space_name: &str, object_id: &str) -> Result<()> {
    let _ = run_anyr(["object", "delete", space_name, object_id])?;
    Ok(())
}

fn delete_type(space_name: &str, type_id: &str) -> Result<()> {
    let _ = run_anyr(["type", "delete", space_name, type_id])?;
    Ok(())
}

fn delete_property(space_name: &str, property_id: &str) -> Result<()> {
    let _ = run_anyr(["property", "delete", space_name, property_id])?;
    Ok(())
}

async fn wait_object_body_contains_like(
    space_name: &str,
    object_id: &str,
    token: &str,
) -> Result<()> {
    let escaped = token.replace('_', "\\_");
    for _ in 0..40 {
        if let Ok(obj) = get_object_json(space_name, object_id)
            && obj
                .get("markdown")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains(token) || m.contains(&escaped))
        {
            return Ok(());
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!(
        "object {object_id} body does not contain '{}' or escaped variant in space {space_name}",
        token
    )
}

async fn wait_object_has_type_name_like(
    space_name: &str,
    object_id: &str,
    type_name: &str,
) -> Result<Value> {
    for _ in 0..40 {
        if let Ok(obj) = get_object_json(space_name, object_id) {
            let name_matches = obj
                .get("type")
                .and_then(Value::as_object)
                .and_then(|t| t.get("name"))
                .and_then(Value::as_str)
                .is_some_and(|n| n == type_name);
            if name_matches {
                return Ok(obj);
            }
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!("object {object_id} type name did not become '{type_name}' in space {space_name}")
}

async fn wait_get_type(space_name: &str, key_or_id: &str) -> Result<Value> {
    for _ in 0..40 {
        if let Ok(v) = get_type(space_name, key_or_id) {
            return Ok(v);
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!("type {} not found in space {}", key_or_id, space_name)
}

async fn wait_get_property(space_name: &str, key_or_id: &str) -> Result<Value> {
    for _ in 0..40 {
        if let Ok(v) = get_property(space_name, key_or_id) {
            return Ok(v);
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!("property {} not found in space {}", key_or_id, space_name)
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
                .ok_or_else(|| anyhow!("type list output missing items"))?
        };
        if let Some(found) = items
            .iter()
            .find(|it| it.get("name").and_then(Value::as_str) == Some(name))
        {
            return Ok(found.clone());
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!(
        "type with name '{}' not found in space {}",
        name,
        space_name
    )
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
                .ok_or_else(|| anyhow!("property list output missing items"))?
        };
        if let Some(found) = items
            .iter()
            .find(|it| it.get("name").and_then(Value::as_str) == Some(name))
        {
            return Ok(found.clone());
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!(
        "property with name '{}' not found in space {}",
        name,
        space_name
    )
}

fn run_anyback<const N: usize>(args: [&str; N]) -> Result<String> {
    run_anyback_dyn(&args)
}

fn run_anyback_dyn(args: &[&str]) -> Result<String> {
    let output = run_with_lock_retry(|| {
        if let Ok(exe) = std::env::var("CARGO_BIN_EXE_anyback") {
            let mut command = Command::new(exe);
            command.args(args);
            configure_test_keystore(&mut command)?;
            command.output().context("failed to execute anyback binary")
        } else {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let workspace_root = manifest_dir
                .parent()
                .ok_or_else(|| anyhow!("failed to resolve workspace root"))?;
            let mut command = Command::new("cargo");
            command.current_dir(workspace_root);
            command.args(["run", "--quiet", "--bin", "anyback", "--"]);
            command.args(args);
            configure_test_keystore(&mut command)?;
            command
                .output()
                .context("failed to execute anyback via cargo run")
        }
    })?;
    if !output.status.success() {
        bail!(
            "anyback command failed (status={}):\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_anyr<const N: usize>(args: [&str; N]) -> Result<String> {
    run_anyr_dyn(&args)
}

fn run_anyr_dyn(args: &[&str]) -> Result<String> {
    let output = run_anyr_raw(args)?;
    if !output.status.success() {
        bail!(
            "anyr command failed (status={}):\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_anyr_raw(args: &[&str]) -> Result<std::process::Output> {
    run_with_lock_retry(|| {
        let mut command = Command::new("anyr");
        command.args(args);
        configure_test_keystore(&mut command)?;
        command.output().context("failed to execute anyr command")
    })
}

fn list_object_names_containing(space_name: &str, token: &str) -> Result<Vec<String>> {
    let output = run_anyr(["object", "list", space_name, "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let items = if let Some(items) = value.as_array() {
        items
    } else {
        value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("object list output missing items"))?
    };
    Ok(items
        .iter()
        .filter_map(|it| it.get("name").and_then(Value::as_str))
        .filter(|name| name.contains(token))
        .map(ToString::to_string)
        .collect())
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
            .ok_or_else(|| anyhow!("object list output missing items"))?
    };
    Ok(items
        .iter()
        .filter_map(|it| {
            let name = it.get("name").and_then(Value::as_str)?;
            if !name.starts_with(prefix) {
                return None;
            }
            it.get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect())
}

fn delete_objects_by_prefix(space_name: &str, prefix: &str) -> Result<()> {
    for id in list_object_ids_by_prefix(space_name, prefix)? {
        let _ = delete_object(space_name, &id);
    }
    Ok(())
}

fn list_object_ids(space_name: &str) -> Result<Vec<String>> {
    let output = run_anyr(["object", "list", space_name, "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let items = if let Some(items) = value.as_array() {
        items
    } else {
        value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("object list output missing items"))?
    };
    Ok(items
        .iter()
        .filter_map(|it| it.get("id").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect())
}

fn list_type_ids_by_name_prefix(space_name: &str, prefix: &str) -> Result<Vec<String>> {
    let output = run_anyr(["type", "list", space_name, "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let items = if let Some(items) = value.as_array() {
        items
    } else {
        value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("type list output missing items"))?
    };
    Ok(items
        .iter()
        .filter_map(|it| {
            let name = it.get("name").and_then(Value::as_str)?;
            if !name.starts_with(prefix) {
                return None;
            }
            it.get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect())
}

fn delete_types_by_name_prefix(space_name: &str, prefix: &str) -> Result<()> {
    for id in list_type_ids_by_name_prefix(space_name, prefix)? {
        let _ = delete_type(space_name, &id);
    }
    Ok(())
}

fn list_property_ids_by_name_prefix(space_name: &str, prefix: &str) -> Result<Vec<String>> {
    let output = run_anyr(["property", "list", space_name, "--all"])?;
    let value: Value = serde_json::from_str(&output)?;
    let items = if let Some(items) = value.as_array() {
        items
    } else {
        value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("property list output missing items"))?
    };
    Ok(items
        .iter()
        .filter_map(|it| {
            let name = it.get("name").and_then(Value::as_str)?;
            if !name.starts_with(prefix) {
                return None;
            }
            it.get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect())
}

fn delete_properties_by_name_prefix(space_name: &str, prefix: &str) -> Result<()> {
    for id in list_property_ids_by_name_prefix(space_name, prefix)? {
        let _ = delete_property(space_name, &id);
    }
    Ok(())
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
            .ok_or_else(|| anyhow!("object list output missing items"))?
    };
    Ok(items
        .iter()
        .filter_map(|it| {
            let is_collection = it.get("type").and_then(Value::as_str) == Some("collection");
            let name = it.get("name").and_then(Value::as_str)?;
            if !(is_collection && name.starts_with("Protobuf Import ")) {
                return None;
            }
            it.get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect())
}

fn summarize_object_ids(space_name: &str, ids: &[String]) -> Vec<String> {
    ids.iter()
        .take(10)
        .map(|id| match get_object_json(space_name, id) {
            Ok(v) => {
                let name = v.get("name").and_then(Value::as_str).unwrap_or("<none>");
                let layout = v.get("layout").and_then(Value::as_str).unwrap_or("<none>");
                let type_key = v
                    .get("type")
                    .and_then(Value::as_object)
                    .and_then(|t| t.get("key"))
                    .and_then(Value::as_str)
                    .unwrap_or("<none>");
                format!("{id}: name='{name}' layout='{layout}' type='{type_key}'")
            }
            Err(e) => format!("{id}: get failed: {e:#}"),
        })
        .collect()
}

fn run_with_lock_retry<F>(mut run: F) -> Result<std::process::Output>
where
    F: FnMut() -> Result<std::process::Output>,
{
    const BACKOFF_MS: [u64; 6] = [0, 500, 1_500, 3_000, 6_000, 10_000];
    let mut last_output: Option<std::process::Output> = None;

    for (attempt, delay_ms) in BACKOFF_MS.iter().enumerate() {
        if *delay_ms > 0 {
            thread::sleep(Duration::from_millis(*delay_ms));
        }
        let output = run()?;
        if output.status.success() {
            return Ok(output);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let haystack = format!("{stdout}\n{stderr}");
        if haystack.contains("Failed locking file")
            || haystack.contains("\"code\":\"internal_server_error\"")
            || haystack.contains("failed to create block")
            || haystack.contains("context deadline exceeded")
            || haystack.contains("sqlite: step: disk I/O error")
        {
            eprintln!(
                "command transient failure; retrying ({}/{})",
                attempt + 1,
                BACKOFF_MS.len()
            );
            last_output = Some(output);
            continue;
        }
        return Ok(output);
    }

    if let Some(output) = last_output {
        Ok(output)
    } else {
        bail!("failed to execute command")
    }
}

fn configure_test_keystore(command: &mut Command) -> Result<()> {
    if let Some(keystore) = cloned_test_keystore()? {
        command.env("ANYTYPE_KEYSTORE", keystore);
    }
    Ok(())
}

fn cloned_test_keystore() -> Result<Option<&'static str>> {
    static CLONED: OnceLock<Option<String>> = OnceLock::new();
    if let Some(value) = CLONED.get() {
        return Ok(value.as_deref());
    }

    let computed = if let Some(source) = std::env::var("ANYTYPE_KEYSTORE")
        .ok()
        .and_then(|value| value.strip_prefix("file:path=").map(ToString::to_string))
    {
        Some(format!(
            "file:path={}",
            clone_sqlite_with_sidecars(Path::new(&source))?.display()
        ))
    } else {
        None
    };

    let _ = CLONED.set(computed);
    Ok(CLONED.get().and_then(|v| v.as_deref()))
}

fn clone_sqlite_with_sidecars(source_db: &Path) -> Result<PathBuf> {
    if !source_db.exists() {
        bail!("source keystore does not exist: {}", source_db.display());
    }

    let target_db = std::env::temp_dir().join(format!("anyback-p1-keystore-{}.db", uniq()));
    fs::copy(source_db, &target_db)?;

    for suffix in ["-wal", "-shm"] {
        let source_sidecar = PathBuf::from(format!("{}{}", source_db.display(), suffix));
        if source_sidecar.exists() {
            let target_sidecar = PathBuf::from(format!("{}{}", target_db.display(), suffix));
            fs::copy(&source_sidecar, &target_sidecar)?;
        }
    }
    Ok(target_db)
}
