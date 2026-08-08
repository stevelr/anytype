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
            "required" | "soak" => {
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
                if section == "required" {
                    manifest.required.push(entry);
                } else {
                    manifest.soak.push(entry);
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
                matches!(next_section, "required" | "soak" | "excluded"),
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
    assert_eq!(manifest.required.len(), 17, "required inventory changed");
    assert_eq!(manifest.soak.len(), 3, "soak inventory changed");
    assert_eq!(manifest.excluded.len(), 2, "excluded inventory changed");

    let mut expected_by_target = BTreeMap::<String, BTreeSet<String>>::new();
    for entry in manifest.required.iter().chain(&manifest.soak) {
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
        (22, 0),
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

#[test]
fn protected_live_workflow_requires_inventory_and_trusted_events() {
    let workflow = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(".github/workflows/anytype-api-live.yml");
    let workflow = std::fs::read_to_string(&workflow)
        .unwrap_or_else(|error| panic!("read {}: {error}", workflow.display()));
    let required = workflow
        .split("  headless-required:\n")
        .nth(1)
        .and_then(|block| block.split("  headless-soak:\n").next())
        .expect("headless-required block");
    let soak = workflow
        .split("  headless-soak:\n")
        .nth(1)
        .expect("headless-soak block");
    assert!(required.contains("if: github.event_name == 'workflow_dispatch' || (github.event_name == 'push' && github.ref == 'refs/heads/main')"));
    assert!(required.contains("needs: ignored-test-inventory"));
    assert!(soak.contains("if: github.event_name == 'schedule'"));
    for block in [required, soak] {
        assert!(block.contains("needs: ignored-test-inventory"));
        assert!(block.contains("runs-on: [ self-hosted, linux, anytype-headless ]"));
        assert!(block.contains("actions/checkout@11d5960a326750d5838078e36cf38b85af677262"));
        assert!(block.contains("python3 anytype-api/scripts/run-live-gate.py"));
        assert!(block.contains("test -f \"/proc/self/fd/$reviewed_fd\""));
        assert!(block.contains("stat -Lc '%d|%i|%s' \"/proc/self/fd/$reviewed_fd\""));
        assert!(block.contains("anchor_hash"));
        assert!(block.contains("current_anchor_hash"));
        assert!(!block.contains("stat -Lc '%F"));
        assert!(!block.contains("tee"));
    }
}
