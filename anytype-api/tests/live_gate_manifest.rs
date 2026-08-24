//! Offline closed-inventory checks for the disposable ignored-test live gate.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::Command,
};

#[derive(Debug, Default)]
struct Manifest {
    version: u8,
    required: Vec<Entry>,
    account_global: Vec<Entry>,
    soak: Vec<Entry>,
    excluded: Vec<ExcludedEntry>,
}

#[derive(Debug)]
struct Entry {
    target: String,
    test: String,
    serial_group: String,
}

#[derive(Debug)]
struct ExcludedEntry {
    target: String,
    test: String,
    reason: String,
}

fn manifest() -> Manifest {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/live-gate-manifest.toml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let mut manifest = Manifest::default();
    let mut section = None;
    let mut target = None;
    let mut test = None;
    let mut reason = None;
    let mut serial_group = None;

    let finish_entry = |section: &str,
                        target: &mut Option<String>,
                        test: &mut Option<String>,
                        reason: &mut Option<String>,
                        serial_group: &mut Option<String>,
                        manifest: &mut Manifest| {
        let target = target
            .take()
            .unwrap_or_else(|| panic!("{section} entry is missing target"));
        let test = test
            .take()
            .unwrap_or_else(|| panic!("{section} entry is missing test"));
        match section {
            "required" | "account_global" | "soak" => {
                assert!(
                    reason.is_none(),
                    "{section} entry must not contain an exclusion reason"
                );
                let entry = Entry {
                    target,
                    test,
                    serial_group: serial_group
                        .take()
                        .unwrap_or_else(|| panic!("{section} entry is missing serial_group")),
                };
                match section {
                    "required" => manifest.required.push(entry),
                    "account_global" => manifest.account_global.push(entry),
                    "soak" => manifest.soak.push(entry),
                    _ => unreachable!("validated live-gate section"),
                }
            }
            "excluded" => {
                assert!(
                    serial_group.is_none(),
                    "excluded entry must not contain a serial group"
                );
                manifest.excluded.push(ExcludedEntry {
                    target,
                    test,
                    reason: reason
                        .take()
                        .unwrap_or_else(|| panic!("excluded entry is missing reason")),
                });
            }
            _ => panic!("unsupported manifest section {section}"),
        }
    };

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(next_section) = line
            .strip_prefix("[[")
            .and_then(|line| line.strip_suffix("]]"))
        {
            if let Some(section) = section.take() {
                finish_entry(
                    section,
                    &mut target,
                    &mut test,
                    &mut reason,
                    &mut serial_group,
                    &mut manifest,
                );
            }
            assert!(
                matches!(
                    next_section,
                    "required" | "account_global" | "soak" | "excluded"
                ),
                "unsupported manifest section {next_section}"
            );
            section = Some(next_section);
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("invalid manifest line {line}"));
        let key = key.trim();
        let value = value.trim();
        if section.is_none() {
            assert_eq!(key, "version", "unexpected top-level manifest key {key}");
            manifest.version = value
                .parse()
                .unwrap_or_else(|error| panic!("invalid manifest version {value}: {error}"));
            continue;
        }
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or_else(|| panic!("manifest value for {key} must be quoted"))
            .to_owned();
        let destination = match key {
            "target" => &mut target,
            "test" => &mut test,
            "reason" => &mut reason,
            "serial_group" => &mut serial_group,
            _ => panic!("unexpected manifest key {key}"),
        };
        assert!(
            destination.replace(value).is_none(),
            "duplicate manifest key {key}"
        );
    }
    if let Some(section) = section {
        finish_entry(
            section,
            &mut target,
            &mut test,
            &mut reason,
            &mut serial_group,
            &mut manifest,
        );
    }
    manifest
}

