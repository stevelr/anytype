use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use anytype::prelude::*;
use rand::{Rng, SeedableRng, rngs::StdRng};
use serde::Serialize;
use serde_json::Value;
use tokio::time::sleep;

struct PrefixCleanupGuard {
    spaces: Vec<String>,
    prefixes: Vec<String>,
}

impl PrefixCleanupGuard {
    fn new(spaces: Vec<String>, prefixes: Vec<String>) -> Self {
        Self { spaces, prefixes }
    }
}

impl Drop for PrefixCleanupGuard {
    fn drop(&mut self) {
        for space in &self.spaces {
            for prefix in &self.prefixes {
                let _ = delete_objects_by_prefix_sync(space, prefix);
            }
        }
    }
}

#[tokio::test]
#[ignore = "nightly integrity fuzz against live Anytype backend"]
async fn nightly_integrity_fuzz_roundtrip() -> Result<()> {
    let (profile, cfg) = IntegrityConfig::from_env()?;
    eprintln!("integrity config: (profile={profile}) {:#?}", cfg);
    let run_chat_roundtrip_checks = profile == "large";

    let client = anytype::test_util::test_client_named("anyback_integrity")
        .map_err(|e| anyhow!("failed to build test client: {e}"))?;
    let (source_space, dest_space) = choose_writable_spaces(&client).await?;
    let chat_space = if run_chat_roundtrip_checks {
        let selected = choose_writable_chat_space(&client).await?;
        ensure!(
            selected.is_some(),
            "nightly-large requires at least one writable chat space"
        );
        selected
    } else {
        None
    };
    let prefix = format!("anyback-integrity-{}", anytype::test_util::unique_suffix());
    let _cleanup = PrefixCleanupGuard::new(
        vec![source_space.name.clone(), dest_space.name.clone()],
        vec![prefix.clone()],
    );
    let mut rng = StdRng::seed_from_u64(cfg.seed);
    let start = Instant::now();
    let mut created_source_ids = Vec::new();
    let mut total_created = 0usize;
    let mut total_body_bytes = 0usize;
    let mut total_uploaded_files = 0usize;

    for iteration in 0..cfg.iterations {
        if start.elapsed().as_secs() >= cfg.max_seconds {
            break;
        }
        if total_created >= cfg.max_total_objects {
            break;
        }
        if total_body_bytes >= cfg.max_total_body_bytes {
            break;
        }

        let budget_objects = cfg.max_total_objects - total_created;
        let batch_size = rng
            .random_range(1..=cfg.max_objects_per_iteration.max(1))
            .min(budget_objects);
        let mut batch = Vec::with_capacity(batch_size);

        for i in 0..batch_size {
            let type_key = random_type(&mut rng, &cfg);
            let name = format!("{prefix}-it{iteration}-obj{i}-{}", rng.random::<u32>());
            let semantic_token =
                format!("anyback-semantic-{iteration}-{i}-{}", rng.random::<u32>());
            let description = format!("desc-{semantic_token}");
            let body = random_body(
                &mut rng,
                cfg.max_body_bytes,
                &prefix,
                &batch,
                &source_space.id,
                &semantic_token,
            );
            total_body_bytes += body.len();

            let object = client
                .new_object(&source_space.id, type_key)
                .name(&name)
                .body(&body)
                .description(&description)
                .create()
                .await
                .with_context(|| format!("failed creating {type_key} object {name}"))?;

            created_source_ids.push(object.id.clone());
            batch.push(GeneratedCase {
                id: object.id,
                requested_name: name,
                observed_name: object.name.filter(|v| !v.trim().is_empty()),
                type_key: type_key.to_string(),
                body_len: body.len(),
                expected_description: Some(description),
                expected_markdown_token: Some(semantic_token),
            });
        }

        if batch.len() < budget_objects && iteration % 2 == 0 {
            let attachment_cases = create_attachment_cases(
                &client,
                &source_space.id,
                &prefix,
                iteration,
                &mut rng,
                budget_objects - batch.len(),
                cfg.max_body_bytes,
            )
            .await?;
            total_body_bytes += attachment_cases.body_bytes;
            total_uploaded_files += attachment_cases.uploaded_files;
            created_source_ids.extend(attachment_cases.created_ids);
            batch.extend(attachment_cases.cases);
        }

        total_created += batch.len();
        let profile_flags = export_arg_profile(iteration);
        let batch_artifacts = run_backup_restore_batch(
            &cfg,
            &source_space.name,
            &dest_space.name,
            &prefix,
            iteration,
            &batch,
            profile_flags,
        )?;
        if let Err(err) =
            wait_validate_batch_semantics(&client, &dest_space.id, &batch, Duration::from_secs(25))
                .await
        {
            let persisted = persist_failure_artifacts(
                &batch_artifacts,
                &batch,
                &prefix,
                iteration,
                "validate-timeout",
            );
            if let Ok(path) = persisted.as_ref() {
                eprintln!("integrity artifacts saved: {}", path.display());
            }
            return Err(err).with_context(|| match persisted {
                Ok(path) => format!(
                    "integrity validation failed; diagnostics saved at {}",
                    path.display()
                ),
                Err(save_err) => {
                    format!("integrity validation failed; additionally failed saving diagnostics: {save_err:#}")
                }
            });
        }
        if iteration % 3 == 0 {
            run_markdown_export_probe(
                &source_space.name,
                &prefix,
                iteration,
                &batch,
                iteration % 2 == 0,
            )?;
        }
        drop(batch_artifacts);
    }

    if run_chat_roundtrip_checks {
        run_regular_space_chat_roundtrip_check(&client, &source_space, &dest_space, &cfg, &prefix)
            .await?;
        if let Some(chat_space) = chat_space.as_ref() {
            run_chat_space_roundtrip_check(&client, chat_space, &cfg, &prefix).await?;
        }
    }

    ensure!(total_created > 0, "integrity test created no objects");
    eprintln!(
        "integrity summary: created={} total_body_bytes={} uploaded_files={} elapsed={}s",
        total_created,
        total_body_bytes,
        total_uploaded_files,
        start.elapsed().as_secs()
    );

    cleanup_source_ids(&client, &source_space.id, &created_source_ids).await?;
    cleanup_by_prefix(&client, &dest_space.id, &prefix).await?;
    Ok(())
}

