//! Offline closed-inventory checks for the any-mcp ignored live gate.

use std::{collections::BTreeSet, path::Path, process::Command};

use sha2::{Digest, Sha256};

const ADMITTED_IGNORED_LIB_TESTS: &[&str] = &[
    "chat_add_toolset::tests::headless_direct_and_preview_stdio_add_concurrent_replay_and_capacity_paths",
    "chat_delete_toolset::tests::headless_direct_and_spawned_stdio_delete_conflict_and_absence",
    "chat_read_toolset::tests::headless_direct_and_stdio_reads_use_cleanup_owned_real_chat",
    "collection_member_toolset::tests::headless_direct_membership_ignores_saved_view_presentation",
    "discussion_toolset::tests::headless_disposable_absent_attached_repeat_and_protocol_parity",
    "file_content::tests::headless_production_direct_and_stdio_files_native_ranges_hash_and_cleanup",
    "object_edit::tests::headless_edit_round_trip_is_cleanup_safe",
    "schema_property_toolset::tests::headless_property_create_update_direct_stdio_and_cache_bounds",
    "schema_space_toolset::tests::headless_direct_stdio_and_ambiguity_use_disposable_real_spaces",
    "schema_tag_toolset::tests::headless_direct_stdio_cache_and_scope_use_disposable_real_space",
    "schema_type_toolset::tests::headless_type_preserve_replace_clear_and_featured_stability",
    "server::headless_integration::headless_archive_applies_and_returns_verified_success",
    "server::headless_integration::headless_artifact_alias_metadata_direct_scenarios",
    "server::headless_integration::headless_artifact_bounded_metadata_direct_scenarios",
    "server::headless_integration::headless_artifact_direct_transport_matrix_scenario",
    "server::headless_integration::headless_artifact_dynamic_filesystem_direct_scenarios",
    "server::headless_integration::headless_artifact_failed_operation_cleanup_direct_scenarios",
    "server::headless_integration::headless_artifact_partial_write_direct_scenarios",
    "server::headless_integration::headless_artifact_policy_direct_scenarios",
    "server::headless_integration::headless_artifact_traversal_direct_scenarios",
    "server::headless_integration::headless_create_body_canonicalization_is_verified_once",
    "server::headless_integration::headless_default_discovery_routes_paginate_and_report_ambiguity",
    "server::headless_integration::headless_direct_body_blocks_runs_shared_scenario",
    "server::headless_integration::headless_direct_chats_registry_runs_all_six_workflows",
    "server::headless_integration::headless_direct_compact_read_sentinel",
    "server::headless_integration::headless_direct_members_minimizes_personal_data",
    "server::headless_integration::headless_direct_ordinary_tools_cover_representative_layouts",
    "server::headless_integration::headless_direct_read_only_sentinel",
    "server::headless_integration::headless_direct_standard_archive",
    "server::headless_integration::headless_direct_standard_discovery",
    "server::headless_integration::headless_direct_standard_documents",
    "server::headless_integration::headless_direct_standard_markdown_noop",
    "server::headless_integration::headless_direct_standard_mutations",
    "server::headless_integration::headless_direct_standard_views",
    "server::headless_integration::headless_exact_edit_accepts_a_converged_arbitrary_body",
    "server::headless_integration::headless_mutations_are_visible_idempotent_and_conflict_safe",
    "server::headless_integration::headless_shared_filters_conform_and_preserve_server_pagination",
    "server::headless_integration::headless_view_body_and_resource_routes_are_complete_and_bound",
];

