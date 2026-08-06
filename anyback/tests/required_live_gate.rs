use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::Value;
use zeroize::{Zeroize, Zeroizing};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(90);
const RESTORE_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_RESULT_BYTES: u64 = 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
const MAX_SERVER_DIAGNOSTIC_BYTES: usize = 64 * 1024;
const REDACTION_ENV_NAMES: [&str; 8] = [
    "ANYTYPE_KEY_HTTP_TOKEN",
    "ANYTYPE_KEY_ACCOUNT_KEY",
    "ANYTYPE_KEY_SESSION_TOKEN",
    "ANYTYPE_API_KEY",
    "ANYTYPE_HTTP_API_KEY",
    "ANYTYPE_GRPC_KEY",
    "ANYTYPE_KEY_FILE",
    "ANYTYPE_KEYSTORE",
];

#[derive(Debug)]
struct CommandOutput {
    status: ExitStatus,
    stdout: Zeroizing<String>,
    stderr: Zeroizing<String>,
}

struct AnyrRunner {
    binary: PathBuf,
    server_log: PathBuf,
    redactions: Vec<Zeroizing<String>>,
}

impl AnyrRunner {
    fn required() -> Result<Self> {
        let binary = required_absolute_file("ANYR_BIN")?;
        let server_log = required_absolute_file("ANYBACK_HEADLESS_REDACTED_LOG_FILE")?;
        let redactions = REDACTION_ENV_NAMES
            .into_iter()
            .filter_map(|name| std::env::var(name).ok())
            .filter(|value| !value.is_empty())
            .map(Zeroizing::new)
            .collect();
        Ok(Self {
            binary,
            server_log,
            redactions,
        })
    }

    fn run(&self, args: &[&str], stdin: Option<&str>, timeout: Duration) -> Result<CommandOutput> {
        let capture = tempfile::tempdir().context("failed to create command capture directory")?;
        let stdout_path = capture.path().join("stdout");
        let stderr_path = capture.path().join("stderr");
        let stdout_file = private_capture_file(&stdout_path)?;
        let stderr_file = private_capture_file(&stderr_path)?;

        let mut command = Command::new(&self.binary);
        command
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));
        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to start installed anyr for {}",
                display_command(args)
            )
        })?;
        if let Some(input) = stdin {
            let mut child_stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("failed to open anyr stdin"))?;
            child_stdin
                .write_all(input.as_bytes())
                .context("failed to write anyr stdin")?;
        }

        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.try_wait().context("failed to poll anyr command")? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(self.command_error(
                    args,
                    &stdout_path,
                    &stderr_path,
                    format!("timed out after {} seconds", timeout.as_secs()),
                ));
            }
            thread::sleep(POLL_INTERVAL);
        };

        let stdout = read_result(&stdout_path)?;
        let stderr = read_result(&stderr_path)?;
        let output = CommandOutput {
            status,
            stdout,
            stderr,
        };
        if !output.status.success() {
            return Err(self.output_error(args, &output));
        }
        Ok(output)
    }

    fn run_json(&self, args: &[&str], timeout: Duration) -> Result<Value> {
        let output = self.run(args, None, timeout)?;
        serde_json::from_str(output.stdout.trim()).with_context(|| {
            format!(
                "installed anyr returned invalid JSON for {}: {}",
                display_command(args),
                self.bounded_redacted(&output.stdout, MAX_DIAGNOSTIC_BYTES)
            )
        })
    }

    fn output_error(&self, args: &[&str], output: &CommandOutput) -> anyhow::Error {
        anyhow!(
            "installed anyr command {} failed with {}; stdout={} stderr={} server_tail={}",
            display_command(args),
            output.status,
            self.bounded_redacted(&output.stdout, MAX_DIAGNOSTIC_BYTES),
            self.bounded_redacted(&output.stderr, MAX_DIAGNOSTIC_BYTES),
            self.server_tail()
        )
    }

    fn command_error(
        &self,
        args: &[&str],
        stdout_path: &Path,
        stderr_path: &Path,
        reason: String,
    ) -> anyhow::Error {
        let stdout = read_tail(stdout_path, MAX_DIAGNOSTIC_BYTES).unwrap_or_default();
        let stderr = read_tail(stderr_path, MAX_DIAGNOSTIC_BYTES).unwrap_or_default();
        anyhow!(
            "installed anyr command {} {}; stdout={} stderr={} server_tail={}",
            display_command(args),
            reason,
            self.bounded_redacted(&stdout, MAX_DIAGNOSTIC_BYTES),
            self.bounded_redacted(&stderr, MAX_DIAGNOSTIC_BYTES),
            self.server_tail()
        )
    }

    fn bounded_redacted(&self, text: &str, limit: usize) -> String {
        bounded_redacted(text, limit, &self.redactions)
    }

    fn server_tail(&self) -> String {
        match read_tail(&self.server_log, MAX_SERVER_DIAGNOSTIC_BYTES) {
            Ok(tail) => self.bounded_redacted(&tail, MAX_SERVER_DIAGNOSTIC_BYTES),
            Err(error) => format!("<server log unavailable: {error:#}>"),
        }
    }
}