#[test]
fn integrity_config_profiles_parse() -> Result<()> {
    let tiny = IntegrityConfig::profile("tiny", 1)?;
    let small = IntegrityConfig::profile("small", 1)?;
    let medium = IntegrityConfig::profile("medium", 1)?;
    let large = IntegrityConfig::profile("large", 1)?;
    assert!(tiny.max_objects_per_iteration <= 3);
    assert!(small.max_objects_per_iteration > tiny.max_objects_per_iteration);
    assert!(medium.max_objects_per_iteration > small.max_objects_per_iteration);
    assert!(large.max_objects_per_iteration > medium.max_objects_per_iteration);
    assert!(small.max_total_objects > tiny.max_total_objects);
    assert!(medium.max_total_objects > small.max_total_objects);
    assert!(large.max_total_objects > medium.max_total_objects);
    Ok(())
}

#[test]
fn export_arg_profile_matrix_rotates_expected_flag_sets() {
    let p0 = export_arg_profile(0);
    let p1 = export_arg_profile(1);
    let p2 = export_arg_profile(2);
    let p3 = export_arg_profile(3);
    assert!(
        !p0.include_files && !p0.include_archived && !p0.include_nested && !p0.include_backlinks
    );
    assert!(
        p1.include_files && !p1.include_archived && !p1.include_nested && !p1.include_backlinks
    );
    assert!(
        !p2.include_files && p2.include_archived && !p2.include_nested && !p2.include_backlinks
    );
    assert!(!p3.include_files && !p3.include_archived && p3.include_nested && p3.include_backlinks);
}

#[derive(Debug, Clone, Serialize)]
struct GeneratedCase {
    id: String,
    requested_name: String,
    observed_name: Option<String>,
    type_key: String,
    body_len: usize,
    expected_description: Option<String>,
    expected_markdown_token: Option<String>,
}

struct AttachmentCaseBatch {
    created_ids: Vec<String>,
    cases: Vec<GeneratedCase>,
    body_bytes: usize,
    uploaded_files: usize,
}

struct BatchArtifacts {
    archive_path: PathBuf,
    ids_file: PathBuf,
    report_path: PathBuf,
    backup_output: String,
    restore_output: String,
    _temp_dir: tempfile::TempDir,
}

#[derive(Debug, Clone, Copy)]
struct ExportArgProfile {
    include_files: bool,
    include_archived: bool,
    include_nested: bool,
    include_backlinks: bool,
}

fn export_arg_profile(iteration: usize) -> ExportArgProfile {
    match iteration % 4 {
        0 => ExportArgProfile {
            include_files: false,
            include_archived: false,
            include_nested: false,
            include_backlinks: false,
        },
        1 => ExportArgProfile {
            include_files: true,
            include_archived: false,
            include_nested: false,
            include_backlinks: false,
        },
        2 => ExportArgProfile {
            include_files: false,
            include_archived: true,
            include_nested: false,
            include_backlinks: false,
        },
        _ => ExportArgProfile {
            include_files: false,
            include_archived: false,
            include_nested: true,
            include_backlinks: true,
        },
    }
}

#[derive(Debug, Clone, Serialize)]
struct IntegrityConfig {
    iterations: usize,
    max_objects_per_iteration: usize,
    max_body_bytes: usize,
    max_seconds: u64,
    max_total_objects: usize,
    max_total_body_bytes: usize,
    seed: u64,
    type_keys: Vec<String>,
    export_format: String,
}

impl IntegrityConfig {
    fn from_env() -> Result<(String, Self)> {
        let profile = std::env::var("ANYBACK_INTEGRITY_PROFILE").unwrap_or_else(|_| "small".into());
        let seed = parse_env_u64("ANYBACK_INTEGRITY_SEED").unwrap_or(0x0BAD_5EED);
        let mut cfg = Self::profile(&profile, seed)?;
        if let Some(v) = parse_env_usize("ANYBACK_INTEGRITY_ITERATIONS") {
            cfg.iterations = v;
        }
        if let Some(v) = parse_env_usize("ANYBACK_INTEGRITY_MAX_OBJECTS_PER_ITERATION") {
            cfg.max_objects_per_iteration = v;
        }
        if let Some(v) = parse_env_usize("ANYBACK_INTEGRITY_MAX_BODY_BYTES") {
            cfg.max_body_bytes = v;
        }
        if let Some(v) = parse_env_u64("ANYBACK_INTEGRITY_MAX_SECONDS") {
            cfg.max_seconds = v;
        }
        if let Some(v) = parse_env_usize("ANYBACK_INTEGRITY_MAX_TOTAL_OBJECTS") {
            cfg.max_total_objects = v;
        }
        if let Some(v) = parse_env_usize("ANYBACK_INTEGRITY_MAX_TOTAL_BODY_BYTES") {
            cfg.max_total_body_bytes = v;
        }
        if let Ok(raw) = std::env::var("ANYBACK_INTEGRITY_TYPES") {
            let parsed: Vec<String> = raw
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect();
            if !parsed.is_empty() {
                cfg.type_keys = parsed;
            }
        }
        if let Ok(format) = std::env::var("ANYBACK_INTEGRITY_FORMAT") {
            let normalized = format.trim().to_ascii_lowercase();
            if !normalized.is_empty() {
                ensure!(
                    normalized == "pb" || normalized == "pb-json",
                    "invalid ANYBACK_INTEGRITY_FORMAT '{normalized}' (expected pb|pb-json)"
                );
                cfg.export_format = normalized;
            }
        }
        Ok((profile, cfg))
    }

