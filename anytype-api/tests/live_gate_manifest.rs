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

#[test]
fn manifest_is_a_complete_partition_of_ignored_tests() {
    let manifest = manifest();
    assert_eq!(manifest.version, 1, "unexpected manifest version");
    assert_eq!(manifest.required.len(), 15, "required inventory changed");
    assert_eq!(manifest.soak.len(), 3, "soak inventory changed");
    assert_eq!(manifest.excluded.len(), 11, "excluded inventory changed");

    let mut expected_by_target = BTreeMap::<String, BTreeSet<String>>::new();
    for entry in manifest.required.iter().chain(&manifest.soak) {
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

    for (target, expected) in expected_by_target {
        assert_eq!(
            ignored_tests(&target),
            expected,
            "ignored-test inventory drifted for target {target}"
        );
    }
}