struct OwnedSpace<'a> {
    runner: &'a AnyrRunner,
    name: String,
    id: Option<String>,
    cleaned: bool,
}

impl<'a> OwnedSpace<'a> {
    fn create(runner: &'a AnyrRunner, name: String) -> Result<Self> {
        let mut owned = Self {
            runner,
            name,
            id: None,
            cleaned: false,
        };
        let output = owned.runner.run(
            &["space", "create", owned.name.as_str()],
            None,
            COMMAND_TIMEOUT,
        )?;
        let value: Value = serde_json::from_str(output.stdout.trim())
            .context("space create returned invalid JSON; cleanup will use the registered name")?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow!("space create output missing a non-empty id"))?;
        owned.id = Some(id.to_string());
        Ok(owned)
    }

    fn identifier(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.name)
    }

    fn cleanup(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        let confirmation = format!("n\ndelete:{}\n", self.name);
        self.runner.run(
            &["space", "delete", self.identifier()],
            Some(&confirmation),
            RESTORE_TIMEOUT,
        )?;
        self.prove_absent()?;
        self.cleaned = true;
        Ok(())
    }

    fn prove_absent(&self) -> Result<()> {
        let id = self.id.as_deref();
        for _ in 0..30 {
            let value = self
                .runner
                .run_json(&["space", "list", "--all"], COMMAND_TIMEOUT)?;
            let items = value
                .get("items")
                .and_then(Value::as_array)
                .or_else(|| value.as_array())
                .ok_or_else(|| anyhow!("space list output missing items"))?;
            let present = items.iter().any(|item| {
                item.get("name").and_then(Value::as_str) == Some(self.name.as_str())
                    || id.is_some_and(|expected| {
                        item.get("id").and_then(Value::as_str) == Some(expected)
                    })
            });
            if !present {
                return Ok(());
            }
            thread::sleep(Duration::from_secs(1));
        }
        bail!("cleanup did not remove disposable space {}", self.name)
    }
}

impl Drop for OwnedSpace<'_> {
    fn drop(&mut self) {
        if !self.cleaned
            && let Err(error) = self.cleanup()
        {
            eprintln!(
                "failed to clean up disposable space {} during unwinding: {error:#}",
                self.name
            );
        }
    }
}

#[test]
#[ignore = "requires protected disposable Anytype server admission"]
fn required_installed_anyr_backup_create_restore() -> Result<()> {
    require_disposable_admission()?;
    let runner = AnyrRunner::required()?;
    verify_authenticated_pings(&runner)?;

    let prefix = std::env::var("ANYTYPE_TEST_SPACE_PREFIX")?;
    let suffix = anytype::test_util::unique_suffix();
    let source_name = format!("{prefix}-backup-smoke-src-{suffix}");
    let destination_name = format!("{prefix}-backup-smoke-dst-{suffix}");
    let object_name = format!("required backup smoke {suffix}");
    let object_body = format!("required backup smoke body {suffix}");

    let mut source = OwnedSpace::create(&runner, source_name)?;
    let mut destination = match OwnedSpace::create(&runner, destination_name) {
        Ok(space) => space,
        Err(error) => return finish_with_cleanup(Err(error), &mut [&mut source]),
    };

    let result =
        exercise_backup_restore(&runner, &source, &destination, &object_name, &object_body);
    finish_with_cleanup(result, &mut [&mut destination, &mut source])?;
    write_execution_marker()
}

