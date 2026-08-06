// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Portable, bounded production-process driver shared by integration suites.

use std::{
    any::Any,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

#[allow(dead_code)]
const DEFAULT_DEADLINE: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How long a non-graceful shutdown waits for an exiting child to become
/// reapable. On Linux a dying process closes its descriptors (producing the
/// stdout EOF that triggers this path) before `waitpid` can observe the exit,
/// so an immediate single `try_wait` races the kernel and misclassifies a
/// clean exit as a hang.
const REAP_GRACE: Duration = Duration::from_secs(1);
pub const FRAME_QUEUE_CAPACITY: usize = 32;
pub const MAX_STDOUT_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_STDOUT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_STDERR_LINE_BYTES: usize = 64 * 1024;
pub const MAX_STDERR_BYTES: usize = 1024 * 1024;

/// Captured, bounded output from a completed production process.
pub struct ProcessOutput {
    pub stdout: Vec<u8>,
    /// Exact stdout frames consumed by the protocol driver, including LF framing.
    pub consumed_stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Fixed classification of the child status without numeric or platform detail.
    #[allow(dead_code)]
    pub exit_category: &'static str,
}

/// Bounded output retained when a child exits or times out before responding.
#[allow(dead_code)]
pub struct ProcessFailureEvidence {
    pub category: &'static str,
    pub transcript: String,
    pub output: ProcessOutput,
}

/// A portable line-framed JSON-RPC driver for the real `any-mcp` binary.
pub struct ProtocolProcess {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    frames: mpsc::Receiver<Vec<u8>>,
    stdout_thread: Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    stderr_thread: Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    consumed_stdout: Vec<u8>,
    transcript: Vec<String>,
    deadline: Duration,
    failure: Option<ProcessFailureEvidence>,
}

impl ProtocolProcess {
    /// Spawns a configured command with bounded stdin/stdout/stderr capture.
    #[allow(dead_code)]
    pub fn spawn(command: Command) -> Self {
        Self::spawn_with_deadline(command, DEFAULT_DEADLINE)
    }

    /// Spawns with a caller-selected finite response and shutdown deadline.
    pub fn spawn_with_deadline(mut command: Command, deadline: Duration) -> Self {
        assert!(!deadline.is_zero(), "process deadline must be positive");
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn production any-mcp binary");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");
        let (frame_tx, frames) = mpsc::sync_channel(FRAME_QUEUE_CAPACITY);
        let stdout_thread = thread::spawn(move || read_stdout(stdout, &frame_tx));
        let stderr_thread = thread::spawn(move || {
            read_bounded_stream(stderr, MAX_STDERR_LINE_BYTES, MAX_STDERR_BYTES)
        });
        Self {
            child: Some(child),
            stdin: Some(stdin),
            frames,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
            consumed_stdout: Vec::new(),
            transcript: Vec::new(),
            deadline,
            failure: None,
        }
    }

    /// Sends one JSON-RPC value as an LF-delimited frame.
    pub fn send(&mut self, frame: Value) {
        self.send_bytes(&serde_json::to_vec(&frame).expect("encode JSON-RPC frame"));
    }

    /// Sends one raw LF-delimited frame.
    pub fn send_bytes(&mut self, frame: &[u8]) {
        let stdin = self.stdin.as_mut().expect("open child stdin");
        stdin.write_all(frame).expect("write JSON-RPC frame");
        stdin.write_all(b"\n").expect("terminate JSON-RPC frame");
        stdin.flush().expect("flush JSON-RPC frame");
    }

    /// Sends a JSON-RPC notification.
    pub fn notification(&mut self, method: &str, params: Value) {
        self.transcript.push(format!("-> notification {method}"));
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Sends a numeric-ID JSON-RPC request and reads its correlated response.
    pub fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.request_with_id(json!(id), method, params)
    }

    /// Sends a JSON-RPC request and reads its correlated response.
    pub fn request_with_id(&mut self, id: Value, method: &str, params: Value) -> Value {
        self.transcript
            .push(format!("-> id={} {method}", display_id(&id)));
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "method": method,
            "params": params
        }));
        let response = self.read_frame();
        assert_eq!(response["id"], id, "response id for {method}");
        self.record_response(&response);
        response
    }

    /// Records a response read after a caller constructed and sent its request.
    pub fn record_response(&mut self, response: &Value) {
        let id = display_id(&response["id"]);
        let outcome = response
            .get("error")
            .and_then(|error| error.get("code"))
            .map_or_else(
                || {
                    response
                        .pointer("/result/structuredContent/code")
                        .and_then(Value::as_str)
                        .map_or_else(|| "result".to_owned(), |code| format!("tool-error:{code}"))
                },
                |code| format!("jsonrpc-error:{code}"),
            );
        self.transcript.push(format!("<- id={id} {outcome}"));
    }

    /// Reads one bounded LF-delimited JSON-RPC frame as exact wire bytes.
    pub fn read_frame_bytes(&mut self) -> Vec<u8> {
        let bytes = match self.frames.recv_timeout(self.deadline) {
            Ok(bytes) => bytes,
            Err(error) => {
                let transcript = self.redacted_transcript();
                let output = self.shutdown(false, false).unwrap_or(ProcessOutput {
                    stdout: Vec::new(),
                    consumed_stdout: Vec::new(),
                    stderr: Vec::new(),
                    exit_category: "unknown",
                });
                let category = match error {
                    mpsc::RecvTimeoutError::Timeout => "response_timeout",
                    mpsc::RecvTimeoutError::Disconnected => "child_eof",
                };
                self.failure = Some(ProcessFailureEvidence {
                    category,
                    transcript,
                    output,
                });
                // Raw stderr remains in `failure` for the scenario owner to
                // sanitize. The panic hook must never format it first.
                panic!("bounded protocol process failed: {category}");
            }
        };
        assert_eq!(bytes.last(), Some(&b'\n'), "one LF-delimited stdout frame");
        assert_ne!(bytes.first(), Some(&b'\n'), "no blank stdout frame");
        self.consumed_stdout.extend_from_slice(&bytes);
        bytes
    }

    /// Reads and decodes one bounded LF-delimited JSON-RPC frame.
    pub fn read_frame(&mut self) -> Value {
        let bytes = self.read_frame_bytes();
        serde_json::from_slice(&bytes[..bytes.len() - 1]).expect("stdout line is one JSON frame")
    }

    /// Returns the metadata-only request/response transcript.
    pub fn redacted_transcript(&self) -> String {
        self.transcript.join("\n")
    }

    /// Takes evidence retained by a non-responsive or exited child.
    #[allow(dead_code)]
    pub fn take_failure(&mut self) -> Option<ProcessFailureEvidence> {
        self.failure.take()
    }

    fn shutdown(&mut self, graceful: bool, require_success: bool) -> Result<ProcessOutput, String> {
        drop(self.stdin.take());
        let mut errors = Vec::new();
        let mut terminated_by_driver = false;
        let status = self.child.take().and_then(|mut child| {
            if graceful {
                let deadline = Instant::now() + self.deadline;
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
                        Ok(None) => {
                            terminated_by_driver = true;
                            errors.push("any-mcp did not exit after clean stdin EOF".to_owned());
                            if let Err(error) = child.kill() {
                                errors.push(format!("kill hung any-mcp child: {error}"));
                            }
                            break match child.wait() {
                                Ok(status) => Some(status),
                                Err(error) => {
                                    errors.push(format!("wait for killed any-mcp child: {error}"));
                                    None
                                }
                            };
                        }
                        Err(error) => {
                            errors.push(format!("poll any-mcp child status: {error}"));
                            if let Err(kill_error) = child.kill() {
                                errors.push(format!(
                                    "kill any-mcp child after poll error: {kill_error}"
                                ));
                            }
                            break match child.wait() {
                                Ok(status) => Some(status),
                                Err(wait_error) => {
                                    errors.push(format!("wait for any-mcp child: {wait_error}"));
                                    None
                                }
                            };
                        }
                    }
                }
            } else {
                let deadline = Instant::now() + REAP_GRACE;
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
                        Ok(None) => {
                            terminated_by_driver = true;
                            if let Err(error) = child.kill() {
                                errors.push(format!("kill dropped any-mcp child: {error}"));
                            }
                            break match child.wait() {
                                Ok(status) => Some(status),
                                Err(error) => {
                                    errors
                                        .push(format!("wait for dropped any-mcp child: {error}"));
                                    None
                                }
                            };
                        }
                        Err(error) => {
                            errors.push(format!("poll dropped any-mcp child: {error}"));
                            terminated_by_driver = true;
                            if let Err(kill_error) = child.kill() {
                                errors.push(format!(
                                    "kill dropped any-mcp child after poll error: {kill_error}"
                                ));
                            }
                            break match child.wait() {
                                Ok(status) => Some(status),
                                Err(wait_error) => {
                                    errors.push(format!(
                                        "wait for dropped any-mcp child after poll error: {wait_error}"
                                    ));
                                    None
                                }
                            };
                        }
                    }
                }
            }
        });
        if require_success
            && let Some(status) = status.as_ref()
            && !status.success()
        {
            errors.push(format!(
                "any-mcp exited unsuccessfully after stdin EOF: {status}"
            ));
        }

        let stdout = join_reader(self.stdout_thread.take(), "stdout", &mut errors);
        let stderr = join_reader(self.stderr_thread.take(), "stderr", &mut errors);
        let exit_category = if terminated_by_driver {
            "terminated"
        } else {
            match status.as_ref() {
                Some(status) if status.success() => "success",
                Some(status) if status.code().is_some() => "exit_code",
                Some(_) => "signal",
                None => "unknown",
            }
        };
        if errors.is_empty() {
            Ok(ProcessOutput {
                stdout,
                consumed_stdout: std::mem::take(&mut self.consumed_stdout),
                stderr,
                exit_category,
            })
        } else {
            Err(errors.join("; "))
        }
    }

    /// Closes stdin, waits for clean exit, and returns bounded output.
    pub fn finish(self) -> ProcessOutput {
        self.try_finish()
            .unwrap_or_else(|error| panic!("bounded protocol process cleanup failed: {error}"))
    }

    /// Terminates the child immediately and returns bounded captured output.
    ///
    /// This is reserved for crash/restart acceptance scenarios that must prove
    /// startup reconciliation rather than orderly shutdown cleanup.
    #[cfg(feature = "acceptance-harness")]
    pub fn terminate(mut self) -> Result<ProcessOutput, String> {
        if let Some(child) = self.child.as_mut() {
            child
                .kill()
                .map_err(|_| "terminate production any-mcp child".to_owned())?;
        }
        self.shutdown(false, false)
    }

    /// Closes stdin and reports bounded shutdown defects without panicking.
    pub fn try_finish(mut self) -> Result<ProcessOutput, String> {
        let output = self.shutdown(true, true)?;
        if output.stdout != output.consumed_stdout {
            return Err("any-mcp emitted unconsumed protocol output before shutdown".to_owned());
        }
        Ok(output)
    }
}