    fn profile(name: &str, seed: u64) -> Result<Self> {
        let cfg = match name {
            "tiny" => Self {
                iterations: 1,
                max_objects_per_iteration: 3,
                max_body_bytes: 512,
                max_seconds: 30,
                max_total_objects: 3,
                max_total_body_bytes: 2 * 1024,
                seed,
                type_keys: vec!["page".into(), "note".into(), "task".into()],
                export_format: "pb".into(),
            },
            "small" => Self {
                iterations: 4,
                max_objects_per_iteration: 16,
                max_body_bytes: 16 * 1024,
                max_seconds: 240,
                max_total_objects: 64,
                max_total_body_bytes: 256 * 1024,
                seed,
                type_keys: vec!["page".into(), "note".into(), "task".into()],
                export_format: "pb".into(),
            },
            "medium" => Self {
                iterations: 8,
                max_objects_per_iteration: 56,
                max_body_bytes: 96 * 1024,
                max_seconds: 900,
                max_total_objects: 420,
                max_total_body_bytes: 8 * 1024 * 1024,
                seed,
                type_keys: vec![
                    "page".into(),
                    "note".into(),
                    "task".into(),
                    "bookmark".into(),
                ],
                export_format: "pb".into(),
            },
            "large" => Self {
                iterations: 12,
                max_objects_per_iteration: 120,
                max_body_bytes: 256 * 1024,
                max_seconds: 1800,
                max_total_objects: 1_200,
                max_total_body_bytes: 64 * 1024 * 1024,
                seed,
                type_keys: vec![
                    "page".into(),
                    "note".into(),
                    "task".into(),
                    "bookmark".into(),
                ],
                export_format: "pb".into(),
            },
            other => bail!(
                "invalid ANYBACK_INTEGRITY_PROFILE '{other}' (expected tiny|small|medium|large)"
            ),
        };
        Ok(cfg)
    }
}

fn parse_env_usize(key: &str) -> Option<usize> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
}

fn parse_env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|v| v.parse::<u64>().ok())
}

fn random_type<'a>(rng: &mut StdRng, cfg: &'a IntegrityConfig) -> &'a str {
    let idx = rng.random_range(0..cfg.type_keys.len());
    cfg.type_keys[idx].as_str()
}

fn random_body(
    rng: &mut StdRng,
    max_len: usize,
    prefix: &str,
    batch: &[GeneratedCase],
    source_space_id: &str,
    semantic_token: &str,
) -> String {
    let target_len = rng.random_range(64..=max_len.max(64));
    let mut body = format!(
        "# {prefix}\n\nseed={}\nsemantic_token={semantic_token}\n",
        rng.random::<u64>()
    );
    if !batch.is_empty() {
        let idx = rng.random_range(0..batch.len());
        body.push_str(&format!(
            "- local-link-name: {}\n",
            batch[idx].requested_name
        ));
    }
    if !batch.is_empty() {
        let idx = rng.random_range(0..batch.len());
        let id = &batch[idx].id;
        body.push_str(&format!(
            "- object-link: https://object.any.coop/{id}?spaceId={source_space_id}\n"
        ));
    }
    while body.len() < target_len {
        body.push_str("lorem ipsum anytype integrity ");
        body.push_str(&format!("{}\n", rng.random::<u32>()));
    }
    body.truncate(target_len);
    body
}