fn exercise_backup_restore(
    runner: &AnyrRunner,
    source: &OwnedSpace<'_>,
    destination: &OwnedSpace<'_>,
    object_name: &str,
    object_body: &str,
) -> Result<()> {
    let object = runner.run_json(
        &[
            "object",
            "create",
            source.identifier(),
            "page",
            "--name",
            object_name,
            "--body",
            object_body,
        ],
        COMMAND_TIMEOUT,
    )?;
    let source_object_id = object
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("object create output missing a non-empty id"))?;

    let archive_dir = tempfile::tempdir().context("failed to create archive directory")?;
    let ids_path = archive_dir.path().join("objects.txt");
    fs::write(&ids_path, format!("{source_object_id}\n"))?;
    let ids_arg = ids_path
        .to_str()
        .ok_or_else(|| anyhow!("object id path is not UTF-8"))?;
    let archive_dir_arg = archive_dir
        .path()
        .to_str()
        .ok_or_else(|| anyhow!("archive directory path is not UTF-8"))?;
    let backup = runner.run_json(
        &[
            "backup",
            "create",
            "--space",
            source.identifier(),
            "--objects",
            ids_arg,
            "--dir",
            archive_dir_arg,
        ],
        RESTORE_TIMEOUT,
    )?;
    let archive = backup
        .get("archive")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("backup create output missing archive path"))?;
    let archive_metadata = fs::metadata(&archive)
        .with_context(|| format!("backup archive does not exist: {}", archive.display()))?;
    ensure!(archive_metadata.len() > 0, "backup archive is empty");

    let archive_arg = archive
        .to_str()
        .ok_or_else(|| anyhow!("archive path is not UTF-8"))?;
    runner.run_json(
        &[
            "backup",
            "restore",
            archive_arg,
            "--space",
            destination.identifier(),
        ],
        RESTORE_TIMEOUT,
    )?;
    verify_restored_content(runner, destination.identifier(), object_name, object_body)?;
    let _captured_server_diagnostics = runner.server_tail();
    Ok(())
}

fn finish_with_cleanup(primary: Result<()>, spaces: &mut [&mut OwnedSpace<'_>]) -> Result<()> {
    let cleanup_errors = spaces
        .iter_mut()
        .filter_map(|space| {
            space
                .cleanup()
                .err()
                .map(|error| format!("{}: {error:#}", space.name))
        })
        .collect::<Vec<_>>();

    match (primary, cleanup_errors.is_empty()) {
        (Ok(()), true) => Ok(()),
        (Err(error), true) => Err(error),
        (Ok(()), false) => bail!(
            "disposable space cleanup failed: {}",
            cleanup_errors.join("; ")
        ),
        (Err(error), false) => bail!(
            "live backup/restore failed: {error:#}; disposable space cleanup also failed: {}",
            cleanup_errors.join("; ")
        ),
    }
}

fn verify_authenticated_pings(runner: &AnyrRunner) -> Result<()> {
    let status = runner.run_json(&["auth", "status"], COMMAND_TIMEOUT)?;
    let ping = status
        .get("ping")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("auth status output missing ping object"))?;
    for transport in ["http", "grpc"] {
        let value = ping
            .get(transport)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("auth status ping missing {transport}"))?;
        ensure!(
            value.to_ascii_lowercase().contains("ok"),
            "auth status {transport} ping is not healthy"
        );
    }
    Ok(())
}

fn verify_restored_content(
    runner: &AnyrRunner,
    destination: &str,
    expected_name: &str,
    expected_body: &str,
) -> Result<()> {
    for _ in 0..40 {
        let listed = runner.run_json(&["object", "list", destination, "--all"], COMMAND_TIMEOUT)?;
        let items = listed
            .get("items")
            .and_then(Value::as_array)
            .or_else(|| listed.as_array())
            .ok_or_else(|| anyhow!("object list output missing items"))?;
        if let Some(restored_id) = items.iter().find_map(|item| {
            (item.get("name").and_then(Value::as_str) == Some(expected_name))
                .then(|| item.get("id").and_then(Value::as_str))
                .flatten()
        }) {
            let restored = runner.run_json(
                &["object", "get", destination, restored_id],
                COMMAND_TIMEOUT,
            )?;
            ensure!(
                restored.get("name").and_then(Value::as_str) == Some(expected_name),
                "restored object name mismatch"
            );
            ensure!(
                restored
                    .get("markdown")
                    .and_then(Value::as_str)
                    .is_some_and(|body| body.contains(expected_body)),
                "restored object body mismatch"
            );
            return Ok(());
        }
        thread::sleep(Duration::from_millis(750));
    }
    bail!("restored semantic content was not found before the deadline")
}