#[cfg(feature = "acceptance-harness")]
const _: fn(ProtocolProcess) -> Result<ProcessOutput, String> = ProtocolProcess::terminate;

impl Drop for ProtocolProcess {
    fn drop(&mut self) {
        if (self.child.is_some()
            || self.stdin.is_some()
            || self.stdout_thread.is_some()
            || self.stderr_thread.is_some())
            && let Err(error) = self.shutdown(false, false)
            && !thread::panicking()
        {
            panic!("bounded dropped protocol process cleanup failed: {error}");
        }
    }
}

fn display_id(id: &Value) -> String {
    match id {
        Value::String(value) => format!("string:{}b", value.len()),
        Value::Number(value) => value.to_string(),
        Value::Null => "null".to_owned(),
        _ => "invalid".to_owned(),
    }
}

pub fn read_stdout(
    stdout: impl Read,
    frames: &mpsc::SyncSender<Vec<u8>>,
) -> std::io::Result<Vec<u8>> {
    let mut reader = BufReader::with_capacity(8 * 1024, stdout);
    let mut all = Vec::new();
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if !line.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "stdout ended with an unterminated protocol frame",
                ));
            }
            return Ok(all);
        }
        let chunk_len = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        let next_line_len = line
            .len()
            .checked_add(chunk_len)
            .ok_or_else(|| std::io::Error::other("stdout line length overflow"))?;
        if next_line_len > MAX_STDOUT_LINE_BYTES {
            return Err(std::io::Error::other(
                "stdout protocol frame exceeds byte cap",
            ));
        }
        let next_total = all
            .len()
            .checked_add(chunk_len)
            .ok_or_else(|| std::io::Error::other("stdout aggregate length overflow"))?;
        if next_total > MAX_STDOUT_BYTES {
            return Err(std::io::Error::other("stdout exceeds aggregate byte cap"));
        }
        let complete = available[chunk_len - 1] == b'\n';
        line.extend_from_slice(&available[..chunk_len]);
        all.extend_from_slice(&available[..chunk_len]);
        reader.consume(chunk_len);
        if complete {
            frames
                .try_send(std::mem::take(&mut line))
                .map_err(|error| {
                    std::io::Error::other(format!(
                        "bounded stdout frame queue unavailable: {error}"
                    ))
                })?;
        }
    }
}