fn run_backup_restore_batch(
    cfg: &IntegrityConfig,
    source_space_name: &str,
    dest_space_name: &str,
    prefix: &str,
    iteration: usize,
    batch: &[GeneratedCase],
    arg_profile: ExportArgProfile,
) -> Result<BatchArtifacts> {
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let ids_file = temp_dir.path().join("ids.txt");
    write_ids_file(
        &ids_file,
        &batch.iter().map(|g| g.id.clone()).collect::<Vec<_>>(),
    )?;
    let report_path = temp_dir.path().join("report.json");

    let mut backup_args = vec![
        "backup".to_string(),
        "--space".to_string(),
        source_space_name.to_string(),
        "--format".to_string(),
        cfg.export_format.clone(),
        "--objects".to_string(),
        ids_file.display().to_string(),
        "--dir".to_string(),
        temp_dir.path().display().to_string(),
        "--prefix".to_string(),
        format!("{prefix}-batch-{iteration}"),
    ];
    if arg_profile.include_files {
        backup_args.push("--include-files".to_string());
    }
    if arg_profile.include_archived {
        backup_args.push("--include-archived".to_string());
    }
    if arg_profile.include_nested {
        backup_args.push("--include-nested".to_string());
    }
    if arg_profile.include_backlinks {
        backup_args.push("--include-backlinks".to_string());
    }
    let backup_args_ref: Vec<&str> = backup_args.iter().map(String::as_str).collect();
    let backup_output = run_anyback_dyn(&backup_args_ref)?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;
    wait_for_archive_ready(&archive_path)?;
    let manifest_count = read_manifest_object_count(&archive_path)?;
    let min_expected = batch.len();
    if arg_profile.include_nested || arg_profile.include_backlinks || arg_profile.include_archived {
        ensure!(
            manifest_count >= min_expected,
            "expected expanded profile to export at least {min_expected} objects, got {manifest_count}"
        );
    }

    let restore_args = [
        "--json",
        "restore",
        "--space",
        dest_space_name,
        "--log",
        &report_path.display().to_string(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ];
    let restore_output = match run_anyback_restore_with_retry(restore_args) {
        Ok(output) => output,
        Err(err) => {
            let partial = BatchArtifacts {
                archive_path: archive_path.clone(),
                ids_file: ids_file.clone(),
                report_path: report_path.clone(),
                backup_output: backup_output.clone(),
                restore_output: String::new(),
                _temp_dir: temp_dir,
            };
            let persisted = persist_failure_artifacts(
                &partial,
                batch,
                prefix,
                iteration,
                "restore-command-failed",
            );
            return Err(err).with_context(|| match persisted {
                Ok(path) => format!(
                    "restore command failed; diagnostics saved at {}",
                    path.display()
                ),
                Err(save_err) => {
                    format!("restore command failed; additionally failed saving diagnostics: {save_err:#}")
                }
            });
        }
    };
    let parsed: Value = serde_json::from_str(&restore_output)
        .with_context(|| format!("restore output was not valid json: {restore_output}"))?;
    ensure!(
        parsed.get("failed").and_then(Value::as_u64) == Some(0),
        "restore had failures for integrity batch: {parsed}"
    );

    let expected = u64::try_from(batch.len()).unwrap_or(0);
    ensure!(
        parsed.get("attempted").and_then(Value::as_u64) == Some(expected),
        "unexpected attempted count for integrity batch: {parsed}"
    );
    let expected_file_names: Vec<&str> = batch
        .iter()
        .filter(|g| g.type_key == "file")
        .filter_map(|g| g.observed_name.as_deref())
        .collect();
    if !expected_file_names.is_empty() {
        let success_rows = parsed
            .get("success")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("restore report missing success array: {parsed}"))?;
        for file_name in expected_file_names {
            let found = success_rows.iter().any(|row| {
                row.get("type").and_then(Value::as_str) == Some("file")
                    && row.get("name").and_then(Value::as_str) == Some(file_name)
            });
            ensure!(
                found,
                "restore report missing file success row for attachment '{file_name}': {parsed}"
            );
        }
    }

    if cfg.max_body_bytes > 8 * 1024 {
        // Spot-check at least one larger object path in medium/large profiles.
        let largest = batch.iter().max_by_key(|g| g.body_len);
        ensure!(largest.is_some(), "expected non-empty batch");
        let largest = largest.expect("checked above");
        eprintln!(
            "integrity batch {} largest object type={} body_len={}",
            iteration, largest.type_key, largest.body_len
        );
    }
    let artifacts = BatchArtifacts {
        archive_path,
        ids_file,
        report_path,
        backup_output,
        restore_output,
        _temp_dir: temp_dir,
    };
    Ok(artifacts)
}