const EXCLUDED_IGNORED_LIB_TESTS: &[(&str, &str)] = &[
    (
        "body_toolset::tests::print_production_token_budget_snapshot",
        "manual reviewed snapshot reporter",
    ),
    (
        "chats_toolset::tests::print_production_chats_token_budget_snapshot",
        "manual reviewed snapshot reporter",
    ),
    (
        "discussion_toolset::tests::read_only_fixture_exposes_page_discussion_without_retaining_content",
        "configured read-only characterization fixture",
    ),
    (
        "file_content::tests::report_files_production_surface_snapshot",
        "manual reviewed snapshot reporter",
    ),
    (
        "file_content::tests::report_files_snapshots",
        "manual reviewed snapshot reporter",
    ),
    (
        "process_tests::production_runtime_worker_stack_probe",
        "isolated subprocess probe owned by the worker-contract test",
    ),
    (
        "schema_toolset::tests::print_production_schema_token_budget_snapshot",
        "manual reviewed snapshot reporter",
    ),
    (
        "server::optional_registry::write_optional_snapshots",
        "manual reviewed snapshot updater",
    ),
    (
        "server::tests::report_compact_catalog_token_breakdown",
        "manual catalog diagnostic reporter",
    ),
    (
        "server::tests::write_catalog_snapshots",
        "manual reviewed snapshot updater",
    ),
];

const HEADLESS_STDIO_IGNORED_TESTS: &[&str] = &[
    "headless_artifact_adversarial_spawned_stdio_scenarios",
    "headless_artifact_content_spawned_scenarios",
    "headless_artifact_crash06_mid_frame_scenario",
    "headless_artifact_crash_restart_scenarios",
    "headless_artifact_exact_cancellation_spawned_scenarios",
    "headless_artifact_lifecycle_and_payload_scenarios",
    "headless_artifact_policy_spawned_scenarios",
    "headless_artifact_spawned_transport_matrix_scenario",
    "headless_artifact_validator_flood_spawned_scenarios",
    "headless_body_blocks_direct_stable_preview_and_object_show",
    "headless_body_blocks_shared_direct_stable_preview_scenarios",
    "headless_stdio_all_optional_toolsets_compose_in_rw_and_preview_ro_children",
    "headless_stdio_all_registered_optional_real_workflows",
    "headless_stdio_chats_registry_runs_stable_and_preview_workflows",
    "headless_stdio_compact_sentinel",
    "headless_stdio_disposable_lifecycle_sentinel",
    "headless_stdio_disposable_panic_cleanup_sentinel",
    "headless_stdio_files_registry_runs_stable_and_preview_workflows",
    "headless_stdio_members_minimizes_personal_data",
    "headless_stdio_ordinary_tools_cover_representative_layouts",
    "headless_stdio_preview_sentinel",
    "headless_stdio_read_only_sentinel",
    "headless_stdio_schema_registry_runs_all_nine_workflows",
    "headless_stdio_standard_archive",
    "headless_stdio_standard_discovery",
    "headless_stdio_standard_documents",
    "headless_stdio_standard_markdown_noop",
    "headless_stdio_standard_mutations",
    "headless_stdio_standard_views",
    "shared_direct_stable_preview_views_write_acceptance_is_exact",
];

const DISCUSSIONS_STDIO_IGNORED_TESTS: &[&str] =
    &["cleanup_owned_stable_and_preview_processes_cover_real_discussions"];

/// Released targets of the artifact data-plane platform matrix, as the
/// portable workflow job declares them.
const PORTABLE_PLATFORM_MATRIX: [(&str, &str); 5] = [
    ("ubuntu-latest", "linux-x86_64"),
    ("ubuntu-24.04-arm", "linux-aarch64"),
    ("macos-latest", "macos-aarch64"),
    ("windows-latest", "windows-x86_64"),
    ("windows-11-arm", "windows-aarch64"),
];

/// Whitespace-compacted artifact acceptance and adversarial commands that
/// every platform row runs.
const PORTABLE_ARTIFACT_SUITES: [&str; 2] = [
    "cargo test --locked -p any-mcp --features acceptance-harness --lib artifact -- --test-threads=1",
    "cargo test --locked -p any-mcp --features acceptance-harness --test headless_stdio_e2e artifact -- --test-threads=1",
];

/// Name filter shared by both compiled artifact control planes.
const PORTABLE_ARTIFACT_FILTER: &str = "artifact";

/// Smallest portable artifact selection each control plane must keep.
///
/// A name filter that matched nothing would pass silently on every platform,
/// so the floor is deliberately far below the current inventories while still
/// rejecting a collapsed selection. Exact counts stay unpinned because
/// acceptance slices change them often.
const PORTABLE_ARTIFACT_FLOOR: usize = 40;