pub fn read_bounded_stream(
    mut stream: impl Read,
    max_line_bytes: usize,
    max_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    let mut all = Vec::new();
    let mut line_bytes = 0_usize;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(all);
        }
        let next_total = all
            .len()
            .checked_add(read)
            .ok_or_else(|| std::io::Error::other("diagnostic aggregate length overflow"))?;
        if next_total > max_bytes {
            return Err(std::io::Error::other(
                "diagnostic stream exceeds aggregate byte cap",
            ));
        }
        for byte in &buffer[..read] {
            line_bytes = line_bytes
                .checked_add(1)
                .ok_or_else(|| std::io::Error::other("diagnostic line length overflow"))?;
            if line_bytes > max_line_bytes {
                return Err(std::io::Error::other("diagnostic line exceeds byte cap"));
            }
            if *byte == b'\n' {
                line_bytes = 0;
            }
        }
        all.extend_from_slice(&buffer[..read]);
    }
}

fn join_reader(
    handle: Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    name: &str,
    errors: &mut Vec<String>,
) -> Vec<u8> {
    let Some(handle) = handle else {
        return Vec::new();
    };
    match handle.join() {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => {
            errors.push(format!("read bounded {name}: {error}"));
            Vec::new()
        }
        Err(payload) => {
            errors.push(format!(
                "{name} reader panicked: {}",
                panic_payload(&payload)
            ));
            Vec::new()
        }
    }
}

fn panic_payload(payload: &Box<dyn Any + Send>) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}