fn run_full_space_backup_restore(
    cfg: &IntegrityConfig,
    source_space_name: &str,
    dest_space_name: &str,
    prefix: &str,
) -> Result<()> {
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let report_path = temp_dir.path().join("chat-report.json");
    let backup_output = run_anyback([
        "backup",
        "--space",
        source_space_name,
        "--format",
        &cfg.export_format,
        "--dir",
        &temp_dir.path().display().to_string(),
        "--prefix",
        prefix,
    ])?;
    let archive_path = parse_archive_path(&backup_output)
        .ok_or_else(|| anyhow!("could not parse archive path from output: {backup_output}"))?;
    wait_for_archive_ready(&archive_path)?;

    let restore_output = run_anyback([
        "--json",
        "restore",
        "--space",
        dest_space_name,
        "--log",
        &report_path.display().to_string(),
        archive_path
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    let parsed: Value = serde_json::from_str(&restore_output)
        .with_context(|| format!("restore output was not valid json: {restore_output}"))?;
    ensure!(
        parsed.get("failed").and_then(Value::as_u64) == Some(0),
        "restore had failures for full-space chat roundtrip: {parsed}"
    );
    Ok(())
}

fn run_markdown_export_probe(
    source_space_name: &str,
    prefix: &str,
    iteration: usize,
    batch: &[GeneratedCase],
    include_properties: bool,
) -> Result<()> {
    let temp_dir = tempfile::tempdir().context("failed to create markdown temp dir")?;
    let ids_file = temp_dir.path().join("md_ids.txt");
    write_ids_file(
        &ids_file,
        &batch.iter().map(|g| g.id.clone()).collect::<Vec<_>>(),
    )?;

    let mut args = vec![
        "backup".to_string(),
        "--space".to_string(),
        source_space_name.to_string(),
        "--format".to_string(),
        "markdown".to_string(),
        "--objects".to_string(),
        ids_file.display().to_string(),
        "--dir".to_string(),
        temp_dir.path().display().to_string(),
        "--prefix".to_string(),
        format!("{prefix}-md-{iteration}"),
    ];
    if include_properties {
        args.push("--include-properties".to_string());
    }
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run_anyback_dyn(&args_ref)?;
    let archive = parse_archive_path(&output)
        .ok_or_else(|| anyhow!("could not parse markdown archive path from output: {output}"))?;
    wait_for_archive_ready(&archive)?;

    let files = list_archive_files(&archive)?;
    let mut markdown_blob = String::new();
    let mut markdown_count = 0usize;
    for rel in files {
        if !rel.to_ascii_lowercase().ends_with(".md") {
            continue;
        }
        markdown_count += 1;
        let text = fs::read_to_string(archive.join(&rel)).with_context(|| {
            format!(
                "failed to read markdown file {}",
                archive.join(&rel).display()
            )
        })?;
        markdown_blob.push_str(&text);
        markdown_blob.push('\n');
    }
    ensure!(markdown_count > 0, "markdown export produced no .md files");
    if include_properties {
        ensure!(
            markdown_blob.contains("---") || markdown_blob.to_ascii_lowercase().contains("schema"),
            "markdown include-properties probe missing expected properties/schema hints"
        );
    }
    Ok(())
}

fn list_archive_files(archive: &Path) -> Result<Vec<String>> {
    let output = run_anyback([
        "--json",
        "list",
        "--files",
        archive
            .to_str()
            .ok_or_else(|| anyhow!("bad archive path"))?,
    ])?;
    let payload: Value =
        serde_json::from_str(&output).with_context(|| format!("invalid list json: {output}"))?;
    let files = payload
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("list output missing files array"))?;
    Ok(files
        .iter()
        .filter_map(|f| {
            f.get("path")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect())
}

fn read_manifest_object_count(archive: &Path) -> Result<usize> {
    let manifest = fs::read_to_string(archive.join("manifest.json"))
        .with_context(|| format!("missing manifest for {}", archive.display()))?;
    let payload: Value = serde_json::from_str(&manifest)?;
    let count = payload
        .get("object_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("manifest missing object_count"))?;
    usize::try_from(count).context("object_count overflow")
}

fn run_anyback_restore_with_retry<const N: usize>(args: [&str; N]) -> Result<String> {
    const MAX_ATTEMPTS: usize = 4;
    for attempt in 1..=MAX_ATTEMPTS {
        match run_anyback(args) {
            Ok(output) => return Ok(output),
            Err(err) => {
                let text = err.to_string();
                let is_retryable = text.contains("file doesn't match Anyblock format")
                    || text.contains("snapshot is not valid");
                if !is_retryable || attempt == MAX_ATTEMPTS {
                    return Err(err);
                }
                let delay_ms = 1200u64 * u64::try_from(attempt).unwrap_or(1);
                eprintln!(
                    "retrying restore after transient import error (attempt {attempt}/{MAX_ATTEMPTS}, delay={}ms)",
                    delay_ms
                );
                thread::sleep(Duration::from_millis(delay_ms));
            }
        }
    }
    bail!("restore retry loop exhausted unexpectedly")
}

fn wait_for_archive_ready(path: &Path) -> Result<()> {
    const MAX_ATTEMPTS: usize = 8;
    let mut last_sig: Option<(usize, u64)> = None;
    let mut stable_polls = 0usize;
    for _ in 0..MAX_ATTEMPTS {
        let sig = archive_signature(path)?;
        let has_payload = sig.0 > 1 && sig.1 > 0;
        if has_payload && Some(sig) == last_sig {
            stable_polls += 1;
            if stable_polls >= 2 {
                return Ok(());
            }
        } else {
            stable_polls = 0;
        }
        last_sig = Some(sig);
        thread::sleep(Duration::from_millis(350));
    }
    bail!(
        "archive not stable/ready for import after retries: {}",
        path.display()
    )
}

fn archive_signature(path: &Path) -> Result<(usize, u64)> {
    ensure!(
        path.is_dir(),
        "archive path is not a directory: {}",
        path.display()
    );
    let mut file_count = 0usize;
    let mut total_size = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else {
                file_count += 1;
                total_size = total_size.saturating_add(entry.metadata()?.len());
            }
        }
    }
    Ok((file_count, total_size))
}

async fn choose_writable_spaces(client: &AnytypeClient) -> Result<(Space, Space)> {
    let dest_name = std::env::var("ANYBACK_TEST_DEST_SPACE").unwrap_or_else(|_| "test10".into());
    let source_name =
        std::env::var("ANYBACK_TEST_SOURCE_SPACE").unwrap_or_else(|_| "test11".into());

    let source = resolve_space_by_name(client, &source_name).await?;
    let dest = resolve_space_by_name(client, &dest_name).await?;

    ensure!(
        is_writable_space(client, &source.id).await?,
        "source space is not writable: {}",
        source.name
    );
    ensure!(
        is_writable_space(client, &dest.id).await?,
        "destination space is not writable: {}",
        dest.name
    );
    Ok((source, dest))
}

async fn choose_writable_chat_space(client: &AnytypeClient) -> Result<Option<Space>> {
    let spaces = client.spaces().list().await?.collect_all().await?;
    for space in spaces {
        if !space.is_chat() {
            continue;
        }
        if is_writable_space(client, &space.id).await? {
            return Ok(Some(space));
        }
    }
    Ok(None)
}

async fn resolve_space_chat_id(client: &AnytypeClient, space_id: &str) -> Result<String> {
    let chat = client.chats().space_chat(space_id).get().await?;
    Ok(chat.id)
}

async fn wait_chat_message_contains(
    client: &AnytypeClient,
    chat_id: &str,
    token: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let page = client
            .chats()
            .list_messages(chat_id)
            .limit(500)
            .list_page()
            .await?;
        if page
            .messages
            .iter()
            .any(|message| message.content.text.contains(token))
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("chat message token '{token}' not found in chat {chat_id}");
        }
        sleep(Duration::from_millis(500)).await;
    }
}

