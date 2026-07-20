// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

use std::process::Command;

fn unauthenticated_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp"));
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