fn require_disposable_admission() -> Result<()> {
    ensure!(
        std::env::var("ANYTYPE_DISPOSABLE_TEST_PROCESS").as_deref() == Ok("1"),
        "ANYTYPE_DISPOSABLE_TEST_PROCESS=1 is required"
    );
    let prefix = std::env::var("ANYTYPE_TEST_SPACE_PREFIX")
        .context("ANYTYPE_TEST_SPACE_PREFIX is required")?;
    ensure!(!prefix.is_empty(), "ANYTYPE_TEST_SPACE_PREFIX is empty");
    ensure!(prefix.len() <= 470, "ANYTYPE_TEST_SPACE_PREFIX is too long");
    ensure!(
        prefix
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')),
        "ANYTYPE_TEST_SPACE_PREFIX contains an unsafe character"
    );
    Ok(())
}

fn write_execution_marker() -> Result<()> {
    let marker_path = required_absolute_path("ANYBACK_REQUIRED_GATE_MARKER")?;
    let nonce = std::env::var("ANYBACK_REQUIRED_GATE_NONCE")
        .context("ANYBACK_REQUIRED_GATE_NONCE is required")?;
    ensure!(!nonce.is_empty(), "ANYBACK_REQUIRED_GATE_NONCE is empty");
    fs::write(&marker_path, format!("{nonce}\n"))
        .with_context(|| format!("failed to write execution marker {}", marker_path.display()))
}

fn required_absolute_file(name: &str) -> Result<PathBuf> {
    let path = required_absolute_path(name)?;
    ensure!(
        path.is_file(),
        "{name} is not a readable file: {}",
        path.display()
    );
    File::open(&path).with_context(|| format!("{name} is not readable: {}", path.display()))?;
    Ok(path)
}

fn required_absolute_path(name: &str) -> Result<PathBuf> {
    let value = std::env::var_os(name).ok_or_else(|| anyhow!("{name} is required"))?;
    let path = PathBuf::from(value);
    ensure!(path.is_absolute(), "{name} must be an absolute path");
    Ok(path)
}

fn private_capture_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("failed to create capture file {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn read_result(path: &Path) -> Result<Zeroizing<String>> {
    let length = fs::metadata(path)?.len();
    ensure!(
        length <= MAX_RESULT_BYTES,
        "anyr result exceeded {MAX_RESULT_BYTES} bytes"
    );
    fs::read_to_string(path)
        .map(Zeroizing::new)
        .with_context(|| format!("failed to read {}", path.display()))
}

fn read_tail(path: &Path, limit: usize) -> Result<Zeroizing<String>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let limit_u64 = u64::try_from(limit).context("diagnostic limit does not fit u64")?;
    if length > limit_u64 {
        let offset = i64::try_from(limit).context("diagnostic limit does not fit i64")?;
        file.seek(SeekFrom::End(-offset))?;
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(limit.min(8192)));
    file.take(limit_u64).read_to_end(&mut bytes)?;
    Ok(Zeroizing::new(String::from_utf8_lossy(&bytes).into_owned()))
}

fn bounded_redacted(text: &str, limit: usize, redactions: &[Zeroizing<String>]) -> String {
    let mut redacted = Zeroizing::new(text.to_string());
    for secret in redactions {
        if !secret.is_empty() && redacted.contains(secret.as_str()) {
            let mut replaced = Zeroizing::new(redacted.replace(secret.as_str(), "[REDACTED]"));
            redacted.zeroize();
            std::mem::swap(&mut redacted, &mut replaced);
        }
    }
    let mut start = redacted.len().saturating_sub(limit);
    while !redacted.is_char_boundary(start) {
        start += 1;
    }
    redacted[start..].to_string()
}