async fn wait_chat_message_absent(
    client: &AnytypeClient,
    chat_id: &str,
    token: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let page = client
            .chats()
            .list_messages(chat_id)
            .limit(500)
            .list_page()
            .await?;
        if !page
            .messages
            .iter()
            .any(|message| message.content.text.contains(token))
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("chat message token '{token}' still present in chat {chat_id}");
        }
        sleep(Duration::from_millis(300)).await;
    }
}

async fn delete_chat_messages_by_token(
    client: &AnytypeClient,
    chat_id: &str,
    token: &str,
) -> Result<()> {
    let page = client
        .chats()
        .list_messages(chat_id)
        .limit(500)
        .list_page()
        .await?;
    for message in page.messages {
        if message.content.text.contains(token) {
            let _ = client
                .chats()
                .delete_message(chat_id, &message.id)
                .delete()
                .await;
        }
    }
    Ok(())
}

async fn run_regular_space_chat_roundtrip_check(
    client: &AnytypeClient,
    source_space: &Space,
    dest_space: &Space,
    cfg: &IntegrityConfig,
    prefix: &str,
) -> Result<()> {
    let token = format!(
        "{}-regular-chat-{}",
        prefix,
        anytype::test_util::unique_suffix()
    );
    let source_chat_id = resolve_space_chat_id(client, &source_space.id).await?;
    let dest_chat_id = resolve_space_chat_id(client, &dest_space.id).await?;
    let source_message_id = client
        .chats()
        .send_text(&source_chat_id, &token)
        .send()
        .await?;

    let result: Result<()> = async {
        if source_space.id == dest_space.id {
            client
                .chats()
                .delete_message(&source_chat_id, &source_message_id)
                .delete()
                .await?;
            wait_chat_message_absent(client, &source_chat_id, &token, Duration::from_secs(8))
                .await?;
        }
        run_full_space_backup_restore(
            cfg,
            source_space.name.as_str(),
            dest_space.name.as_str(),
            &format!("{prefix}-regular-chat"),
        )?;
        wait_chat_message_contains(client, &dest_chat_id, &token, Duration::from_secs(20)).await
    }
    .await;

    let _ = delete_chat_messages_by_token(client, &source_chat_id, &token).await;
    if dest_chat_id != source_chat_id {
        let _ = delete_chat_messages_by_token(client, &dest_chat_id, &token).await;
    }
    result
}

async fn run_chat_space_roundtrip_check(
    client: &AnytypeClient,
    chat_space: &Space,
    cfg: &IntegrityConfig,
    prefix: &str,
) -> Result<()> {
    let token = format!(
        "{}-chat-space-{}",
        prefix,
        anytype::test_util::unique_suffix()
    );
    let chat_id = resolve_space_chat_id(client, &chat_space.id).await?;
    let source_message_id = client.chats().send_text(&chat_id, &token).send().await?;

    let result: Result<()> = async {
        client
            .chats()
            .delete_message(&chat_id, &source_message_id)
            .delete()
            .await?;
        wait_chat_message_absent(client, &chat_id, &token, Duration::from_secs(8)).await?;
        run_full_space_backup_restore(
            cfg,
            chat_space.name.as_str(),
            chat_space.name.as_str(),
            &format!("{prefix}-chat-space"),
        )?;
        wait_chat_message_contains(client, &chat_id, &token, Duration::from_secs(20)).await
    }
    .await;

    let _ = delete_chat_messages_by_token(client, &chat_id, &token).await;
    result
}

