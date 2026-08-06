/*
 * anyr - list, search, and manipulate anytype objects
 * github.com/stevelr/anytype
 *
 * SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */

//! Stream-routing contract for `anyr` diagnostics.
//!
//! Machine-readable output (`--json`, `--yaml`, backup/restore result documents)
//! is only parseable if tracing diagnostics never reach stdout. These tests run
//! the real binary with both streams redirected to pipes, so they also cover the
//! ANSI decision: styling follows stderr's terminal-ness, and a piped stderr must
//! stay free of escape sequences.
//!
//! `auth find-grpc` is used as the carrier because it needs no credentials and
//! completes whether or not a local Anytype server is running; the assertions
//! only concern which stream carries what.

use std::process::{Command, Output};

/// Escape byte that introduces every ANSI control sequence.
const ESC: u8 = 0x1b;

/// Runs `anyr` with piped stdout/stderr and a cleared `RUST_LOG`.
fn run_anyr(args: &[&str], rust_log: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anyr"));
    command.args(args).env_remove("RUST_LOG");
    if let Some(value) = rust_log {
        command.env("RUST_LOG", value);
    }
    command
        .output()
        .expect("failed to run the anyr binary under test")
}

#[test]
fn conflict_warning_goes_to_stderr_not_stdout() {
    let output = run_anyr(&["--json", "--table", "auth", "find-grpc"], None);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("WARN"),
        "tracing diagnostics must not appear on stdout, got: {stdout:?}"
    );
    assert!(
        !output.stdout.contains(&ESC),
        "stdout must not carry ANSI escape sequences, got: {stdout:?}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--json conflicts with --table"),
        "the conflict warning must reach stderr, got: {stderr:?}"
    );
}

#[test]
fn piped_stderr_diagnostics_carry_no_ansi_styling() {
    let output = run_anyr(&["--json", "--table", "auth", "find-grpc"], None);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.stderr.contains(&ESC),
        "a non-terminal stderr must not be ANSI styled, got: {stderr:?}"
    );
}

#[test]
fn verbose_and_rust_log_diagnostics_keep_stdout_clean() {
    for (args, rust_log) in [
        (vec!["-v", "--json", "auth", "find-grpc"], None::<&str>),
        (vec!["--json", "auth", "find-grpc"], Some("debug")),
    ] {
        let output = run_anyr(&args, rust_log);
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            !output.stdout.contains(&ESC),
            "stdout must stay unstyled for {args:?} (RUST_LOG={rust_log:?}), got: {stdout:?}"
        );
        for level in ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"] {
            assert!(
                !stdout.contains(level),
                "{level} diagnostics leaked to stdout for {args:?} (RUST_LOG={rust_log:?}), got: {stdout:?}"
            );
        }
        // Whatever stdout holds must still be a single machine-readable document.
        let trimmed = stdout.trim();
        if !trimmed.is_empty() {
            serde_json::from_str::<serde_json::Value>(trimmed).unwrap_or_else(|err| {
                panic!("stdout is not valid JSON for {args:?}: {err} in {trimmed:?}")
            });
        }
    }
}
