// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

use std::{fs::OpenOptions, io::Write, path::PathBuf, process::Command};

fn unauthenticated_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-process-test"));
    command
        .env("RUST_LOG", "warn")
        .env("ANYTYPE_KEYSTORE", "env")
        .env("ANYTYPE_KEYSTORE_SERVICE", "any-mcp-process-test")
        .env_remove("ANYTYPE_KEY_HTTP_TOKEN")
        .env_remove("ANYTYPE_KEY_ACCOUNT_ID")
        .env_remove("ANYTYPE_KEY_ACCOUNT_KEY")
        .env_remove("ANYTYPE_KEY_SESSION_TOKEN");
    command
}

fn invalid_config_file(contents: &str) -> PathBuf {
    // The selected-config reader opens every ancestor with O_NOFOLLOW, so the
    // fixture path must not run through a symlinked temp dir (macOS `/var`).
    let path = std::env::temp_dir()
        .canonicalize()
        .expect("canonical temporary directory")
        .join(format!(
            "any-mcp-invalid-config-{}-{}.toml",
            std::process::id(),
            getrandom::u64().unwrap_or(0)
        ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::{Foundation::GENERIC_WRITE, Storage::FileSystem::WRITE_DAC};

        options.access_mode(GENERIC_WRITE | WRITE_DAC);
    }
    let mut file = options.open(&path).expect("create invalid config");
    #[cfg(windows)]
    anytype::test_util::protect_private_windows_file(&file, false).expect("protect invalid config");
    file.write_all(contents.as_bytes())
        .expect("write invalid config");
    file.sync_all().expect("sync invalid config");
    path
}

#[test]
fn startup_auth_failure_is_nonzero_stderr_only_and_redacted() {
    let output = unauthenticated_command()
        .output()
        .expect("run any-mcp test binary");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout is reserved for MCP frames"
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(stderr.contains("HTTP credentials are missing"));
    assert!(!stderr.contains("ANYTYPE_KEY_HTTP_TOKEN="));
}

#[test]
fn invalid_operational_setting_does_not_echo_its_value() {
    let secret_like_value = "secret-value-that-is-not-a-number";
    let output = unauthenticated_command()
        .env("ANY_MCP_MAX_CONCURRENCY", secret_like_value)
        .output()
        .expect("run any-mcp test binary");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout is reserved for MCP frames"
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(stderr.contains("ANY_MCP_MAX_CONCURRENCY"));
    assert!(!stderr.contains(secret_like_value));
}

#[test]
fn invalid_read_only_setting_fails_before_auth_without_echoing_its_value() {
    let secret_like_value = "secret-value-that-is-not-zero-or-one";
    let output = unauthenticated_command()
        .env("ANY_MCP_READ_ONLY", secret_like_value)
        .output()
        .expect("run any-mcp test binary");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout is reserved for MCP frames"
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(stderr.contains("ANY_MCP_READ_ONLY"));
    assert!(!stderr.contains(secret_like_value));
    assert!(!stderr.contains("HTTP credentials are missing"));
}

#[test]
fn invalid_protocol_mode_fails_before_auth_without_echoing_its_value() {
    let secret_like_value = "secret-preview-selector";
    let output = unauthenticated_command()
        .env("ANY_MCP_PROTOCOL", secret_like_value)
        .output()
        .expect("run any-mcp test binary");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout is reserved for MCP frames"
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(stderr.contains("ANY_MCP_PROTOCOL"));
    assert!(!stderr.contains(secret_like_value));
    assert!(!stderr.contains("HTTP credentials are missing"));
}

#[test]
fn invalid_catalog_profile_fails_before_auth_without_echoing_its_value() {
    let secret_like_value = "secret-catalog-profile";
    let output = unauthenticated_command()
        .env("ANY_MCP_PROFILE", secret_like_value)
        .output()
        .expect("run any-mcp test binary");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout is reserved for MCP frames"
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(stderr.contains("ANY_MCP_PROFILE"));
    assert!(!stderr.contains(secret_like_value));
    assert!(!stderr.contains("HTTP credentials are missing"));
}

#[test]
fn invalid_toml_reports_redacted_location_path_and_reason_before_auth() {
    let secret_like_value = "operator-secret-config-value";
    let path = invalid_config_file(&format!(
        "schema_version = 1\n[spaces]\nread_only = \"{secret_like_value}\"\n"
    ));
    let output = unauthenticated_command()
        .arg("--config")
        .arg(&path)
        .output()
        .expect("run any-mcp test binary");
    std::fs::remove_file(path).expect("remove invalid config");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout is reserved for MCP frames"
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(stderr.contains("invalid any-mcp TOML configuration"));
    assert!(stderr.contains("line 3, column 13"));
    assert!(stderr.contains("spaces.read_only"));
    assert!(stderr.contains("value has the wrong type"));
    assert!(!stderr.contains(secret_like_value));
    assert!(!stderr.contains("HTTP credentials are missing"));
}

#[test]
fn optional_selector_fails_before_auth_with_fixed_secret_safe_category() {
    let secret_like_value = "secret_like_optional_selector";
    let output = unauthenticated_command()
        .env("ANY_MCP_TOOLSETS", secret_like_value)
        .output()
        .expect("run any-mcp test binary");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout is reserved for MCP frames"
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(stderr.contains("invalid optional toolset selector"));
    assert!(!stderr.contains(secret_like_value));
    assert!(!stderr.contains("HTTP credentials are missing"));
}

#[test]
fn linked_schema_registry_reaches_auth_without_echoing_selector() {
    let output = unauthenticated_command()
        .env("ANY_MCP_TOOLSETS", "schema")
        .output()
        .expect("run any-mcp test binary");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout is reserved for MCP frames"
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(stderr.contains("HTTP credentials are missing"));
    assert!(!stderr.contains("schema"));
    assert!(!stderr.contains("unsupported optional toolset selector"));
}

#[test]
fn linked_body_blocks_registry_reaches_auth_without_echoing_selector() {
    let output = unauthenticated_command()
        .env("ANY_MCP_TOOLSETS", "body-blocks")
        .output()
        .expect("run any-mcp test binary");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout is reserved for MCP frames"
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(stderr.contains("HTTP credentials are missing"));
    assert!(!stderr.contains("body-blocks"));
    assert!(!stderr.contains("unsupported optional toolset selector"));
}

#[test]
fn linked_views_write_registry_reaches_auth_without_echoing_selector() {
    let output = unauthenticated_command()
        .env("ANY_MCP_TOOLSETS", "views-write")
        .output()
        .expect("run any-mcp test binary");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout is reserved for MCP frames"
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(stderr.contains("HTTP credentials are missing"));
    assert!(!stderr.contains("views-write"));
    assert!(!stderr.contains("unsupported optional toolset selector"));
}