async fn create_attachment_cases(
    client: &AnytypeClient,
    source_space_id: &str,
    prefix: &str,
    iteration: usize,
    rng: &mut StdRng,
    remaining_budget: usize,
    max_body_bytes: usize,
) -> Result<AttachmentCaseBatch> {
    if remaining_budget < 2 {
        return Ok(AttachmentCaseBatch {
            created_ids: Vec::new(),
            cases: Vec::new(),
            body_bytes: 0,
            uploaded_files: 0,
        });
    }

    let mut created_ids = Vec::new();
    let mut cases = Vec::new();
    let semantic_token = format!("anyback-attach-{iteration}-{}", rng.random::<u32>());
    let attachment_name = format!(
        "{prefix}-it{iteration}-attachment-host-{}",
        rng.random::<u32>()
    );
    let attachment_description = format!("attach-desc-{semantic_token}");
    let attachment_body = random_body(
        rng,
        max_body_bytes.min(16 * 1024),
        prefix,
        &cases,
        source_space_id,
        &semantic_token,
    );
    let attachment_page = client
        .new_object(source_space_id, "page")
        .name(&attachment_name)
        .body(&attachment_body)
        .description(&attachment_description)
        .create()
        .await
        .with_context(|| format!("failed creating attachment host page {attachment_name}"))?;
    created_ids.push(attachment_page.id.clone());
    cases.push(GeneratedCase {
        id: attachment_page.id.clone(),
        requested_name: attachment_name,
        observed_name: attachment_page.name.filter(|v| !v.trim().is_empty()),
        type_key: "page".to_string(),
        body_len: attachment_body.len(),
        expected_description: Some(attachment_description),
        expected_markdown_token: Some(semantic_token.clone()),
    });

    let file_name = format!(
        "{prefix}-it{iteration}-attachment-{}.txt",
        rng.random::<u32>()
    );
    let file_temp = tempfile::Builder::new()
        .prefix("anyback-integrity-attachment-")
        .suffix(".txt")
        .tempfile()
        .context("failed to create attachment temp file")?;
    fs::write(
        file_temp.path(),
        format!(
            "file-attachment-token={semantic_token}\nseed={}\n",
            rng.random::<u64>()
        ),
    )
    .with_context(|| {
        format!(
            "failed writing temp attachment file {}",
            file_temp.path().display()
        )
    })?;
    let file_object = client
        .files()
        .upload(source_space_id)
        .from_path(file_temp.path())
        .file_type(FileType::File)
        .created_in_context(&attachment_page.id)
        .details(serde_json::json!({ "name": file_name }))
        .upload()
        .await
        .with_context(|| format!("failed uploading attachment file {file_name}"))?;
    created_ids.push(file_object.id.clone());
    cases.push(GeneratedCase {
        id: file_object.id,
        requested_name: file_name.clone(),
        observed_name: file_object.name.filter(|v| !v.trim().is_empty()),
        type_key: "file".to_string(),
        body_len: 0,
        expected_description: None,
        expected_markdown_token: None,
    });

    Ok(AttachmentCaseBatch {
        created_ids,
        cases,
        body_bytes: attachment_body.len(),
        uploaded_files: 1,
    })
}

async fn resolve_space_by_name(client: &AnytypeClient, name: &str) -> Result<Space> {
    let spaces = client.spaces().list().await?.collect_all().await?;
    let needle = name.to_lowercase();
    let matches: Vec<_> = spaces
        .into_iter()
        .filter(|space| space.name.to_lowercase() == needle)
        .collect();
    match matches.len() {
        0 => bail!("space not found: {name}"),
        1 => Ok(matches[0].clone()),
        _ => bail!("space name is ambiguous: {name}"),
    }
}

async fn is_writable_space(client: &AnytypeClient, space_id: &str) -> Result<bool> {
    let probe_name = format!(
        "anyback-integrity-probe-{}",
        anytype::test_util::unique_suffix()
    );
    match client
        .new_object(space_id, "page")
        .name(&probe_name)
        .body("probe")
        .create()
        .await
    {
        Ok(object) => {
            let _ = client.object(space_id, &object.id).delete().await;
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

async fn wait_validate_batch_semantics(
    client: &AnytypeClient,
    space_id: &str,
    batch: &[GeneratedCase],
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let objects = client
            .objects(space_id)
            .limit(10_000)
            .list()
            .await?
            .collect_all()
            .await?;

        let mut pending_or_invalid = Vec::new();
        for case in batch {
            if case.type_key == "file" {
                continue;
            }

            let Some(expected_name) = case.observed_name.as_deref() else {
                continue;
            };
            let candidate = objects.iter().find(|obj| {
                obj.name.as_deref() == Some(expected_name)
                    && obj.r#type.as_ref().is_some_and(|t| t.key == case.type_key)
            });
            let Some(candidate) = candidate else {
                pending_or_invalid.push(format!(
                    "object '{}' (type {}) not available yet",
                    expected_name, case.type_key
                ));
                continue;
            };

            let full = match client.object(space_id, &candidate.id).get().await {
                Ok(object) => object,
                Err(err) => {
                    pending_or_invalid.push(format!(
                        "object '{}' could not be read yet: {err}",
                        expected_name
                    ));
                    continue;
                }
            };
            if let Some(actual_name) = full.name.as_deref()
                && actual_name != expected_name
            {
                pending_or_invalid.push(format!(
                    "name mismatch for '{}': got '{}'",
                    expected_name, actual_name
                ));
            }
            if let Some(expected_desc) = case.expected_description.as_deref() {
                let actual_desc = full.get_property_str("description").unwrap_or_default();
                if actual_desc != expected_desc {
                    pending_or_invalid.push(format!(
                        "description mismatch for '{}': expected '{}', got '{}'",
                        expected_name, expected_desc, actual_desc
                    ));
                }
            }
            if let Some(marker) = case.expected_markdown_token.as_deref() {
                let markdown = full.markdown.as_deref().unwrap_or_default();
                if !markdown.contains(marker) {
                    pending_or_invalid.push(format!(
                        "markdown marker missing for '{}': marker '{}'",
                        expected_name, marker
                    ));
                }
            }
            if full.r#type.as_ref().is_none_or(|t| t.key != case.type_key) {
                pending_or_invalid.push(format!(
                    "type mismatch for '{}': expected '{}'",
                    expected_name, case.type_key
                ));
            }
        }

        if pending_or_invalid.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "integrity validation timed out with pending semantic checks: {pending_or_invalid:?}"
            );
        }
        sleep(Duration::from_millis(500)).await;
    }
}

