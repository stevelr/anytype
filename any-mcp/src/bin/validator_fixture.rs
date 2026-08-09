// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-License-Identifier: Apache-2.0

//! Tiny scripted validator executable for the FLOOD acceptance cases.
//!
//! Production validator activation hashes the pinned executable under a
//! 128 MiB ceiling and requires a non-writable native binary, so the
//! acceptance harness pins this small dedicated fixture instead of the full
//! process-test binary. The scripted behaviors mirror the FLOOD matrix rows:
//! `FLOOD-01` floods stdout past the configured cap, `FLOOD-02` never exits,
//! and `FLOOD-03` forks a descendant and floods stderr.

use std::io::{Read, Write};

fn main() -> std::process::ExitCode {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let expected = ["--brief", "--mime-type", "--", "-"];
    if arguments.len() != expected.len()
        || !arguments
            .iter()
            .zip(expected)
            .all(|(observed, expected)| observed == expected)
    {
        return std::process::ExitCode::FAILURE;
    }
    let mut input = Vec::new();
    if std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut input)
        .is_err()
    {
        return std::process::ExitCode::FAILURE;
    }
    if input.starts_with(b"FLOOD-01") {
        let bytes = vec![b'x'; 8 * 1024];
        return if std::io::stdout().write_all(&bytes).is_ok() {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        };
    }
    if input.starts_with(b"FLOOD-02") {
        loop {
            std::thread::park_timeout(std::time::Duration::from_secs(60));
        }
    }
    if input.starts_with(b"FLOOD-03") {
        #[cfg(unix)]
        // SAFETY: the child performs no allocation or lock-taking before it
        // blocks, and the production process boundary owns the complete group.
        unsafe {
            if libc::fork() == 0 {
                loop {
                    libc::pause();
                }
            }
        }
        let bytes = vec![b'e'; 8 * 1024];
        return if std::io::stderr().write_all(&bytes).is_ok() {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        };
    }
    std::process::ExitCode::FAILURE
}