fn ignored_tests(target: &str) -> BTreeSet<String> {
    let mut command = Command::new(env!("CARGO"));
    command.args(["test", "--locked", "-p", "anytype"]);
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

fn integration_test_targets() -> BTreeSet<String> {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version=1"])
        .output()
        .unwrap_or_else(|error| panic!("run cargo metadata: {error}"));
    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("parse cargo metadata: {error}"));
    let package = metadata["packages"]
        .as_array()
        .and_then(|packages| {
            packages
                .iter()
                .find(|package| package["name"].as_str() == Some("anytype"))
        })
        .unwrap_or_else(|| panic!("cargo metadata did not contain package anytype"));
    let mut targets = package["targets"]
        .as_array()
        .unwrap_or_else(|| panic!("package anytype has no targets"))
        .iter()
        .filter(|target| {
            target["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("test")))
        })
        .filter_map(|target| target["name"].as_str().map(str::to_owned))
        .collect::<BTreeSet<_>>();
    targets.insert("lib".to_owned());
    targets
}

fn safe_target(target: &str) -> bool {
    let mut chars = target.bytes();
    matches!(chars.next(), Some(byte) if byte.is_ascii_alphanumeric())
        && chars.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn safe_test_path(test: &str) -> bool {
    test.split("::").all(|segment| {
        let mut chars = segment.bytes();
        matches!(chars.next(), Some(byte) if byte.is_ascii_alphabetic() || byte == b'_')
            && chars.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn source_ignore_attribute_inventory(path: &PathBuf) -> (usize, usize) {
    let mut ignored = 0;
    let mut cfg_ignored = 0;
    let entries =
        std::fs::read_dir(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("read source entry: {error}"));
        let path = entry.path();
        if path.is_dir() {
            let (nested_ignored, nested_cfg_ignored) = source_ignore_attribute_inventory(&path);
            ignored += nested_ignored;
            cfg_ignored += nested_cfg_ignored;
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        for (line_number, line) in source.lines().enumerate() {
            let attribute = line.trim_start();
            if attribute.starts_with("#[ignore") {
                assert!(
                    attribute.contains(']'),
                    "unsupported multiline ignore attribute at {}:{}",
                    path.display(),
                    line_number + 1
                );
                ignored += 1;
            }
            if attribute.starts_with("#[cfg_attr") {
                assert!(
                    attribute.contains(']'),
                    "unsupported multiline cfg_attr at {}:{}",
                    path.display(),
                    line_number + 1
                );
                if attribute.contains("ignore") {
                    cfg_ignored += 1;
                }
            }
        }
    }
    (ignored, cfg_ignored)
}

#[test]
fn manifest_is_a_complete_partition_of_ignored_tests() {
    let manifest = manifest();
    assert_eq!(manifest.version, 1, "unexpected manifest version");
    assert_eq!(manifest.required.len(), 22, "required inventory changed");
    assert_eq!(
        manifest.account_global.len(),
        1,
        "account-global inventory changed"
    );
    assert_eq!(manifest.soak.len(), 3, "soak inventory changed");
    assert_eq!(manifest.excluded.len(), 2, "excluded inventory changed");

    let mut expected_by_target = BTreeMap::<String, BTreeSet<String>>::new();
    for entry in manifest
        .required
        .iter()
        .chain(&manifest.account_global)
        .chain(&manifest.soak)
    {
        assert!(safe_target(&entry.target), "unsafe target {}", entry.target);
        assert!(
            safe_test_path(&entry.test),
            "unsafe test path {}",
            entry.test
        );
        assert_eq!(
            entry.serial_group, "disposable_anytype_api",
            "unexpected serial group for {}::{}",
            entry.target, entry.test
        );
        assert!(
            expected_by_target
                .entry(entry.target.clone())
                .or_default()
                .insert(entry.test.clone()),
            "duplicate manifest entry for {}::{}",
            entry.target,
            entry.test
        );
    }
    for entry in &manifest.excluded {
        assert!(safe_target(&entry.target), "unsafe target {}", entry.target);
        assert!(
            safe_test_path(&entry.test),
            "unsafe test path {}",
            entry.test
        );
        assert!(
            !entry.reason.trim().is_empty(),
            "excluded {}::{} has no reason",
            entry.target,
            entry.test
        );
        assert!(
            expected_by_target
                .entry(entry.target.clone())
                .or_default()
                .insert(entry.test.clone()),
            "duplicate manifest entry for {}::{}",
            entry.target,
            entry.test
        );
    }

    let actual_by_target = integration_test_targets()
        .into_iter()
        .map(|target| {
            let ignored = ignored_tests(&target);
            (target, ignored)
        })
        .filter(|(_, ignored)| !ignored.is_empty())
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual_by_target, expected_by_target,
        "ignored-test inventory drifted from the manifest"
    );
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        source_ignore_attribute_inventory(&source_root),
        (28, 0),
        "source ignore-attribute inventory drifted"
    );
}

#[test]
fn manifest_entry_grammar_rejects_traversal_and_options() {
    for target in ["test_body", "lib"] {
        assert!(safe_target(target));
    }
    for target in ["../test_body", "-test", "test\tbody", "test\nbody", ""] {
        assert!(!safe_target(target));
    }
    assert!(safe_test_path("module_name::test_case"));
    for test in ["../test_case", "-test_case", "test\tcase", "test\ncase", ""] {
        assert!(!safe_test_path(test));
    }
}

fn top_level_mapping<'a>(document: &'a str, name: &str) -> BTreeSet<(&'a str, &'a str)> {
    let marker = format!("{name}:");
    let mut in_mapping = false;
    let mut entries = BTreeSet::new();

    for raw_line in document.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let indent = raw_line.len() - raw_line.trim_start_matches(' ').len();
        if !in_mapping {
            if indent == 0 && line == marker {
                in_mapping = true;
            }
            continue;
        }
        if indent == 0 {
            break;
        }
        if indent != 2 || line.starts_with('-') {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .unwrap_or_else(|| panic!("invalid {name} mapping entry {line:?}"));
        assert!(entries.insert((key.trim(), value.trim())));
    }

    assert!(in_mapping, "workflow has no top-level {name} mapping");
    entries
}

fn workflow_crons(workflow: &str) -> BTreeSet<&str> {
    workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- cron:"))
        .map(str::trim)
        .map(|value| value.trim_matches(|character| character == '\'' || character == '"'))
        .collect()
}

fn permission_values(workflow: &str) -> Vec<&str> {
    let lines = workflow.lines().collect::<Vec<_>>();
    let mut values = Vec::new();
    for (index, raw_line) in lines.iter().enumerate() {
        if raw_line.trim() != "permissions:" {
            continue;
        }
        let parent_indent = raw_line.len() - raw_line.trim_start_matches(' ').len();
        for child in &lines[index + 1..] {
            let line = child.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let indent = child.len() - child.trim_start_matches(' ').len();
            if indent <= parent_indent {
                break;
            }
            if indent == parent_indent + 2 {
                let (_, value) = line
                    .split_once(':')
                    .unwrap_or_else(|| panic!("invalid permissions entry {line:?}"));
                values.push(value.trim());
            }
        }
    }
    values
}

fn guarded_schedules(job: &str) -> BTreeSet<&str> {
    job.split("github.event.schedule == '")
        .skip(1)
        .map(|suffix| {
            suffix
                .split_once('\'')
                .map(|(schedule, _)| schedule)
                .expect("schedule guard closes its quoted value")
        })
        .collect()
}

fn assert_actions_are_commit_pinned(workflow: &str) {
    for line in workflow.lines().filter(|line| {
        let line = line.trim();
        line.starts_with("- uses:") || line.starts_with("uses:")
    }) {
        let reference = line
            .split_once('@')
            .map(|(_, reference)| reference.split_whitespace().next().unwrap_or(""))
            .unwrap_or("");
        assert_eq!(reference.len(), 40, "action is not commit-pinned: {line}");
        assert!(
            reference.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "action is not commit-pinned: {line}"
        );
    }
}

#[test]
fn protected_live_workflow_requires_inventory_and_trusted_events() {
    let workflow = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(".github/workflows/anytype-api-live.yml");
    let workflow = std::fs::read_to_string(&workflow)
        .unwrap_or_else(|error| panic!("read {}: {error}", workflow.display()));
    assert_eq!(
        top_level_mapping(&workflow, "on")
            .into_iter()
            .map(|(event, _)| event)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["push", "schedule", "workflow_dispatch"]),
        "credentialed jobs must only be reachable from reviewed events"
    );
    assert_eq!(
        top_level_mapping(&workflow, "permissions"),
        BTreeSet::from([("contents", "read")]),
        "workflow-wide token permissions must remain read-only"
    );
    assert!(
        permission_values(&workflow)
            .into_iter()
            .all(|value| value == "read"),
        "protected jobs must not widen token permissions"
    );
    assert!(workflow.contains("  push:\n    branches:\n      - main\n"));
    let required = workflow
        .split("  headless-required:\n")
        .nth(1)
        .and_then(|block| block.split("  headless-account-global:\n").next())
        .expect("headless-required block");
    let account_global = workflow
        .split("  headless-account-global:\n")
        .nth(1)
        .and_then(|block| block.split("  headless-soak:\n").next())
        .expect("headless-account-global block");
    let soak = workflow
        .split("  headless-soak:\n")
        .nth(1)
        .expect("headless-soak block");
    let required_schedules = guarded_schedules(required);
    let account_global_schedules = guarded_schedules(account_global);
    let soak_schedules = guarded_schedules(soak);
    assert_eq!(required_schedules.len(), 1);
    assert_eq!(required_schedules, account_global_schedules);
    assert_eq!(soak_schedules.len(), 1);
    assert!(required_schedules.is_disjoint(&soak_schedules));
    assert_eq!(
        workflow_crons(&workflow),
        required_schedules.union(&soak_schedules).copied().collect(),
        "every configured schedule must select one reviewed tier, and every guarded schedule must exist"
    );
    assert!(required.contains("github.event_name == 'push'"));
    assert!(required.contains("needs: ignored-test-inventory"));
    assert!(account_global.contains("needs: ignored-test-inventory"));
    assert!(account_global.lines().any(|line| {
        line.trim()
            .strip_prefix("runs-on:")
            .is_some_and(|runner| runner.trim().starts_with("ubuntu-"))
    }));
    assert!(account_global.contains("provision-headless-server.sh ANYTYPE_ACCOUNT_GLOBAL"));
    assert!(account_global.contains("ANYTYPE_ACCOUNT_GLOBAL_TEST_PROCESS=1"));
    assert!(
        account_global
            .contains("run-live-gate.py account_global anytype-api/tests/live-gate-manifest.toml")
    );
    for guard in [
        "systemd-run --user --wait --pipe --collect --same-dir",
        "--service-type=exec",
        "--property=KillMode=control-group",
        "trap cleanup_unit EXIT",
        "systemctl --user stop \"$unit\"",
        "systemctl --user is-active --quiet \"$unit\"",
    ] {
        assert!(
            account_global.contains(guard),
            "account-global job lacks process-tree guard {guard:?}"
        );
    }
    assert!(!account_global.contains("ANYTYPE_HEADLESS_NETWORK_MODE: connected"));
    assert!(!account_global.contains("self-hosted"));
    for block in [required, soak] {
        assert!(block.contains("needs: ignored-test-inventory"));
        // The disposable per-runner server replaced the retired self-hosted
        // anytype-headless runner.
        assert!(block.lines().any(|line| {
            line.trim()
                .strip_prefix("runs-on:")
                .is_some_and(|runner| runner.trim().starts_with("ubuntu-"))
        }));
        assert!(!block.contains("self-hosted"));
        assert!(block.contains("provision-headless-server.sh ANY_MCP_HEADLESS"));
        assert!(block.contains("python3 anytype-api/scripts/run-live-gate.py"));
        assert!(block.contains("test -f \"/proc/self/fd/$reviewed_fd\""));
        assert!(block.contains("stat -Lc '%d|%i|%s' \"/proc/self/fd/$reviewed_fd\""));
        assert!(block.contains("anchor_hash"));
        assert!(block.contains("current_anchor_hash"));
        assert!(!block.contains("stat -Lc '%F"));
        assert!(!block.contains("tee"));
    }
    assert!(!required.contains("ANYTYPE_HEADLESS_NETWORK_MODE: connected"));
    assert!(soak.contains("ANYTYPE_HEADLESS_NETWORK_MODE: connected"));
    assert_eq!(
        workflow.matches("actions/checkout@").count(),
        workflow.matches("persist-credentials: false").count(),
        "every checkout must disable credential persistence"
    );
    assert_actions_are_commit_pinned(&workflow);
}
