//! Restore matrix coverage tracker and executable test catalog.
//!
//! This file intentionally starts as a planning scaffold with a few
//! machine-checkable invariants. We will convert rows to executable e2e tests
//! incrementally in priority order.
//! Run with:
//!   `cargo test -p anyback --test restore_matrix -- --nocapture`

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, FixedOffset, Utc};
use serde_json::Value;
use tokio::time::sleep;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjKind {
    Object,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DestState {
    Missing,
    ExistsActive,
    ExistsArchived,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaseStatus {
    Works,
    WorksWithServerPatch,
    Fails,
    Untested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BugDisposition {
    PatchAnytypeHeart,
    KnownBug,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Priority {
    P0,
    P1,
    P2,
}

#[derive(Debug, Clone, Copy)]
struct CoreCase {
    id: &'static str,
    priority: Priority,
    kind: ObjKind,
    snapshot_archived: bool,
    dest: DestState,
    replace: bool,
    status: CaseStatus,
    disposition: BugDisposition,
    notes: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct ScenarioCase {
    id: &'static str,
    priority: Priority,
    status: CaseStatus,
    disposition: BugDisposition,
    notes: &'static str,
}

// Base matrix (24 meaningful states): 3 destination states x 2 snapshot archived
// x 2 object kinds x 2 replace values.
const CORE_CASES: &[CoreCase] = &[
    // Object rows
    CoreCase {
        id: "core-object-sa0-dest-missing-repl0",
        priority: Priority::P1,
        kind: ObjKind::Object,
        snapshot_archived: false,
        dest: DestState::Missing,
        replace: false,
        status: CaseStatus::Untested,
        disposition: BugDisposition::NotApplicable,
        notes: "Create path without replace.",
    },
    CoreCase {
        id: "core-object-sa0-dest-missing-repl1",
        priority: Priority::P0,
        kind: ObjKind::Object,
        snapshot_archived: false,
        dest: DestState::Missing,
        replace: true,
        status: CaseStatus::Fails,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Known issue from permanently-deleted restore path.",
    },
    CoreCase {
        id: "core-object-sa0-dest-active-repl0",
        priority: Priority::P1,
        kind: ObjKind::Object,
        snapshot_archived: false,
        dest: DestState::ExistsActive,
        replace: false,
        status: CaseStatus::Untested,
        disposition: BugDisposition::NotApplicable,
        notes: "Skip-existing path.",
    },
    CoreCase {
        id: "core-object-sa0-dest-active-repl1",
        priority: Priority::P0,
        kind: ObjKind::Object,
        snapshot_archived: false,
        dest: DestState::ExistsActive,
        replace: true,
        status: CaseStatus::WorksWithServerPatch,
        disposition: BugDisposition::NotApplicable,
        notes: "Validated on branch fix/restore-archive-existing.",
    },
    CoreCase {
        id: "core-object-sa0-dest-archived-repl0",
        priority: Priority::P1,
        kind: ObjKind::Object,
        snapshot_archived: false,
        dest: DestState::ExistsArchived,
        replace: false,
        status: CaseStatus::Untested,
        disposition: BugDisposition::NotApplicable,
        notes: "Skip-existing archived.",
    },
    CoreCase {
        id: "core-object-sa0-dest-archived-repl1",
        priority: Priority::P0,
        kind: ObjKind::Object,
        snapshot_archived: false,
        dest: DestState::ExistsArchived,
        replace: true,
        status: CaseStatus::WorksWithServerPatch,
        disposition: BugDisposition::NotApplicable,
        notes: "Unarchive-on-replace validated with server patch.",
    },
    CoreCase {
        id: "core-object-sa1-dest-missing-repl0",
        priority: Priority::P1,
        kind: ObjKind::Object,
        snapshot_archived: true,
        dest: DestState::Missing,
        replace: false,
        status: CaseStatus::Untested,
        disposition: BugDisposition::NotApplicable,
        notes: "Archive state from snapshot when creating.",
    },
    CoreCase {
        id: "core-object-sa1-dest-missing-repl1",
        priority: Priority::P1,
        kind: ObjKind::Object,
        snapshot_archived: true,
        dest: DestState::Missing,
        replace: true,
        status: CaseStatus::Untested,
        disposition: BugDisposition::NotApplicable,
        notes: "Replace flag should not matter on missing destination object.",
    },
    CoreCase {
        id: "core-object-sa1-dest-active-repl0",
        priority: Priority::P1,
        kind: ObjKind::Object,
        snapshot_archived: true,
        dest: DestState::ExistsActive,
        replace: false,
        status: CaseStatus::Untested,
        disposition: BugDisposition::NotApplicable,
        notes: "Skip-existing with archived snapshot source.",
    },
    CoreCase {
        id: "core-object-sa1-dest-active-repl1",
        priority: Priority::P1,
        kind: ObjKind::Object,
        snapshot_archived: true,
        dest: DestState::ExistsActive,
        replace: true,
        status: CaseStatus::Untested,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Check archive-on-replace semantics for existing active object.",
    },
    CoreCase {
        id: "core-object-sa1-dest-archived-repl0",
        priority: Priority::P1,
        kind: ObjKind::Object,
        snapshot_archived: true,
        dest: DestState::ExistsArchived,
        replace: false,
        status: CaseStatus::Untested,
        disposition: BugDisposition::NotApplicable,
        notes: "No-op expected.",
    },
    CoreCase {
        id: "core-object-sa1-dest-archived-repl1",
        priority: Priority::P1,
        kind: ObjKind::Object,
        snapshot_archived: true,
        dest: DestState::ExistsArchived,
        replace: true,
        status: CaseStatus::Untested,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Replace should retain archived true.",
    },
    // File rows
    CoreCase {
        id: "core-file-sa0-dest-missing-repl0",
        priority: Priority::P1,
        kind: ObjKind::File,
        snapshot_archived: false,
        dest: DestState::Missing,
        replace: false,
        status: CaseStatus::Untested,
        disposition: BugDisposition::NotApplicable,
        notes: "Create file object path.",
    },
    CoreCase {
        id: "core-file-sa0-dest-missing-repl1",
        priority: Priority::P1,
        kind: ObjKind::File,
        snapshot_archived: false,
        dest: DestState::Missing,
        replace: true,
        status: CaseStatus::Untested,
        disposition: BugDisposition::NotApplicable,
        notes: "Create path even with replace.",
    },
    CoreCase {
        id: "core-file-sa0-dest-active-repl0",
        priority: Priority::P1,
        kind: ObjKind::File,
        snapshot_archived: false,
        dest: DestState::ExistsActive,
        replace: false,
        status: CaseStatus::Untested,
        disposition: BugDisposition::NotApplicable,
        notes: "Skip-existing file object.",
    },
    CoreCase {
        id: "core-file-sa0-dest-active-repl1",
        priority: Priority::P0,
        kind: ObjKind::File,
        snapshot_archived: false,
        dest: DestState::ExistsActive,
        replace: true,
        status: CaseStatus::Fails,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Known issue: replace does not reset existing file object state/dates.",
    },
    CoreCase {
        id: "core-file-sa0-dest-archived-repl0",
        priority: Priority::P1,
        kind: ObjKind::File,
        snapshot_archived: false,
        dest: DestState::ExistsArchived,
        replace: false,
        status: CaseStatus::Untested,
        disposition: BugDisposition::NotApplicable,
        notes: "Skip-existing archived file object.",
    },
    CoreCase {
        id: "core-file-sa0-dest-archived-repl1",
        priority: Priority::P1,
        kind: ObjKind::File,
        snapshot_archived: false,
        dest: DestState::ExistsArchived,
        replace: true,
        status: CaseStatus::Untested,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Likely same path as active file + replace.",
    },
    CoreCase {
        id: "core-file-sa1-dest-missing-repl0",
        priority: Priority::P2,
        kind: ObjKind::File,
        snapshot_archived: true,
        dest: DestState::Missing,
        replace: false,
        status: CaseStatus::Untested,
        disposition: BugDisposition::KnownBug,
        notes: "Low-priority unless archived file restore is user-facing requirement.",
    },
    CoreCase {
        id: "core-file-sa1-dest-missing-repl1",
        priority: Priority::P2,
        kind: ObjKind::File,
        snapshot_archived: true,
        dest: DestState::Missing,
        replace: true,
        status: CaseStatus::Untested,
        disposition: BugDisposition::KnownBug,
        notes: "Same as non-replace create path.",
    },
    CoreCase {
        id: "core-file-sa1-dest-active-repl0",
        priority: Priority::P2,
        kind: ObjKind::File,
        snapshot_archived: true,
        dest: DestState::ExistsActive,
        replace: false,
        status: CaseStatus::Untested,
        disposition: BugDisposition::KnownBug,
        notes: "Skip-existing.",
    },
    CoreCase {
        id: "core-file-sa1-dest-active-repl1",
        priority: Priority::P1,
        kind: ObjKind::File,
        snapshot_archived: true,
        dest: DestState::ExistsActive,
        replace: true,
        status: CaseStatus::Untested,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Important if archived bit for files is expected to replay.",
    },
    CoreCase {
        id: "core-file-sa1-dest-archived-repl0",
        priority: Priority::P2,
        kind: ObjKind::File,
        snapshot_archived: true,
        dest: DestState::ExistsArchived,
        replace: false,
        status: CaseStatus::Untested,
        disposition: BugDisposition::KnownBug,
        notes: "Expected no-op.",
    },
    CoreCase {
        id: "core-file-sa1-dest-archived-repl1",
        priority: Priority::P1,
        kind: ObjKind::File,
        snapshot_archived: true,
        dest: DestState::ExistsArchived,
        replace: true,
        status: CaseStatus::Untested,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Exercise archived-file replace path.",
    },
];

// Orthogonal scenarios that do not fit cleanly into the 24-state base matrix.
const SCENARIO_CASES: &[ScenarioCase] = &[
    ScenarioCase {
        id: "scenario-full-backup-restore-simple-object",
        priority: Priority::P0,
        status: CaseStatus::Untested,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "MVP: full backup restore of a simple object (page/task, no linked objects).",
    },
    ScenarioCase {
        id: "scenario-incremental-since-restore-simple-object",
        priority: Priority::P0,
        status: CaseStatus::Untested,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "MVP: partial/incremental backup created with --since restores simple object correctly.",
    },
    ScenarioCase {
        id: "scenario-nested-links-forward",
        priority: Priority::P1,
        status: CaseStatus::Untested,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Parent->child nested references preserve IDs and date fields.",
    },
    ScenarioCase {
        id: "scenario-nested-links-backlinks",
        priority: Priority::P1,
        status: CaseStatus::Untested,
        disposition: BugDisposition::KnownBug,
        notes: "Backlink reconstruction may be eventually consistent.",
    },
    ScenarioCase {
        id: "scenario-type-object-rename",
        priority: Priority::P1,
        status: CaseStatus::Untested,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Type object rename before backup then restore.",
    },
    ScenarioCase {
        id: "scenario-relation-object-rename",
        priority: Priority::P1,
        status: CaseStatus::Untested,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Property/relation object rename before backup then restore.",
    },
    ScenarioCase {
        id: "scenario-object-type-changed-v1-to-v2",
        priority: Priority::P1,
        status: CaseStatus::Untested,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Object changes type between backups; restore full+incremental chain.",
    },
    ScenarioCase {
        id: "scenario-type-and-property-dependency-order",
        priority: Priority::P2,
        status: CaseStatus::Untested,
        disposition: BugDisposition::KnownBug,
        notes: "Deferred for MVP; mostly relevant to scoped restore (--objects) and schema-heavy restore.",
    },
    ScenarioCase {
        id: "scenario-restore-objects-flag-scoped-restore",
        priority: Priority::P2,
        status: CaseStatus::Untested,
        disposition: BugDisposition::KnownBug,
        notes: "Deferred for MVP; candidate to remove --objects from restore path for now.",
    },
    ScenarioCase {
        id: "scenario-file-date-preservation-on-recreate",
        priority: Priority::P0,
        status: CaseStatus::Untested,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Recreated file object should preserve createdDate and lastModifiedDate.",
    },
    ScenarioCase {
        id: "scenario-permanently-deleted-id-recovery",
        priority: Priority::P0,
        status: CaseStatus::Fails,
        disposition: BugDisposition::PatchAnytypeHeart,
        notes: "Restore should make object discoverable and readable after permanent delete.",
    },
];

#[test]
fn core_matrix_has_24_cases() {
    assert_eq!(CORE_CASES.len(), 24, "core matrix must keep 24 rows");
}

#[test]
fn all_case_ids_are_unique() {
    let mut seen = BTreeSet::new();
    for case in CORE_CASES {
        assert!(seen.insert(case.id), "duplicate core case id: {}", case.id);
    }
    for case in SCENARIO_CASES {
        assert!(
            seen.insert(case.id),
            "duplicate scenario case id: {}",
            case.id
        );
    }
}

#[test]
fn p0_cases_have_explicit_disposition() {
    for case in CORE_CASES {
        if case.priority == Priority::P0 {
            assert!(
                case.disposition != BugDisposition::NotApplicable
                    || case.status != CaseStatus::Fails,
                "P0 failing core case {} must decide patch-vs-known-bug",
                case.id
            );
        }
    }
    for case in SCENARIO_CASES {
        if case.priority == Priority::P0 {
            assert!(
                case.disposition != BugDisposition::NotApplicable
                    || case.status != CaseStatus::Fails,
                "P0 failing scenario {} must decide patch-vs-known-bug",
                case.id
            );
        }
    }
}

#[test]
fn print_priority_summary_for_operator() {
    let p0_core = CORE_CASES
        .iter()
        .filter(|c| c.priority == Priority::P0)
        .count();
    let p1_core = CORE_CASES
        .iter()
        .filter(|c| c.priority == Priority::P1)
        .count();
    let p2_core = CORE_CASES
        .iter()
        .filter(|c| c.priority == Priority::P2)
        .count();
    let p0_scen = SCENARIO_CASES
        .iter()
        .filter(|c| c.priority == Priority::P0)
        .count();
    let p1_scen = SCENARIO_CASES
        .iter()
        .filter(|c| c.priority == Priority::P1)
        .count();
    let p2_scen = SCENARIO_CASES
        .iter()
        .filter(|c| c.priority == Priority::P2)
        .count();
    eprintln!(
        "restore-matrix priorities: core(P0={}, P1={}, P2={}) scenarios(P0={}, P1={}, P2={})",
        p0_core, p1_core, p2_core, p0_scen, p1_scen, p2_scen
    );
}

#[test]
fn print_current_failures_for_triage() {
    for case in CORE_CASES.iter().filter(|c| c.status == CaseStatus::Fails) {
        eprintln!(
            "FAIL core={} priority={:?} disposition={:?} kind={:?} sa={} dest={:?} replace={} notes={}",
            case.id,
            case.priority,
            case.disposition,
            case.kind,
            case.snapshot_archived,
            case.dest,
            case.replace,
            case.notes
        );
    }
    for case in SCENARIO_CASES
        .iter()
        .filter(|c| c.status == CaseStatus::Fails)
    {
        eprintln!(
            "FAIL scenario={} priority={:?} disposition={:?} notes={}",
            case.id, case.priority, case.disposition, case.notes
        );
    }
}

#[tokio::test]
async fn e2e_matrix_p0_full_restore_simple_object() -> Result<()> {
    let space = choose_writable_space_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let original_name = format!("anyback-matrix-p0-full-{unique}");
    let original_body = format!("full restore body {unique}");
    let object_id = create_object(&space, &original_name, &original_body)?;

    let _ = delete_objects_by_prefix(&space, "anyback-matrix-p0-full-");

    let (_, backup_modified) = object_dates(&space, &object_id)?;
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let backup_output = run_anyback([
        "backup",
        "--space",
        space.as_str(),
        "--types",
        "page",
        "--prefix",
        &format!("anyback-matrix-p0-full-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let modified_name = format!("anyback-matrix-p0-full-modified-{unique}");
    let modified_body = format!("modified after backup {unique}");
    update_object_name_retry(&space, &object_id, &modified_name).await?;
    update_object_body_retry(&space, &object_id, &modified_body).await?;
    sleep(Duration::from_millis(500)).await;

    let _ = run_anyback([
        "restore",
        "--replace",
        "--space",
        space.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;

    wait_object_name_eq(&space, &object_id, &original_name).await?;
    wait_object_body_contains(&space, &object_id, &original_body).await?;
    let (_, restored_modified) = object_dates(&space, &object_id)?;
    assert_eq!(
        restored_modified, backup_modified,
        "last_modified_date should match backup value: backup={backup_modified} restored={restored_modified}"
    );
    Ok(())
}

#[tokio::test]
async fn e2e_matrix_p0_incremental_since_restore_simple_object() -> Result<()> {
    let space = choose_writable_space_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-matrix-p0-inc-{unique}");
    let object_id = create_object(&space, &object_name, &format!("inc-v1 {unique}"))?;

    let _ = delete_objects_by_prefix(&space, "anyback-matrix-p0-inc-");
    sleep(Duration::from_secs(2)).await;
    let since = Utc::now().to_rfc3339();
    sleep(Duration::from_secs(2)).await;

    let v2_name = format!("anyback-matrix-p0-inc-v2-{unique}");
    let v2_body = format!("inc-v2 {unique}");
    update_object_name_retry(&space, &object_id, &v2_name).await?;
    update_object_body_retry(&space, &object_id, &v2_body).await?;
    sleep(Duration::from_millis(600)).await;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let inc_output = run_anyback([
        "backup",
        "--space",
        space.as_str(),
        "--mode",
        "incremental",
        "--since",
        &since,
        "--types",
        "page",
        "--prefix",
        &format!("anyback-matrix-p0-inc-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let inc_archive = parse_archive_path(&inc_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {inc_output}"))?;
    let selected_ids = backup_selected_ids(&inc_archive)?;
    assert!(
        selected_ids.contains(&object_id),
        "incremental manifest missing expected object {object_id}; selected={selected_ids:?}"
    );

    let v3_name = format!("anyback-matrix-p0-inc-v3-{unique}");
    let v3_body = format!("inc-v3 {unique}");
    update_object_name_retry(&space, &object_id, &v3_name).await?;
    update_object_body_retry(&space, &object_id, &v3_body).await?;
    sleep(Duration::from_millis(600)).await;

    let _ = run_anyback([
        "restore",
        "--replace",
        "--space",
        space.as_str(),
        inc_archive
            .to_str()
            .ok_or_else(|| anyhow!("bad incremental archive path"))?,
    ])?;

    wait_object_name_eq(&space, &object_id, &v2_name).await?;
    wait_object_body_contains(&space, &object_id, &v2_body).await?;
    Ok(())
}

#[tokio::test]
async fn e2e_matrix_p0_recovers_permanently_deleted_object() -> Result<()> {
    let space = choose_writable_space_cli().await?;
    let space_id = resolve_space_id(&space)?;
    let unique = anytype::test_util::unique_suffix();
    let object_name = format!("anyback-matrix-p0-permdelete-{unique}");
    let object_body = "matrix permanent-delete body";
    let object_id = create_object(&space, &object_name, object_body)?;
    sleep(Duration::from_millis(500)).await;
    let (_, backup_modified) = object_dates(&space, &object_id)?;

    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let backup_output = run_anyback([
        "backup",
        "--space",
        space.as_str(),
        "--types",
        "page",
        "--prefix",
        &format!("anyback-matrix-p0-permdelete-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    delete_object(&space, &object_id)?;
    sleep(Duration::from_millis(600)).await;

    let client = anytype::test_util::test_client_named("anyback_restore_matrix_permdelete")
        .map_err(|e| anyhow!("failed to build client: {e}"))?;
    client
        .delete_archived(space_id.as_str(), std::slice::from_ref(&object_id))
        .await
        .context("permanent delete failed")?;
    sleep(Duration::from_millis(600)).await;

    assert!(
        get_object_json(&space, &object_id).is_err(),
        "permanently deleted object should not be retrievable before restore"
    );

    let _ = run_anyback([
        "restore",
        "--replace",
        "--space",
        space.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;

    wait_object_name_eq(&space, &object_id, &object_name).await?;
    wait_object_body_contains(&space, &object_id, object_body).await?;
    let obj = get_object_json(&space, &object_id)?;
    assert_ne!(
        obj["archived"].as_bool(),
        Some(true),
        "restored object should not be archived"
    );
    let (_, restored_modified) = object_dates(&space, &object_id)?;
    assert_eq!(
        restored_modified, backup_modified,
        "last_modified_date should match backup value: backup={backup_modified} restored={restored_modified}"
    );
    Ok(())
}

#[tokio::test]
async fn e2e_matrix_p0_file_recreate_preserves_dates() -> Result<()> {
    let (source_space, dest_space) = choose_two_distinct_writable_spaces_cli().await?;
    let unique = anytype::test_util::unique_suffix();
    let file_name = format!("anyback-matrix-p0-filedate-{unique}.png");
    let lookup_token = format!("anyback-matrix-p0-filedate-{unique}");

    let temp_upload = tempfile::tempdir().context("failed to create upload temp dir")?;
    let image_path = temp_upload.path().join(&file_name);
    write_tiny_png(&image_path)?;
    let source_file_id = upload_file_object(&source_space, &image_path)?;
    let (source_created, source_modified) = file_dates(&source_space, &source_file_id)?;

    sleep(Duration::from_secs(2)).await;

    let object_ids = vec![source_file_id.clone()];
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("ids.txt");
    write_ids_file(&ids_file, &object_ids)?;

    let backup_output = run_anyback([
        "backup",
        "--space",
        source_space.as_str(),
        "--objects",
        &ids_file.display().to_string(),
        "--include-files",
        "--prefix",
        &format!("anyback-matrix-p0-filedate-{unique}"),
        "--dir",
        &temp_dir.path().display().to_string(),
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;

    let _ = run_anyback([
        "restore",
        "--replace",
        "--space",
        dest_space.as_str(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;

    let restored_id = wait_find_file_id_by_token(&dest_space, &lookup_token).await?;
    let (restored_created, restored_modified) = file_dates(&dest_space, &restored_id)?;

    assert_eq!(
        restored_created, source_created,
        "createdDate mismatch for recreated file: source={} restored={}",
        source_created, restored_created
    );
    assert_eq!(
        restored_modified, source_modified,
        "lastModifiedDate mismatch for recreated file: source={} restored={}",
        source_modified, restored_modified
    );
    Ok(())
}

async fn choose_writable_space_cli() -> Result<String> {
    for candidate in ["test10", "test11", "test9"] {
        if is_writable_space_cli(candidate).await? {
            return Ok(candidate.to_string());
        }
    }
    bail!("no writable space found among test10,test11,test9")
}

async fn choose_two_distinct_writable_spaces_cli() -> Result<(String, String)> {
    let candidates = ["test10", "test11", "test9"];
    let mut writable = Vec::new();
    for candidate in candidates {
        if is_writable_space_cli(candidate).await? {
            writable.push(candidate.to_string());
        }
    }
    if writable.len() < 2 {
        bail!("need at least two writable spaces among test10,test11,test9");
    }
    Ok((writable[0].clone(), writable[1].clone()))
}

async fn is_writable_space_cli(space_name: &str) -> Result<bool> {
    let probe_name = format!(
        "anyback-restore-matrix-write-probe-{}",
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

fn run_anyback<const N: usize>(args: [&str; N]) -> Result<String> {
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
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "anyback command failed (status={}):\nstdout:\n{}\nstderr:\n{}",
            output.status,
            stdout,
            stderr
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_anyr<const N: usize>(args: [&str; N]) -> Result<String> {
    let output = run_with_lock_retry(|| {
        let mut command = Command::new("anyr");
        command.args(args);
        configure_test_keystore(&mut command)?;
        command.output().context("failed to execute anyr command")
    })?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "anyr command failed (status={}):\nstdout:\n{}\nstderr:\n{}",
            output.status,
            stdout,
            stderr
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn resolve_space_id(space_name: &str) -> Result<String> {
    let output = run_anyr(["space", "get", space_name])?;
    let value: Value = serde_json::from_str(&output)?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("space get output missing id for {space_name}"))?;
    Ok(id.to_string())
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
        if looks_like_keyring_lock_error(&output) || looks_like_transient_anytype_error(&output) {
            eprintln!(
                "command transient failure; retrying ({}/{MAX_ATTEMPTS})",
                attempt + 1
            );
            last_output = Some(output);
            continue;
        }
        return Ok(output);
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
        || haystack.contains("sqlite: step: disk I/O error")
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

    let mut target_db = std::env::temp_dir();
    target_db.push(format!(
        "anyback-matrix-keystore-{}-{}.db",
        std::process::id(),
        anytype::test_util::unique_suffix()
    ));
    fs::copy(source_db, &target_db).with_context(|| {
        format!(
            "failed to copy keystore {} to {}",
            source_db.display(),
            target_db.display()
        )
    })?;

    for suffix in ["-wal", "-shm"] {
        let source_sidecar = PathBuf::from(format!("{}{}", source_db.display(), suffix));
        if source_sidecar.exists() {
            let target_sidecar = PathBuf::from(format!("{}{}", target_db.display(), suffix));
            fs::copy(&source_sidecar, &target_sidecar).with_context(|| {
                format!(
                    "failed to copy sidecar {} to {}",
                    source_sidecar.display(),
                    target_sidecar.display()
                )
            })?;
        }
    }
    Ok(target_db)
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

async fn update_object_name_retry(space_name: &str, object_id: &str, name: &str) -> Result<()> {
    let mut last_err: Option<anyhow::Error> = None;
    for delay in [0_u64, 500, 1500, 3000, 6000, 10000] {
        if delay > 0 {
            sleep(Duration::from_millis(delay)).await;
        }
        match update_object_name(space_name, object_id, name) {
            Ok(()) => return Ok(()),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("update_object_name failed")))
}

async fn update_object_body_retry(space_name: &str, object_id: &str, body: &str) -> Result<()> {
    let mut last_err: Option<anyhow::Error> = None;
    for delay in [0_u64, 500, 1500, 3000, 6000, 10000] {
        if delay > 0 {
            sleep(Duration::from_millis(delay)).await;
        }
        match update_object_body(space_name, object_id, body) {
            Ok(()) => return Ok(()),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("update_object_body failed")))
}

fn delete_object(space_name: &str, object_id: &str) -> Result<()> {
    let _ = run_anyr(["object", "delete", space_name, object_id])?;
    Ok(())
}

fn delete_objects_by_prefix(space_name: &str, prefix: &str) -> Result<()> {
    for id in list_object_ids_by_prefix(space_name, prefix)? {
        let _ = delete_object(space_name, &id);
    }
    Ok(())
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

fn write_tiny_png(path: &Path) -> Result<()> {
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

fn file_dates(space_name: &str, object_id: &str) -> Result<(i64, i64)> {
    let output = run_anyr(["file", "get", space_name, object_id])?;
    let value: Value = serde_json::from_str(&output)?;
    let details = value
        .get("details")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("file get output missing details"))?;

    let created = details
        .get("createdDate")
        .and_then(Value::as_f64)
        .map(|v| v as i64)
        .ok_or_else(|| anyhow!("file createdDate missing"))?;
    let modified = details
        .get("lastModifiedDate")
        .and_then(Value::as_f64)
        .map(|v| v as i64)
        .ok_or_else(|| anyhow!("file lastModifiedDate missing"))?;
    Ok((created, modified))
}

fn write_ids_file(path: &Path, ids: &[String]) -> Result<()> {
    let mut text = String::new();
    for id in ids {
        text.push_str(id);
        text.push('\n');
    }
    fs::write(path, text)
        .with_context(|| format!("failed to create id file {}", path.display()))?;
    Ok(())
}

async fn wait_find_file_id_by_token(space_name: &str, token: &str) -> Result<String> {
    for _ in 0..40 {
        let output = run_anyr(["file", "search", space_name, "--name-contains", token])?;
        let value: Value = serde_json::from_str(&output)?;
        let items = value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("file search output missing items"))?;
        if let Some(id) = items.iter().find_map(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        }) {
            return Ok(id);
        }
        sleep(Duration::from_millis(750)).await;
    }
    bail!(
        "file name containing token '{}' not found in space {}",
        token,
        space_name
    );
}