async fn cleanup_source_ids(client: &AnytypeClient, space_id: &str, ids: &[String]) -> Result<()> {
    for id in ids {
        let _ = client.object(space_id, id).delete().await;
    }
    Ok(())
}

async fn cleanup_by_prefix(client: &AnytypeClient, space_id: &str, prefix: &str) -> Result<()> {
    let objects = client
        .objects(space_id)
        .filter(Filter::text_contains("name", prefix))
        .list()
        .await?
        .collect_all()
        .await?;
    for object in objects {
        if object
            .name
            .as_deref()
            .is_some_and(|name| name.contains(prefix))
        {
            let _ = client.object(space_id, &object.id).delete().await;
        }
    }
    Ok(())
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
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "anyback command failed (status={}):\nstdout:\n{}\nstderr:\n{}",
            output.status,
            stdout,
            stderr
        );
    }

    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.stderr.is_empty() {
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    Ok(text)
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

fn delete_objects_by_prefix_sync(space_name: &str, prefix: &str) -> Result<()> {
    for id in list_object_ids_by_prefix_sync(space_name, prefix)? {
        let _ = run_anyr(["object", "delete", space_name, &id]);
    }
    Ok(())
}

fn list_object_ids_by_prefix_sync(space_name: &str, prefix: &str) -> Result<Vec<String>> {
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

fn run_with_lock_retry<F>(mut run: F) -> Result<std::process::Output>
where
    F: FnMut() -> Result<std::process::Output>,
{
    const MAX_ATTEMPTS: usize = 5;
    const BACKOFF_MS: [u64; MAX_ATTEMPTS] = [0, 500, 1_500, 3_000, 10_000];

    let mut last_output: Option<std::process::Output> = None;
    for (attempt, delay_ms) in BACKOFF_MS.iter().enumerate() {
        if *delay_ms > 0 {
            thread::sleep(Duration::from_millis(*delay_ms));
        }
        let output = run()?;
        if output.status.success() || !looks_like_keyring_lock_error(&output) {
            return Ok(output);
        }
        eprintln!(
            "command hit keyring lock; retrying ({}/{MAX_ATTEMPTS})",
            attempt + 1
        );
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
        "anyback-integrity-keystore-{}-{}.db",
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

fn persist_failure_artifacts(
    artifacts: &BatchArtifacts,
    batch: &[GeneratedCase],
    prefix: &str,
    iteration: usize,
    reason: &str,
) -> Result<PathBuf> {
    let root = std::env::temp_dir().join("anyback-integrity-failures");
    fs::create_dir_all(&root)?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let dir = root.join(format!(
        "{stamp}-{reason}-it{iteration}-{}",
        anytype::test_util::unique_suffix()
    ));
    fs::create_dir_all(&dir)?;

    let batch_json = serde_json::to_string_pretty(batch)
        .context("failed to serialize generated batch metadata")?;
    fs::write(dir.join("batch.json"), batch_json)?;
    fs::write(dir.join("backup_output.txt"), &artifacts.backup_output)?;
    fs::write(dir.join("restore_output.txt"), &artifacts.restore_output)?;

    let ids_copy = artifacts
        .ids_file
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ids.txt"));
    let _ = fs::copy(&artifacts.ids_file, dir.join(ids_copy));

    if artifacts.report_path.exists() {
        let report_copy = artifacts
            .report_path
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("report.json"));
        let _ = fs::copy(&artifacts.report_path, dir.join(report_copy));
    }

    let archive_dest = dir.join("archive");
    copy_dir_recursive(&artifacts.archive_path, &archive_dest)?;
    let listing = archive_file_listing(&archive_dest)?;
    fs::write(dir.join("archive_files.txt"), listing)?;

    fs::write(
        dir.join("README.txt"),
        format!(
            "reason={reason}\niteration={iteration}\nprefix={prefix}\narchive={}\n",
            artifacts.archive_path.display()
        ),
    )?;

    Ok(dir)
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    ensure!(
        src.is_dir(),
        "source directory for recursive copy is not a directory: {}",
        src.display()
    );
    fs::create_dir_all(dest)?;
    let mut stack = vec![(src.to_path_buf(), dest.to_path_buf())];
    while let Some((from_dir, to_dir)) = stack.pop() {
        for entry in fs::read_dir(&from_dir)? {
            let entry = entry?;
            let from_path = entry.path();
            let to_path = to_dir.join(entry.file_name());
            if from_path.is_dir() {
                fs::create_dir_all(&to_path)?;
                stack.push((from_path, to_path));
            } else {
                fs::copy(&from_path, &to_path).with_context(|| {
                    format!(
                        "failed copying failure artifact file {} to {}",
                        from_path.display(),
                        to_path.display()
                    )
                })?;
            }
        }
    }
    Ok(())
}

fn archive_file_listing(path: &Path) -> Result<String> {
    ensure!(path.is_dir(), "archive listing path is not a directory");
    let mut rows = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(path)
                    .with_context(|| format!("failed to relativize {}", p.display()))?;
                let bytes = entry.metadata()?.len();
                rows.push((rel.to_string_lossy().to_string(), bytes));
            }
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    for (path, bytes) in rows {
        out.push_str(&format!("{bytes:>10} {path}\n"));
    }
    Ok(out)
}

fn write_ids_file(path: &Path, ids: &[String]) -> Result<()> {
    let mut file = fs::File::create(path)
        .with_context(|| format!("failed to create id file {}", path.display()))?;
    for id in ids {
        writeln!(file, "{id}")?;
    }
    Ok(())
}