fn display_command(args: &[&str]) -> String {
    let mut display = String::from("anyr");
    for arg in args {
        display.push(' ');
        display.push_str(&redact_argument(arg));
    }
    display
}

fn redact_argument(arg: &str) -> String {
    if Path::new(arg).is_absolute() {
        return "[ABSOLUTE_PATH]".to_string();
    }
    arg.to_string()
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn failure_diagnostics_are_bounded_and_redacted() {
        let secret = Zeroizing::new("credential-value-that-must-not-escape".to_string());
        let text = format!(
            "{}{}",
            "x".repeat(MAX_DIAGNOSTIC_BYTES * 2),
            secret.as_str()
        );
        let rendered = bounded_redacted(&text, MAX_DIAGNOSTIC_BYTES, std::slice::from_ref(&secret));
        assert!(rendered.len() <= MAX_DIAGNOSTIC_BYTES);
        assert!(!rendered.contains(secret.as_str()));
        assert!(rendered.contains("[REDACTED]"));
    }

    #[test]
    fn canonical_env_keystore_credentials_are_redacted() {
        for name in [
            "ANYTYPE_KEY_HTTP_TOKEN",
            "ANYTYPE_KEY_ACCOUNT_KEY",
            "ANYTYPE_KEY_SESSION_TOKEN",
        ] {
            assert!(
                REDACTION_ENV_NAMES.contains(&name),
                "missing credential redaction for {name}"
            );
        }
    }

    #[test]
    fn pending_space_drop_attempts_delete_and_proves_absence() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let script = temp.path().join("fake-anyr");
        let log = temp.path().join("commands.log");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1 $2\" = \"space delete\" ]; then cat >/dev/null; printf '{{}}\\n'; exit 0; fi\nif [ \"$1 $2\" = \"space list\" ]; then printf '{{\"items\":[]}}\\n'; exit 0; fi\nexit 1\n",
                log.display()
            ),
        )?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700))?;
        let server_log = temp.path().join("server.log");
        fs::write(&server_log, "redacted server log\n")?;

        let runner = AnyrRunner {
            binary: script,
            server_log,
            redactions: Vec::new(),
        };
        {
            let _pending = OwnedSpace {
                runner: &runner,
                name: "disposable-assertion-failure".to_string(),
                id: None,
                cleaned: false,
            };
        }

        let commands = fs::read_to_string(log)?;
        assert!(commands.contains("space delete disposable-assertion-failure"));
        assert!(commands.contains("space list --all"));
        Ok(())
    }

    #[test]
    fn cleanup_failures_preserve_the_primary_error() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let script = temp.path().join("fake-anyr");
        fs::write(
            &script,
            "#!/bin/sh\nprintf 'delete failed with configured token' >&2\nexit 1\n",
        )?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700))?;
        let server_log = temp.path().join("server.log");
        fs::write(&server_log, "redacted server log\n")?;
        let runner = AnyrRunner {
            binary: script,
            server_log,
            redactions: Vec::new(),
        };
        let mut source = OwnedSpace {
            runner: &runner,
            name: "cleanup-source".to_string(),
            id: Some("source-id".to_string()),
            cleaned: false,
        };
        let mut destination = OwnedSpace {
            runner: &runner,
            name: "cleanup-destination".to_string(),
            id: Some("destination-id".to_string()),
            cleaned: false,
        };

        let result = finish_with_cleanup(
            Err(anyhow!("primary backup failure")),
            &mut [&mut destination, &mut source],
        );
        source.cleaned = true;
        destination.cleaned = true;
        let error = result.expect_err("cleanup failures must fail the gate");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("primary backup failure"));
        assert!(rendered.contains("cleanup-source"));
        assert!(rendered.contains("cleanup-destination"));
        Ok(())
    }

    #[test]
    fn workflow_requires_exact_serial_execution_and_callback_marker() {
        let workflow = include_str!("../../.github/workflows/anyr-anyback-live.yml");
        assert!(workflow.contains("--exact --test-threads=1"));
        assert!(workflow.contains("ANYBACK_REQUIRED_GATE_MARKER"));
        assert!(workflow.contains("cmp --silent"));
    }
}