#[derive(Debug, Eq, PartialEq)]
struct InventoryDrift {
    missing: BTreeSet<String>,
    unexpected: BTreeSet<String>,
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("any-mcp manifest has a workspace parent")
}

fn listed_tests(
    target: &str,
    acceptance_harness: bool,
    filter: Option<&str>,
    ignored_only: bool,
) -> BTreeSet<String> {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(workspace_root())
        .args(["test", "--locked", "-p", "any-mcp"]);
    if acceptance_harness {
        command.args(["--features", "acceptance-harness"]);
    }
    if target == "lib" {
        command.arg("--lib");
    } else {
        command.args(["--test", target]);
    }
    command.arg("--");
    if let Some(filter) = filter {
        command.arg(filter);
    }
    command.arg("--list");
    if ignored_only {
        command.arg("--ignored");
    }
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("list tests for {target}: {error}"));
    assert!(
        output.status.success(),
        "listing tests for {target} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_suffix(": test").map(str::to_owned))
        .collect()
}

fn ignored_tests(target: &str, acceptance_harness: bool) -> BTreeSet<String> {
    listed_tests(target, acceptance_harness, None, true)
}

fn assert_sorted_unique(entries: &[&str]) {
    assert!(
        entries.windows(2).all(|pair| pair[0] < pair[1]),
        "manifest entries must be sorted and unique"
    );
}

fn inventory_drift(expected: &[&str], actual: &BTreeSet<String>) -> Option<InventoryDrift> {
    let expected = expected
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let missing = expected
        .difference(actual)
        .cloned()
        .collect::<BTreeSet<_>>();
    let unexpected = actual
        .difference(&expected)
        .cloned()
        .collect::<BTreeSet<_>>();
    if missing.is_empty() && unexpected.is_empty() {
        None
    } else {
        Some(InventoryDrift {
            missing,
            unexpected,
        })
    }
}

fn test_names(entries: &[&str]) -> BTreeSet<String> {
    entries.iter().copied().map(str::to_owned).collect()
}

fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

fn workflow_job<'a>(workflow: &'a str, job: &str, next_job: Option<&str>) -> &'a str {
    let start_marker = format!("  {job}:\n");
    let start = workflow
        .find(&start_marker)
        .unwrap_or_else(|| panic!("workflow job {job} is missing"));
    let end = next_job
        .map(|next| {
            workflow[start + start_marker.len()..]
                .find(&format!("  {next}:\n"))
                .map(|offset| start + start_marker.len() + offset)
                .unwrap_or(workflow.len())
        })
        .unwrap_or(workflow.len());
    &workflow[start..end]
}

fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn shared_headless_provisioner_honors_the_cli_override() {
    let provisioner = include_str!("../../.github/scripts/provision-headless-server.sh");
    assert!(provisioner.contains("ANYTYPE_CLI_BIN=\"${ANYTYPE_CLI_BIN:-anytype}\""));
    assert!(provisioner.contains("command -v -- \"$ANYTYPE_CLI_BIN\""));
    assert!(provisioner.contains("network_mode=\"${ANYTYPE_HEADLESS_NETWORK_MODE:-isolated}\""));
    assert!(provisioner.contains("isolated | connected"));
    assert!(!provisioner.contains("command -v anytype"));
}

#[test]
fn ignored_library_manifest_is_closed_and_filter_safe() {
    assert_eq!(ADMITTED_IGNORED_LIB_TESTS.len(), 38);
    assert_sorted_unique(ADMITTED_IGNORED_LIB_TESTS);
    assert!(
        ADMITTED_IGNORED_LIB_TESTS
            .iter()
            .all(|test| test.contains("headless_")),
        "every admitted library test must match the protected filter"
    );

    let excluded = EXCLUDED_IGNORED_LIB_TESTS
        .iter()
        .map(|(test, reason)| {
            assert!(!reason.is_empty(), "excluded test {test} needs a rationale");
            assert!(
                !test.contains("headless_"),
                "excluded test {test} unexpectedly matches the live filter"
            );
            *test
        })
        .collect::<Vec<_>>();
    assert_sorted_unique(&excluded);

    let declared = ADMITTED_IGNORED_LIB_TESTS
        .iter()
        .copied()
        .chain(excluded.iter().copied())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    assert_eq!(declared.len(), 48, "manifest contains duplicate entries");
    assert_eq!(ignored_tests("lib", false), declared);
}

