use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

/// Runs `anyr` once and rejects terminal controls before returning the attempt.
pub(crate) fn run_once_checked(args: &[&str]) -> Result<Output> {
    let mut command = Command::new(anyr_binary()?);
    command.args(args);
    let _keystore = super::keystore::configure_test_keystore(&mut command)?;
    let output = command.output().context("failed to execute anyr command")?;
    checked_attempt(output)
}

/// Runs `anyr` through a suite-provided retry policy and separates its output.
pub(crate) fn run_anyr_parts<F>(args: &[&str], run: F) -> Result<(String, String)>
where
    F: FnOnce(&[&str]) -> Result<Output>,
{
    let output = checked_attempt(run(args)?)?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        bail!("anyr command failed ({})", output_metadata(&output));
    }

    Ok((stdout.trim().to_string(), stderr))
}

/// Parses the structured (compact JSON) result document written by `anyr`.
pub(crate) fn parse_json_output(output: &str) -> Result<Value> {
    serde_json::from_str(output.trim()).with_context(|| {
        format!(
            "expected structured anyr output ({})",
            stream_metadata(output.as_bytes())
        )
    })
}

/// Returns the archive path from structured `anyr backup create` output.
pub(crate) fn parse_archive_path(output: &str) -> Result<PathBuf> {
    let parsed = parse_json_output(output)?;
    let archive = parsed
        .get("archive")
        .and_then(Value::as_str)
        .filter(|archive| !archive.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "structured anyr output missing archive path ({})",
                stream_metadata(output.as_bytes())
            )
        })?;
    let categories = control_categories(archive.as_bytes());
    if !categories.is_path_clean() {
        bail!(
            "structured anyr archive path contains control characters (decoded_archive_controls={categories}; output_metadata={})",
            stream_metadata(output.as_bytes())
        );
    }
    Ok(PathBuf::from(archive))
}

/// Asserts that non-TTY output contains no terminal control sequences.
pub(crate) fn assert_non_tty_output_clean(output: &str) {
    let metadata = stream_metadata(output.as_bytes());
    assert!(
        control_categories(output.as_bytes()).is_non_tty_clean(),
        "unexpected terminal control in non-TTY output ({metadata})"
    );
}

/// Returns bounded metadata suitable for diagnostics without exposing content.
pub(crate) fn output_metadata(output: &Output) -> String {
    format!(
        "status={}; stdout={}; stderr={}",
        output.status,
        stream_metadata(&output.stdout),
        stream_metadata(&output.stderr)
    )
}

/// Returns the byte length and terminal-control categories for one stream.
pub(crate) fn stream_metadata(stream: &[u8]) -> String {
    let categories = control_categories(stream);
    format!("bytes={}; controls={categories}", stream.len())
}

fn checked_attempt(output: Output) -> Result<Output> {
    if !control_categories(&output.stderr).is_non_tty_clean() {
        bail!(
            "anyr stderr contains terminal control ({})",
            output_metadata(&output)
        );
    }
    Ok(output)
}

#[derive(Debug, Default)]
struct ControlCategories {
    invalid_utf8: bool,
    tab: bool,
    newline: bool,
    c0: bool,
    escape: bool,
    carriage_return: bool,
    delete: bool,
    c1: bool,
}

impl ControlCategories {
    fn is_non_tty_clean(&self) -> bool {
        !self.invalid_utf8
            && !self.c0
            && !self.escape
            && !self.carriage_return
            && !self.delete
            && !self.c1
    }

    fn is_path_clean(&self) -> bool {
        self.is_non_tty_clean() && !self.tab && !self.newline
    }
}

impl std::fmt::Display for ControlCategories {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut categories = Vec::with_capacity(8);
        if self.invalid_utf8 {
            categories.push("invalid-utf8");
        }
        if self.tab {
            categories.push("tab");
        }
        if self.newline {
            categories.push("newline");
        }
        if self.c0 {
            categories.push("c0");
        }
        if self.escape {
            categories.push("escape");
        }
        if self.carriage_return {
            categories.push("carriage-return");
        }
        if self.delete {
            categories.push("delete");
        }
        if self.c1 {
            categories.push("c1");
        }
        if categories.is_empty() {
            formatter.write_str("none")
        } else {
            formatter.write_str(&categories.join(","))
        }
    }
}

fn control_categories(stream: &[u8]) -> ControlCategories {
    let Ok(text) = std::str::from_utf8(stream) else {
        return ControlCategories {
            invalid_utf8: true,
            ..ControlCategories::default()
        };
    };

    let mut categories = ControlCategories::default();
    for character in text.chars() {
        match character {
            '\t' => categories.tab = true,
            '\n' => categories.newline = true,
            '\u{1b}' => categories.escape = true,
            '\r' => categories.carriage_return = true,
            '\u{0}'..='\u{1f}' => categories.c0 = true,
            '\u{7f}' => categories.delete = true,
            '\u{80}'..='\u{9f}' => categories.c1 = true,
            _ => {}
        }
    }
    categories
}

