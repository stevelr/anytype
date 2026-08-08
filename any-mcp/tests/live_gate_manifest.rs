//! Offline closed-inventory checks for the any-mcp ignored live gate.

use std::{collections::BTreeSet, path::Path, process::Command};

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
    "server::headless_integration::headless_artifact_direct_transport_matrix_scenario",
    "server::headless_integration::headless_artifact_policy_direct_scenarios",
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

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("any-mcp manifest has a workspace parent")
}

fn ignored_tests(target: &str, acceptance_harness: bool) -> BTreeSet<String> {
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
    let output = command
        .args(["--", "--list", "--ignored"])
        .output()
        .unwrap_or_else(|error| panic!("list ignored tests for {target}: {error}"));
    assert!(
        output.status.success(),
        "listing ignored tests for {target} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_suffix(": test").map(str::to_owned))
        .collect()
}

fn assert_sorted_unique(entries: &[&str]) {
    assert!(
        entries.windows(2).all(|pair| pair[0] < pair[1]),
        "manifest entries must be sorted and unique"
    );
}

fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

#[test]
fn ignored_library_manifest_is_closed_and_filter_safe() {
    assert_eq!(ADMITTED_IGNORED_LIB_TESTS.len(), 32);
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
    assert_eq!(declared.len(), 42, "manifest contains duplicate entries");
    assert_eq!(ignored_tests("lib", false), declared);
}

#[test]
fn whole_binary_live_targets_match_the_gate_pins() {
    assert_eq!(
        ignored_tests("headless_stdio_e2e", true).len(),
        25,
        "headless stdio live-test count changed"
    );
    assert_eq!(
        ignored_tests("discussions_stdio_acceptance", true).len(),
        1,
        "discussions process live-test count changed"
    );
}

#[test]
fn workflow_runs_each_pinned_live_target_in_both_protected_jobs() {
    let workflow = include_str!("../../.github/workflows/any-mcp.yml");
    for needle in [
        "run_required_live_gate direct 32 cargo test",
        "run_required_live_gate stdio 25 cargo test",
        "run_required_live_gate discussions 1 cargo test",
    ] {
        assert_eq!(
            occurrences(workflow, needle),
            2,
            "workflow must run {needle:?} in both protected jobs"
        );
    }
    assert_eq!(occurrences(workflow, "group: anytype-headless-live"), 2);
    assert!(!workflow.contains("group: any-mcp-headless"));
    assert_eq!(occurrences(workflow, "--test live_gate_manifest"), 1);
}