#[test]
fn whole_binary_live_target_manifests_are_closed() {
    assert_sorted_unique(HEADLESS_STDIO_IGNORED_TESTS);
    assert_sorted_unique(DISCUSSIONS_STDIO_IGNORED_TESTS);
    assert_eq!(HEADLESS_STDIO_IGNORED_TESTS.len(), 30);
    assert_eq!(DISCUSSIONS_STDIO_IGNORED_TESTS.len(), 1);
    assert_eq!(
        inventory_drift(
            HEADLESS_STDIO_IGNORED_TESTS,
            &ignored_tests("headless_stdio_e2e", true)
        ),
        None,
        "headless stdio ignored-test inventory changed"
    );
    assert_eq!(
        inventory_drift(
            DISCUSSIONS_STDIO_IGNORED_TESTS,
            &ignored_tests("discussions_stdio_acceptance", true)
        ),
        None,
        "discussions process ignored-test inventory changed"
    );
}

#[test]
fn artifact_suite_filter_selects_a_populated_portable_matrix() {
    for target in ["lib", "headless_stdio_e2e"] {
        let selected = listed_tests(target, true, Some(PORTABLE_ARTIFACT_FILTER), false);
        assert!(
            selected
                .iter()
                .all(|test| test.contains(PORTABLE_ARTIFACT_FILTER)),
            "artifact filter selected an unrelated test in {target}"
        );
        let live = listed_tests(target, true, Some(PORTABLE_ARTIFACT_FILTER), true);
        let portable = selected.difference(&live).count();
        assert!(
            portable >= PORTABLE_ARTIFACT_FLOOR,
            "artifact control plane {target} selects only {portable} portable tests"
        );
    }
}

#[test]
fn inventory_comparison_rejects_a_renamed_test() {
    let expected = ["headless_alpha", "headless_beta"];
    let actual = test_names(&["headless_alpha", "headless_beta_renamed"]);
    assert_eq!(
        inventory_drift(&expected, &actual),
        Some(InventoryDrift {
            missing: test_names(&["headless_beta"]),
            unexpected: test_names(&["headless_beta_renamed"]),
        })
    );
}

#[test]
fn inventory_comparison_rejects_a_removed_test() {
    let expected = ["headless_alpha", "headless_beta"];
    let actual = test_names(&["headless_alpha"]);
    assert_eq!(
        inventory_drift(&expected, &actual),
        Some(InventoryDrift {
            missing: test_names(&["headless_beta"]),
            unexpected: BTreeSet::new(),
        })
    );
}

#[test]
fn inventory_comparison_rejects_a_same_count_replacement() {
    let expected = ["headless_alpha", "headless_beta"];
    let actual = test_names(&["headless_alpha", "headless_gamma"]);
    assert_eq!(actual.len(), expected.len());
    assert_eq!(
        inventory_drift(&expected, &actual),
        Some(InventoryDrift {
            missing: test_names(&["headless_beta"]),
            unexpected: test_names(&["headless_gamma"]),
        })
    );
}