/// Resolves the `anyr` executable under test.
///
/// `ANYR_BIN` wins when set; otherwise the binary built alongside this test
/// harness is required. The harness never falls back to `PATH`.
fn anyr_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("ANYR_BIN") {
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(anyhow!("ANYR_BIN is not a file: {}", path.display()));
        }
        return Ok(path);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(target_dir) = exe.parent().and_then(Path::parent)
    {
        let candidate = target_dir.join(format!("anyr{}", std::env::consts::EXE_SUFFIX));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "anyr test binary not found; run `cargo build -p anyr` first or set ANYR_BIN"
    ))
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, process::ExitStatus};

    use super::*;

    #[test]
    fn checked_attempt_accepts_plain_stderr() -> Result<()> {
        let output = synthetic_output(0, b"structured output", b"progress\tcomplete\n");
        let checked = checked_attempt(output)?;
        assert!(checked.status.success());
        Ok(())
    }

    #[test]
    fn terminal_failure_reports_only_metadata() {
        let output = synthetic_output(1, b"stdout-secret-token", b"stderr-secret-token\x1b[2K");
        let error = expect_error(checked_attempt(output), "escape sequence was accepted");
        let message = error.to_string();
        assert!(message.contains("status="));
        assert!(message.contains("controls=escape"));
        assert!(!message.contains("secret-token"));
    }

    #[test]
    fn retry_checks_each_attempt_before_classification() {
        let attempts = Cell::new(0);
        let result = crate::run_with_lock_retry(|| {
            let attempt = attempts.get() + 1;
            attempts.set(attempt);
            let output = if attempt == 1 {
                synthetic_output(1, b"", b"Failed locking file\n")
            } else {
                synthetic_output(0, b"structured output", "terminal\u{9b}control".as_bytes())
            };
            checked_attempt(output)
        });

        let error = expect_error(result, "the retried attempt was not validated");
        assert_eq!(attempts.get(), 2);
        assert!(error.to_string().contains("controls=c1"));
    }

    #[test]
    fn nonzero_error_reports_only_metadata() {
        let output = synthetic_output(1, b"stdout-secret-token", b"stderr-secret-token\n");
        let error = expect_error(
            run_anyr_parts(&[], |_| Ok(output)),
            "nonzero status was accepted",
        );
        let message = error.to_string();
        assert!(message.contains("status="));
        assert!(message.contains("stdout=bytes="));
        assert!(message.contains("stderr=bytes="));
        assert!(!message.contains("secret-token"));
    }

    #[test]
    fn invalid_json_error_reports_only_metadata() {
        let output = "not-json-secret-token";
        let error = expect_error(parse_json_output(output), "invalid JSON was accepted");
        let message = format!("{error:#}");
        assert!(message.contains("bytes="));
        assert!(message.contains("controls=none"));
        assert!(!message.contains("secret-token"));
    }

    #[test]
    fn malformed_archive_json_does_not_echo_payload() {
        let output = "{\"secret\":\"archive-secret-token\"\x1b";
        let error = expect_error(
            parse_archive_path(output),
            "malformed archive JSON was accepted",
        );
        let message = format!("{error:#}");
        assert!(message.contains("bytes="));
        assert!(message.contains("controls=escape"));
        assert!(!message.contains("archive-secret-token"));
    }

    #[test]
    fn missing_archive_field_does_not_echo_payload() {
        let output = r#"{"secret":"archive-secret-token"}"#;
        let error = expect_error(
            parse_archive_path(output),
            "missing archive field was accepted",
        );
        let message = format!("{error:#}");
        assert!(message.contains("missing archive path"));
        assert!(message.contains("controls=none"));
        assert!(!message.contains("archive-secret-token"));
    }

    #[test]
    fn escaped_archive_escape_is_rejected_without_payload() {
        let output = r#"{"archive":"archive-secret-token\u001brest"}"#;
        let error = expect_error(
            parse_archive_path(output),
            "escaped archive control was accepted",
        );
        let message = format!("{error:#}");
        assert!(message.contains("decoded_archive_controls=escape"));
        assert!(!message.contains("archive-secret-token"));
    }

    #[test]
    fn escaped_archive_newline_is_rejected_without_payload() {
        let output = r#"{"archive":"archive-secret-token\nrest"}"#;
        let error = expect_error(
            parse_archive_path(output),
            "escaped archive newline was accepted",
        );
        let message = format!("{error:#}");
        assert!(message.contains("decoded_archive_controls=newline"));
        assert!(!message.contains("archive-secret-token"));
    }

    #[test]
    fn detects_relevant_c0_and_c1_controls() {
        assert!(control_categories(b"ordinary\ttext\n").is_non_tty_clean());
        for control in [
            "\u{0}", "\u{7}", "\u{1b}", "\r", "\u{7f}", "\u{80}", "\u{9f}",
        ] {
            assert!(
                !control_categories(control.as_bytes()).is_non_tty_clean(),
                "control {control:?} was accepted"
            );
        }
        assert!(!control_categories(&[0xff]).is_non_tty_clean());
    }

    #[test]
    fn assertion_failure_does_not_echo_output() {
        let result =
            std::panic::catch_unwind(|| assert_non_tty_output_clean("stderr-secret-token\x1b[2K"));
        let panic = result.expect_err("escape sequence must trigger assertion");
        let message = panic_message(&panic);
        assert!(message.contains("controls=escape"));
        assert!(!message.contains("secret-token"));
    }

    fn synthetic_output(status: i32, stdout: &[u8], stderr: &[u8]) -> Output {
        Output {
            status: synthetic_exit_status(status),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    #[cfg(unix)]
    fn synthetic_exit_status(status: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;

        ExitStatus::from_raw(status << 8)
    }

    #[cfg(windows)]
    fn synthetic_exit_status(status: i32) -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;

        ExitStatus::from_raw(status as u32)
    }

    fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
        if let Some(message) = panic.downcast_ref::<String>() {
            message.clone()
        } else if let Some(message) = panic.downcast_ref::<&str>() {
            (*message).to_string()
        } else {
            "non-string panic".to_string()
        }
    }

    fn expect_error<T>(result: Result<T>, message: &str) -> anyhow::Error {
        match result {
            Ok(_) => panic!("{message}"),
            Err(error) => error,
        }
    }
}
