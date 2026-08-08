// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-License-Identifier: Apache-2.0

//! Private process wrapper used by any-mcp's spawned integration tests.

#[cfg(feature = "acceptance-harness")]
fn run_validator_fixture(arguments: &[std::ffi::OsString]) -> Option<std::process::ExitCode> {
    use std::io::{Read, Write};

    let expected = ["--brief", "--mime-type", "--", "-"];
    if arguments.len() != expected.len()
        || !arguments
            .iter()
            .zip(expected)
            .all(|(observed, expected)| observed == expected)
    {
        return None;
    }
    let mut input = Vec::new();
    if std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut input)
        .is_err()
    {
        return Some(std::process::ExitCode::FAILURE);
    }
    if input.starts_with(b"FLOOD-01") {
        let bytes = vec![b'x'; 8 * 1024];
        let status = std::io::stdout().write_all(&bytes);
        return Some(if status.is_ok() {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        });
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
        let status = std::io::stderr().write_all(&bytes);
        return Some(if status.is_ok() {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        });
    }
    Some(std::process::ExitCode::FAILURE)
}

fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    #[cfg(feature = "acceptance-harness")]
    if let Some(status) = run_validator_fixture(&arguments) {
        if status != std::process::ExitCode::SUCCESS {
            std::process::exit(1);
        }
        return;
    }
    #[cfg(feature = "acceptance-harness")]
    let status = any_mcp::run_acceptance_process(arguments);
    #[cfg(not(feature = "acceptance-harness"))]
    let status = any_mcp::run_process(arguments);
    if status != std::process::ExitCode::SUCCESS {
        std::process::exit(1);
    }
}