#[test]
fn workflow_isolates_protected_jobs_to_trusted_events_and_pinned_actions() {
    // Every platform row of the matrix runs this gate, and a Windows checkout
    // may translate the committed line endings. The reviewed representation is
    // therefore pinned in its canonical newline form.
    //
    // Updating after an intentional workflow change: review the workflow
    // diff, run this test, and replace the pinned digest with the reported
    // `left` value (equivalently `sha256sum .github/workflows/any-mcp.yml`
    // on an LF checkout). The structural assertions below are the audit
    // checklist; extend them when the change adds or removes an invariant.
    let workflow = include_str!("../../.github/workflows/any-mcp.yml").replace("\r\n", "\n");
    let workflow = workflow.as_str();
    let digest = Sha256::digest(workflow.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        digest, "a847f21ef6938c7426be615bb7e3a01f0ca812149e730b04b8e125e406e5bb87",
        "workflow policy is an exact reviewed representation; audit before updating this digest"
    );
    let portable = workflow_job(workflow, "portable-contracts", Some("headless-e2e"));
    let live = workflow_job(workflow, "headless-e2e", Some("headless-clean-server-soak"));
    let clean = workflow_job(workflow, "headless-clean-server-soak", None);

    assert!(workflow.contains("  workflow_dispatch:\n"));
    assert!(!workflow.contains("  pull_request:\n"));
    assert!(!workflow.contains("  push:\n"));
    assert!(!workflow.contains("  schedule:\n"));
    assert!(!portable.contains("self-hosted"));
    assert_eq!(occurrences(portable, "if: runner.os == 'Linux'"), 1);

    let compact_portable = compact_whitespace(portable);
    for (os, platform) in PORTABLE_PLATFORM_MATRIX {
        assert!(
            compact_portable.contains(&format!("- os: {os} platform: {platform}")),
            "portable matrix is missing platform row {platform}"
        );
    }
    assert_eq!(
        occurrences(portable, "- os: "),
        PORTABLE_PLATFORM_MATRIX.len(),
        "portable matrix carries an undeclared platform row"
    );
    for suite in PORTABLE_ARTIFACT_SUITES {
        assert!(
            compact_portable.contains(suite),
            "portable artifact suite step is missing {suite}"
        );
    }

    for (block, predicate) in [
        (
            live,
            "if: ${{ inputs.tier == 'live' || inputs.tier == 'all' }}",
        ),
        (
            clean,
            "if: ${{ inputs.tier == 'clean-server' || inputs.tier == 'all' }}",
        ),
    ] {
        assert!(compact_whitespace(block).contains(predicate));
        assert!(block.contains("needs: portable-contracts"));
        assert!(block.contains("runs-on: ubuntu-24.04"));
        assert!(block.contains("provision-headless-server.sh ANY_MCP_HEADLESS"));
        assert!(block.contains("loginctl enable-linger"));
        assert!(!block.contains("tee"));
        assert!(block.contains("reviewed-evidence.py start"));
        assert!(block.contains("reviewed-evidence.py capture"));
        assert!(block.contains("ANY_MCP_HEADLESS_EVIDENCE_CONTEXT"));
        assert!(block.contains("ANY_MCP_HEADLESS_REVIEWED_LOG_FILE"));
        assert!(block.contains("retention-days: 7"));
        assert!(block.contains("\"$RUNNER_TEMP\"/any-mcp-live-??????"));
        assert!(block.contains("systemctl --user show-environment"));
        for label in ["direct", "stdio", "discussions"] {
            assert!(block.contains(&format!("run-live-cgroup.sh test {label} --")));
        }
        for target in [
            "--lib headless_ -- \\",
            "--test headless_stdio_e2e -- --ignored",
        ] {
            assert!(
                block.contains(target),
                "live gate narrowed a whole-target run and can drop artifact owners: {target}"
            );
        }
    }
    // The disposable per-runner server replaced the retired self-hosted
    // anytype-headless runner outright: no runner labels, protection
    // environment, cross-run concurrency group, or host reset script remain.
    assert!(!workflow.contains("self-hosted"));
    assert!(!workflow.contains("anytype-headless"));
    assert!(!workflow.contains("command reset --"));
    assert_eq!(
        occurrences(workflow, "provision-headless-server.sh ANY_MCP_HEADLESS"),
        2
    );
    assert_eq!(
        occurrences(workflow, "run-live-cgroup.sh command auth --"),
        2
    );
    assert_eq!(occurrences(workflow, "--test live_gate_manifest"), 1);

    let action_lines = workflow
        .lines()
        .filter(|line| {
            let line = line.trim();
            line.starts_with("- uses:") || line.starts_with("uses:")
        })
        .collect::<Vec<_>>();
    assert_eq!(action_lines.len(), 13);
    for line in action_lines {
        let reference = line
            .split_once('@')
            .map(|(_, reference)| reference.split_whitespace().next().unwrap_or(""))
            .unwrap_or("");
        assert_eq!(reference.len(), 40, "action is not pinned: {line}");
        assert!(
            reference.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "action is not pinned: {line}"
        );
    }
    assert_eq!(
        occurrences(
            workflow,
            "actions/checkout@11d5960a326750d5838078e36cf38b85af677262"
        ),
        1
    );
    assert_eq!(
        occurrences(
            workflow,
            "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0"
        ),
        3
    );
    assert_eq!(
        occurrences(
            workflow,
            "DeterminateSystems/nix-installer-action@ef8a148080ab6020fd15196c2084a2eea5ff2d25"
        ),
        3
    );
    assert_eq!(
        occurrences(
            workflow,
            "actions/setup-python@ece7cb06caefa5fff74198d8649806c4678c61a1"
        ),
        1
    );
    assert_eq!(occurrences(workflow, "run: rustup show"), 1);
    assert_eq!(
        occurrences(
            workflow,
            "Swatinem/rust-cache@49a0bdc70d2e1b713ca9e2869b211fcce03d3c1c"
        ),
        3
    );
    assert_eq!(
        occurrences(
            workflow,
            "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02"
        ),
        2
    );
}

#[test]
fn live_helpers_pin_counts_and_source_bound_fresh_evidence() {
    let runner = include_str!("../scripts/run-live-gate.py");
    let evidence = include_str!("../scripts/reviewed-evidence.py");
    let reviewer = include_str!("../scripts/review-server-log.py");
    let cgroup = include_str!("../scripts/run-live-cgroup.sh");
    let helper_tests = include_str!("../scripts/test_live_gate_security.py");

    assert!(runner.contains(&format!(
        "EXPECTED = {{\"direct\": {}, \"stdio\": {}, \"discussions\": {}}}",
        ADMITTED_IGNORED_LIB_TESTS.len(),
        HEADLESS_STDIO_IGNORED_TESTS.len(),
        DISCUSSIONS_STDIO_IGNORED_TESTS.len()
    )));
    assert!(runner.contains("OUTPUT_LIMIT = 1024 * 1024"));
    assert!(runner.contains("TemporaryFile(dir=private_dir)"));
    assert!(runner.contains("os.chmod(transcript.fileno(), 0o600)"));
    assert!(!runner.contains("tee"));
    assert!(!runner.contains("print(output"));

    for required in [
        "O_NOFOLLOW",
        "os.fstat(descriptor)",
        "metadata.st_dev",
        "metadata.st_ino",
        "metadata.st_size < start_bytes",
        "hashlib.sha256(anchor).hexdigest()",
        "FRESH_ARTIFACT_LIMIT = 64_000",
        "ARTIFACT_LIMIT = 65_536",
        "reviewed_log_invalid",
        "reviewed_log_unavailable",
        "object_pairs_hook=unique_object",
    ] {
        assert!(
            evidence.contains(required),
            "missing evidence guard {required:?}"
        );
    }
    assert!(helper_tests.contains("stale-allowlisted-event"));
    assert!(helper_tests.contains("assertNotIn"));
    assert!(helper_tests.contains("os.replace"));
    assert!(helper_tests.contains("source.chmod(0o640)"));
    assert!(helper_tests.contains("PRIVATE_MALFORMED"));
    assert!(!evidence.contains("payload = payload + fresh"));
    for required in [
        "LINE_BYTES = 64 * 1024",
        "O_NOFOLLOW",
        "metadata.st_mode & 0o777 != 0o600",
        "server_oversized",
        "separators=(\",\", \":\")",
    ] {
        assert!(
            reviewer.contains(required),
            "missing server-log reviewer guard {required:?}"
        );
    }
    assert!(!reviewer.contains("decode("));
    for required in [
        "systemd-run --user --scope",
        "--property=RuntimeMaxSec=1100s",
        "trap cleanup EXIT",
        "systemctl --user stop \"$unit\"",
        "secrets.token_hex(8)",
    ] {
        assert!(
            cgroup.contains(required),
            "missing cgroup guard {required:?}"
        );
    }
}
