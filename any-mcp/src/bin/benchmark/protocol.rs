// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use super::{
    config::{Server, ServerArtifact},
    secret::SecretSet,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use tiktoken_rs::CoreBPE;

const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_STDOUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 1024 * 1024;
const MAX_JSON_DEPTH: usize = 64;
const MAX_TRANSCRIPT_EVENTS: usize = 512;
const FRAME_QUEUE: usize = 8;
const REAP_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct Interval {
    pub start_ns: u64,
    pub first_byte_ns: u64,
    pub end_ns: u64,
}

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct JsonMeasures {
    pub wire_bytes: u64,
    pub canonical_bytes: u64,
    pub canonical_tokens: u64,
}

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct CatalogMeasures {
    pub response: JsonMeasures,
    pub tools_subtree_wire_bytes: u64,
    pub tools_subtree_canonical_bytes: u64,
    pub tools_subtree_tokens: u64,
}

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct StartupMeasures {
    pub spawn: Interval,
    pub initialize: Interval,
    pub list: Interval,
}

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct CallMeasures {
    pub interval: Interval,
    pub response: JsonMeasures,
}

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct RedactedStderr {
    pub bytes: u64,
    pub lines: u64,
    pub truncated: bool,
    pub summary_sha256: String,
}

struct ReceivedFrame {
    bytes: Vec<u8>,
    first_byte: Instant,
    complete: Instant,
}

#[derive(Clone)]
struct ReaderResult {
    bytes: usize,
    lines: usize,
    truncated: bool,
}

struct OwnedChild {
    child: Child,
    #[cfg(unix)]
    process_group: Option<i32>,
    cgroup_baseline: Option<BTreeSet<u32>>,
    shutdown_complete: bool,
}

impl OwnedChild {
    fn new(child: Child, cgroup_baseline: Option<BTreeSet<u32>>) -> Self {
        #[cfg(unix)]
        let process_group = i32::try_from(child.id()).ok();
        Self {
            child,
            #[cfg(unix)]
            process_group,
            cgroup_baseline,
            shutdown_complete: false,
        }
    }

    fn shutdown(&mut self) -> Result<(), String> {
        if self.shutdown_complete {
            return Ok(());
        }
        #[cfg(unix)]
        if let Some(process_group) = self.process_group {
            kill_process_group(process_group)?;
        } else {
            self.child
                .kill()
                .map_err(|error| format!("cannot terminate benchmark child: {error}"))?;
        }
        #[cfg(not(unix))]
        match self.child.kill() {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
            Err(error) => return Err(format!("cannot terminate benchmark child: {error}")),
        }
        self.kill_new_cgroup_members()?;
        let deadline = Instant::now() + REAP_DEADLINE;
        loop {
            if self
                .child
                .try_wait()
                .map_err(|error| format!("cannot reap benchmark child: {error}"))?
                .is_some()
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err("benchmark child did not reap before the deadline".to_owned());
            }
            thread::sleep(Duration::from_millis(10));
        }
        #[cfg(unix)]
        if let Some(process_group) = self.process_group {
            require_process_group_empty(process_group, deadline)?;
        }
        self.require_owned_cgroup_empty(deadline)?;
        self.shutdown_complete = true;
        Ok(())
    }

    fn kill_new_cgroup_members(&self) -> Result<(), String> {
        let Some(baseline) = &self.cgroup_baseline else {
            return Ok(());
        };
        let members = cgroup_members()?;
        let fresh = members
            .difference(baseline)
            .filter(|pid| **pid != std::process::id());
        #[cfg(unix)]
        for pid in fresh {
            let pid = i32::try_from(*pid)
                .map_err(|_| "cgroup member pid is outside the supported range".to_owned())?;
            // SAFETY: the PID came from the dedicated service's cgroup.procs.
            // ESRCH means it exited during enumeration.
            let result = unsafe { libc::kill(pid, libc::SIGKILL) };
            if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                return Err("cannot terminate owned cgroup descendant".to_owned());
            }
        }
        // A cgroup baseline exists only under the Linux supervisor; without
        // Unix signals an unexpected member cannot be terminated here.
        #[cfg(not(unix))]
        if fresh.count() > 0 {
            return Err("cannot terminate owned cgroup descendant".to_owned());
        }
        Ok(())
    }

    fn require_owned_cgroup_empty(&self, deadline: Instant) -> Result<(), String> {
        let Some(baseline) = &self.cgroup_baseline else {
            return Ok(());
        };
        loop {
            let remaining = cgroup_members()?
                .difference(baseline)
                .copied()
                .filter(|pid| *pid != std::process::id())
                .collect::<Vec<_>>();
            if remaining.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("owned cgroup descendants remained after teardown".to_owned());
            }
            self.kill_new_cgroup_members()?;
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

pub struct JsonRpcHost {
    child: Option<OwnedChild>,
    stdin: Option<ChildStdin>,
    frames: mpsc::Receiver<Result<ReceivedFrame, String>>,
    stdout_thread: Option<thread::JoinHandle<Result<usize, String>>>,
    stderr_thread: Option<thread::JoinHandle<Result<ReaderResult, String>>>,
    stdout_done: mpsc::Receiver<Result<usize, String>>,
    stderr_done: mpsc::Receiver<Result<ReaderResult, String>>,
    epoch: Instant,
    spawn: Interval,
    next_id: u64,
    transcript_events: usize,
    tokenizer: CoreBPE,
    response_deadline: Duration,
    write_deadline: Duration,
    drain_deadline: Duration,
}

pub struct InitializedHost {
    pub host: JsonRpcHost,
    pub startup: Option<StartupMeasures>,
    pub catalog: CatalogMeasures,
}

pub struct LaunchDomain {
    pub sudo: String,
    pub ip: String,
    pub setpriv: String,
    pub namespace: String,
    pub bootstrap: String,
    pub service_uid: u32,
    pub service_gid: u32,
    pub preserved_credential_fds: usize,
}

impl JsonRpcHost {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_deadlines(
        server: &Server,
        secrets: Arc<SecretSet>,
        credential_indices: &[usize],
        live_protected: bool,
        response_deadline: Duration,
        write_deadline: Duration,
        drain_deadline: Duration,
    ) -> Result<Self, String> {
        Self::spawn_with_deadlines_inner(
            server,
            secrets,
            credential_indices,
            live_protected,
            response_deadline,
            write_deadline,
            drain_deadline,
            false,
            None,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_failed_protection_for_test(
        server: &Server,
        secrets: Arc<SecretSet>,
        credential_indices: &[usize],
        response_deadline: Duration,
        write_deadline: Duration,
        drain_deadline: Duration,
    ) -> Result<Self, String> {
        Self::spawn_with_deadlines_inner(
            server,
            secrets,
            credential_indices,
            false,
            response_deadline,
            write_deadline,
            drain_deadline,
            true,
            None,
            false,
        )
    }

    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_in_namespace_for_test(
        server: &Server,
        secrets: Arc<SecretSet>,
        credential_indices: &[usize],
        domain: &LaunchDomain,
        response_deadline: Duration,
        write_deadline: Duration,
        drain_deadline: Duration,
    ) -> Result<Self, String> {
        Self::spawn_with_deadlines_inner(
            server,
            secrets,
            credential_indices,
            false,
            response_deadline,
            write_deadline,
            drain_deadline,
            false,
            Some(domain),
            false,
        )
    }

    #[allow(clippy::too_many_arguments, dead_code)]
    pub fn spawn_protected(
        server: &Server,
        secrets: Arc<SecretSet>,
        credential_indices: &[usize],
        domain: &LaunchDomain,
        response_deadline: Duration,
        write_deadline: Duration,
        drain_deadline: Duration,
    ) -> Result<Self, String> {
        Self::spawn_with_deadlines_inner(
            server,
            secrets,
            credential_indices,
            true,
            response_deadline,
            write_deadline,
            drain_deadline,
            false,
            Some(domain),
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_with_deadlines_inner(
        server: &Server,
        secrets: Arc<SecretSet>,
        credential_indices: &[usize],
        live_protected: bool,
        response_deadline: Duration,
        write_deadline: Duration,
        drain_deadline: Duration,
        fail_protection_after_spawn: bool,
        domain: Option<&LaunchDomain>,
        verify_domain: bool,
    ) -> Result<Self, String> {
        if response_deadline.is_zero() || write_deadline.is_zero() || drain_deadline.is_zero() {
            return Err("benchmark process deadlines must be positive".to_owned());
        }
        if server.credentials.len() != credential_indices.len() {
            return Err("credential index count differs from server credentials".to_owned());
        }
        if !live_protected
            && !matches!(
                &server.artifact,
                ServerArtifact::Local { revision, .. } if revision == "self-test"
            )
        {
            return Err("portable host launch is restricted to the fake-child fixture".to_owned());
        }
        if live_protected && domain.is_none() {
            return Err("live benchmark hosts require an attested arm namespace".to_owned());
        }
        if live_protected && !cfg!(target_os = "linux") {
            return Err("live benchmark hosts require Linux namespace protection".to_owned());
        }
        if live_protected {
            validate_live_domain(
                server,
                domain.ok_or_else(|| "protected host namespace is absent".to_owned())?,
            )?;
        }
        secrets.reject_public_values(server.environment.values().map(String::as_str))?;
        let tokenizer = tiktoken_rs::o200k_base()
            .map_err(|error| format!("cannot initialize benchmark tokenizer: {error}"))?;
        let cgroup_baseline = live_protected.then(cgroup_members).transpose()?;
        let verification_baseline = cgroup_baseline.clone();
        let epoch = Instant::now();
        let spawn_start = Instant::now();
        for index in credential_indices {
            let _ = secrets.value(*index)?;
        }
        let mut command = match domain {
            Some(domain) => protected_command(server, domain)?,
            None if server.credentials.is_empty() => {
                let mut command = Command::new(&server.executable);
                command
                    .args(&server.arguments)
                    .env_clear()
                    .envs(&server.environment);
                command
            }
            None => {
                return Err(
                    "credentialed benchmark hosts require the FD bootstrap namespace".to_owned(),
                );
            }
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            let inherited_fds = domain
                .map(|_| {
                    server
                        .credentials
                        .iter()
                        .map(|credential| credential.source_fd)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            // SAFETY: setpgid is async-signal-safe and neither allocates nor
            // acquires locks between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    if libc::setpgid(0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    for descriptor in &inherited_fds {
                        let flags = libc::fcntl(*descriptor, libc::F_GETFD);
                        if flags < 0
                            || libc::fcntl(*descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC)
                                < 0
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
        }
        let child = command
            .spawn()
            .map_err(|error| format!("cannot spawn benchmark child: {error}"))?;
        let mut child = OwnedChild::new(child, cgroup_baseline);
        let spawn_end = Instant::now();
        if fail_protection_after_spawn {
            return Err("injected post-spawn protection failure".to_owned());
        }
        if live_protected && verify_domain {
            let domain = domain.ok_or_else(|| "protected host namespace is absent".to_owned())?;
            verify_pid_domain(
                child.child.id(),
                verification_baseline
                    .as_ref()
                    .ok_or_else(|| "protected host cgroup baseline is absent".to_owned())?,
                &domain.namespace,
                domain.service_uid,
                domain.service_gid,
            )?;
        }
        let stdin = child
            .child
            .stdin
            .take()
            .ok_or_else(|| "benchmark child stdin is unavailable".to_owned())?;
        let stdout = child
            .child
            .stdout
            .take()
            .ok_or_else(|| "benchmark child stdout is unavailable".to_owned())?;
        let stderr = child
            .child
            .stderr
            .take()
            .ok_or_else(|| "benchmark child stderr is unavailable".to_owned())?;
        set_nonblocking(&stdin)?;
        let (sender, frames) = mpsc::sync_channel(FRAME_QUEUE);
        let (stdout_done_sender, stdout_done) = mpsc::sync_channel(1);
        let (stderr_done_sender, stderr_done) = mpsc::sync_channel(1);
        let stdout_secrets = Arc::clone(&secrets);
        let stdout_thread = thread::Builder::new()
            .name("any-mcp-benchmark-stdout".to_owned())
            .spawn(move || {
                let result = read_stdout(stdout, sender, &stdout_secrets);
                let _ = stdout_done_sender.send(result.clone());
                result
            })
            .map_err(|_| "cannot start bounded stdout reader".to_owned())?;
        let stderr_thread = match thread::Builder::new()
            .name("any-mcp-benchmark-stderr".to_owned())
            .spawn(move || {
                let result = read_stderr(stderr, &secrets);
                let _ = stderr_done_sender.send(result.clone());
                result
            }) {
            Ok(handle) => handle,
            Err(_) => {
                drop(stdin);
                let shutdown = child.shutdown();
                let drained =
                    receive_and_join(&stdout_done, Some(stdout_thread), "stdout", drain_deadline);
                shutdown?;
                let _ = drained?;
                return Err("cannot start bounded stderr reader".to_owned());
            }
        };
        Ok(Self {
            child: Some(child),
            stdin: Some(stdin),
            frames,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
            stdout_done,
            stderr_done,
            epoch,
            spawn: interval(epoch, spawn_start, spawn_start, spawn_end),
            next_id: 1,
            transcript_events: 0,
            tokenizer,
            response_deadline,
            write_deadline,
            drain_deadline,
        })
    }

    pub fn initialize(mut self, controlled_warm: bool) -> Result<InitializedHost, String> {
        let init_start = Instant::now();
        let (_, init_frame) = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "any-mcp-benchmark", "version": "1"}
            }),
            init_start,
        )?;
        let initialize = interval(
            self.epoch,
            init_start,
            init_frame.first_byte,
            init_frame.complete,
        );
        self.notification("notifications/initialized", json!({}))?;
        let first_list_start = Instant::now();
        let (first_list, first_list_frame) =
            self.request("tools/list", json!({}), first_list_start)?;
        let (catalog, list_interval) = if controlled_warm {
            let second_start = Instant::now();
            let (second, second_frame) = self.request("tools/list", json!({}), second_start)?;
            (
                catalog_measures(&second, &second_frame.bytes, &self.tokenizer)?,
                interval(
                    self.epoch,
                    second_start,
                    second_frame.first_byte,
                    second_frame.complete,
                ),
            )
        } else {
            (
                catalog_measures(&first_list, &first_list_frame.bytes, &self.tokenizer)?,
                interval(
                    self.epoch,
                    first_list_start,
                    first_list_frame.first_byte,
                    first_list_frame.complete,
                ),
            )
        };
        let startup = (!controlled_warm).then(|| StartupMeasures {
            spawn: self.spawn.clone(),
            initialize,
            list: list_interval,
        });
        Ok(InitializedHost {
            host: self,
            startup,
            catalog,
        })
    }

    pub fn call(&mut self, name: &str, arguments: Value) -> Result<(Value, CallMeasures), String> {
        let start = Instant::now();
        let (response, frame) = self.request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            start,
        )?;
        if response.get("error").is_some() {
            return Err("MCP tool call returned a JSON-RPC error".to_owned());
        }
        let measures = CallMeasures {
            interval: interval(self.epoch, start, frame.first_byte, frame.complete),
            response: json_measures(&response, &frame.bytes, &self.tokenizer)?,
        };
        Ok((response, measures))
    }

    pub fn finish(mut self) -> Result<RedactedStderr, String> {
        self.stdin.take();
        let deadline = Instant::now() + self.drain_deadline;
        let mut exit_status = None;
        loop {
            let status = self
                .child
                .as_mut()
                .ok_or_else(|| "benchmark child handle is unavailable".to_owned())?
                .child
                .try_wait()
                .map_err(|error| format!("cannot poll benchmark child: {error}"))?;
            if let Some(status) = status {
                exit_status = Some(status);
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.shutdown_child()?;
        let stdout_bytes = receive_and_join(
            &self.stdout_done,
            self.stdout_thread.take(),
            "stdout",
            self.drain_deadline,
        )??;
        if stdout_bytes > MAX_STDOUT_BYTES {
            return Err("benchmark child exceeded the cumulative stdout bound".to_owned());
        }
        let stderr = receive_and_join(
            &self.stderr_done,
            self.stderr_thread.take(),
            "stderr",
            self.drain_deadline,
        )??;
        let status = exit_status
            .ok_or_else(|| "benchmark child failed to exit before its drain deadline".to_owned())?;
        if !status.success() {
            return Err("benchmark child exited unsuccessfully".to_owned());
        }
        let summary = format!(
            "stderr-v1 bytes={} lines={} truncated={}",
            stderr.bytes, stderr.lines, stderr.truncated
        );
        Ok(RedactedStderr {
            bytes: to_u64(stderr.bytes),
            lines: to_u64(stderr.lines),
            truncated: stderr.truncated,
            summary_sha256: sha256(summary.as_bytes()),
        })
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        _start: Instant,
    ) -> Result<(Value, ReceivedFrame), String> {
        if self.transcript_events >= MAX_TRANSCRIPT_EVENTS {
            return Err("JSON-RPC transcript event bound exceeded".to_owned());
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| "JSON-RPC request id exhausted".to_owned())?;
        self.write_frame(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;
        self.transcript_events += 1;
        let frame =
            self.frames
                .recv_timeout(self.response_deadline)
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout => {
                        "JSON-RPC response deadline expired".to_owned()
                    }
                    mpsc::RecvTimeoutError::Disconnected => {
                        "benchmark child stdout closed before its response".to_owned()
                    }
                })??;
        let response: Value =
            serde_json::from_slice(frame.bytes.strip_suffix(b"\n").unwrap_or(&frame.bytes))
                .map_err(|_| "benchmark child emitted invalid JSON".to_owned())?;
        validate_depth(&response, 0)?;
        if response.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(
                "JSON-RPC response id does not match the sole outstanding request".to_owned(),
            );
        }
        self.transcript_events += 1;
        Ok((response, frame))
    }

    fn notification(&mut self, method: &str, params: Value) -> Result<(), String> {
        if self.transcript_events >= MAX_TRANSCRIPT_EVENTS {
            return Err("JSON-RPC transcript event bound exceeded".to_owned());
        }
        self.write_frame(&json!({"jsonrpc": "2.0", "method": method, "params": params}))?;
        self.transcript_events += 1;
        Ok(())
    }

    fn write_frame(&mut self, value: &Value) -> Result<(), String> {
        validate_depth(value, 0)?;
        let mut encoded = serde_json::to_vec(value)
            .map_err(|error| format!("cannot encode JSON-RPC request: {error}"))?;
        if encoded.len() > MAX_FRAME_BYTES {
            return Err("JSON-RPC request exceeds the frame bound".to_owned());
        }
        encoded.push(b'\n');
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "benchmark child stdin is closed".to_owned())?;
        write_all_deadline(stdin, &encoded, self.write_deadline)
    }

    fn shutdown_child(&mut self) -> Result<(), String> {
        self.child
            .as_mut()
            .ok_or_else(|| "benchmark child handle is unavailable".to_owned())?
            .shutdown()
    }
}

impl Drop for JsonRpcHost {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.shutdown_child();
        let _ = receive_and_join(
            &self.stdout_done,
            self.stdout_thread.take(),
            "stdout",
            self.drain_deadline,
        );
        let _ = receive_and_join(
            &self.stderr_done,
            self.stderr_thread.take(),
            "stderr",
            self.drain_deadline,
        );
    }
}

fn protected_command(server: &Server, domain: &LaunchDomain) -> Result<Command, String> {
    if domain.preserved_credential_fds == 0 || domain.preserved_credential_fds > 8 {
        return Err("protected host credential descriptor count is invalid".to_owned());
    }
    for path in [&domain.sudo, &domain.ip, &domain.setpriv, &domain.bootstrap] {
        if !std::path::Path::new(path).is_absolute() {
            return Err("protected host launch executable must be absolute".to_owned());
        }
    }
    if domain.namespace.is_empty()
        || domain.namespace.len() > 96
        || !domain
            .namespace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("protected host namespace is invalid".to_owned());
    }
    let close_from = 3usize.saturating_add(domain.preserved_credential_fds);
    if server.credentials.iter().any(|credential| {
        credential.source_fd < 3
            || usize::try_from(credential.source_fd).map_or(true, |fd| fd >= close_from)
    }) {
        return Err("host credential descriptor is outside the preserved range".to_owned());
    }
    let mut command = Command::new(&domain.sudo);
    command
        .env_clear()
        .args(["-n", "-C", &close_from.to_string(), "--"])
        .arg(&domain.ip)
        .args(["netns", "exec", &domain.namespace])
        .arg(&domain.setpriv)
        .args([
            &format!("--reuid={}", domain.service_uid),
            &format!("--regid={}", domain.service_gid),
            "--clear-groups",
        ])
        .arg(&domain.bootstrap)
        .args([
            "credential-exec",
            "--close-from",
            &close_from.to_string(),
            "--target-sha256",
            &server.executable_sha256,
        ]);
    for (name, value) in &server.environment {
        command.args(["--public-env", name, value]);
    }
    for credential in &server.credentials {
        command.args([
            "--credential-fd",
            &credential.child_environment,
            &credential.source_fd.to_string(),
        ]);
    }
    if let ServerArtifact::OfficialNpm {
        bundle_path,
        bundle_sha256,
        ..
    } = &server.artifact
    {
        let index = server
            .arguments
            .iter()
            .position(|argument| argument == bundle_path)
            .ok_or_else(|| "official bundle is absent from the server arguments".to_owned())?;
        command.args(["--pinned-arg", &index.to_string(), bundle_sha256]);
    }
    command
        .arg("--")
        .arg(&server.executable)
        .args(&server.arguments);
    Ok(command)
}

#[cfg(target_os = "linux")]
fn validate_live_domain(server: &Server, domain: &LaunchDomain) -> Result<(), String> {
    let local = std::env::var("ANY_MCP_BENCHMARK_LOCAL_NETNS")
        .map_err(|_| "protected local namespace attestation is absent".to_owned())?;
    let upstream = std::env::var("ANY_MCP_BENCHMARK_UPSTREAM_NETNS")
        .map_err(|_| "protected upstream namespace attestation is absent".to_owned())?;
    let sudo = std::env::var("ANY_MCP_BENCHMARK_SUDO")
        .map_err(|_| "protected sudo attestation is absent".to_owned())?;
    let ip = std::env::var("ANY_MCP_BENCHMARK_IP")
        .map_err(|_| "protected ip attestation is absent".to_owned())?;
    let setpriv = std::env::var("ANY_MCP_BENCHMARK_SETPRIV")
        .map_err(|_| "protected setpriv attestation is absent".to_owned())?;
    let service_uid = std::env::var("ANY_MCP_BENCHMARK_SERVICE_UID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| "protected service uid attestation is absent".to_owned())?;
    let service_gid = std::env::var("ANY_MCP_BENCHMARK_SERVICE_GID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| "protected service gid attestation is absent".to_owned())?;
    validate_non_root_service_identity(service_uid, service_gid)?;
    let current = std::env::current_exe()
        .map_err(|_| "protected bootstrap executable is unavailable".to_owned())?;
    validate_immutable_root_executable(&current)?;
    let expected_namespace = match server.artifact {
        ServerArtifact::Local { .. } => local,
        ServerArtifact::OfficialNpm { .. } => upstream,
    };
    if domain.namespace != expected_namespace
        || domain.sudo != sudo
        || domain.ip != ip
        || domain.setpriv != setpriv
        || domain.service_uid != service_uid
        || domain.service_gid != service_gid
        || std::path::Path::new(&domain.bootstrap) != current
    {
        return Err("protected host launch domain differs from supervisor attestation".to_owned());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn validate_non_root_service_identity(uid: u32, gid: u32) -> Result<(), String> {
    if uid == 0 || gid == 0 {
        return Err("protected service credentials must be non-root".to_owned());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn validate_immutable_root_executable(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    if !path.is_absolute()
        || std::fs::canonicalize(path)
            .map_err(|_| "cannot canonicalize protected bootstrap executable".to_owned())?
            != path
    {
        return Err("protected bootstrap executable path is mutable".to_owned());
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "cannot inspect protected bootstrap executable".to_owned())?;
    if !metadata.file_type().is_file()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err("protected bootstrap executable is not immutable root-owned data".to_owned());
    }
    let mut ancestor = path.parent();
    while let Some(directory) = ancestor {
        let metadata = std::fs::symlink_metadata(directory)
            .map_err(|_| "cannot inspect protected bootstrap ancestry".to_owned())?;
        if !metadata.file_type().is_dir()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err("protected bootstrap ancestry permits a path swap".to_owned());
        }
        ancestor = directory.parent();
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn validate_live_domain(_server: &Server, _domain: &LaunchDomain) -> Result<(), String> {
    Err("live benchmark hosts require Linux namespace protection".to_owned())
}

fn read_stdout(
    stdout: impl Read,
    sender: mpsc::SyncSender<Result<ReceivedFrame, String>>,
    secrets: &SecretSet,
) -> Result<usize, String> {
    let mut reader = BufReader::with_capacity(16 * 1024, stdout);
    let mut scanner = secrets.scanner();
    let mut cumulative = 0usize;
    loop {
        let mut frame = Vec::new();
        let mut first = [0u8; 1];
        let first_read = reader
            .read(&mut first)
            .map_err(|_| "cannot read benchmark child stdout".to_owned())?;
        if first_read == 0 {
            return Ok(cumulative);
        }
        let first_byte = Instant::now();
        frame.push(first[0]);
        let suffix_bytes = if first[0] == b'\n' {
            0
        } else {
            reader
                .by_ref()
                .take(MAX_FRAME_BYTES as u64)
                .read_until(b'\n', &mut frame)
                .map_err(|_| "cannot read benchmark child stdout".to_owned())?
        };
        let bytes = suffix_bytes.saturating_add(1);
        scanner.inspect(&frame)?;
        cumulative = cumulative
            .checked_add(bytes)
            .ok_or_else(|| "benchmark child stdout byte count overflowed".to_owned())?;
        if bytes > MAX_FRAME_BYTES || cumulative > MAX_STDOUT_BYTES {
            let _ = sender.try_send(Err(
                "benchmark child stdout exceeded a byte bound".to_owned()
            ));
            return Err("benchmark child stdout exceeded a byte bound".to_owned());
        }
        if !frame.ends_with(b"\n") {
            let _ = sender.try_send(Err(
                "benchmark child emitted an unterminated frame".to_owned()
            ));
            return Err("benchmark child emitted an unterminated frame".to_owned());
        }
        let complete = Instant::now();
        match sender.try_send(Ok(ReceivedFrame {
            bytes: frame,
            first_byte,
            complete,
        })) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                return Err("JSON-RPC frame queue exceeded its bound".to_owned());
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return Ok(cumulative),
        }
    }
}

fn read_stderr(stderr: impl Read, secrets: &SecretSet) -> Result<ReaderResult, String> {
    let mut reader = BufReader::with_capacity(16 * 1024, stderr);
    let mut scanner = secrets.scanner();
    let mut buffer = [0u8; 16 * 1024];
    let mut bytes = 0usize;
    let mut lines = 0usize;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| "cannot drain benchmark child stderr".to_owned())?;
        if read == 0 {
            break;
        }
        scanner.inspect(&buffer[..read])?;
        lines = lines.saturating_add(buffer[..read].iter().filter(|byte| **byte == b'\n').count());
        let next = bytes.saturating_add(read);
        if next > MAX_STDERR_BYTES {
            return Err("benchmark child stderr exceeded the byte bound".to_owned());
        }
        bytes = next;
    }
    Ok(ReaderResult {
        bytes,
        lines,
        truncated: false,
    })
}

fn catalog_measures(
    response: &Value,
    wire: &[u8],
    tokenizer: &CoreBPE,
) -> Result<CatalogMeasures, String> {
    let tools = response
        .pointer("/result/tools")
        .ok_or_else(|| "tools/list response has no tools subtree".to_owned())?;
    let tools_wire = serde_json::to_vec(tools)
        .map_err(|error| format!("cannot encode tools subtree: {error}"))?;
    let tools_canonical = canonical_bytes(tools)?;
    Ok(CatalogMeasures {
        response: json_measures(response, wire, tokenizer)?,
        tools_subtree_wire_bytes: to_u64(tools_wire.len()),
        tools_subtree_canonical_bytes: to_u64(tools_canonical.len()),
        tools_subtree_tokens: to_u64(
            tokenizer
                .encode_with_special_tokens(
                    std::str::from_utf8(&tools_canonical)
                        .map_err(|_| "canonical tools JSON is not UTF-8".to_owned())?,
                )
                .len(),
        ),
    })
}

fn json_measures(value: &Value, wire: &[u8], tokenizer: &CoreBPE) -> Result<JsonMeasures, String> {
    let canonical = canonical_bytes(value)?;
    let text = std::str::from_utf8(&canonical)
        .map_err(|_| "canonical response JSON is not UTF-8".to_owned())?;
    Ok(JsonMeasures {
        wire_bytes: to_u64(wire.len()),
        canonical_bytes: to_u64(canonical.len()),
        canonical_tokens: to_u64(tokenizer.encode_with_special_tokens(text).len()),
    })
}

pub fn canonical_bytes(value: &Value) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&canonical(value))
        .map_err(|error| format!("cannot encode canonical JSON: {error}"))
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        Value::Object(items) => {
            let ordered = items
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(Map::from_iter(ordered))
        }
        scalar => scalar.clone(),
    }
}

fn validate_depth(value: &Value, depth: usize) -> Result<(), String> {
    if depth > MAX_JSON_DEPTH {
        return Err("JSON-RPC frame exceeds the depth bound".to_owned());
    }
    match value {
        Value::Array(values) => {
            for value in values {
                validate_depth(value, depth + 1)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_depth(value, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn interval(epoch: Instant, start: Instant, first: Instant, end: Instant) -> Interval {
    Interval {
        start_ns: nanos(epoch, start),
        first_byte_ns: nanos(epoch, first),
        end_ns: nanos(epoch, end),
    }
}

fn nanos(epoch: Instant, value: Instant) -> u64 {
    u64::try_from(value.saturating_duration_since(epoch).as_nanos()).unwrap_or(u64::MAX)
}

fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut result = String::with_capacity(64);
    for byte in digest {
        let _ = std::fmt::Write::write_fmt(&mut result, format_args!("{byte:02x}"));
    }
    result
}

fn receive_and_join<T: Send + 'static>(
    receiver: &mpsc::Receiver<T>,
    handle: Option<thread::JoinHandle<T>>,
    name: &str,
    deadline: Duration,
) -> Result<T, String> {
    let result = receiver
        .recv_timeout(deadline)
        .map_err(|_| format!("benchmark {name} reader exceeded its drain deadline"))?;
    handle
        .ok_or_else(|| format!("benchmark {name} reader is unavailable"))?
        .join()
        .map_err(|_| format!("benchmark {name} reader panicked"))?;
    Ok(result)
}

#[cfg(unix)]
fn set_nonblocking(stdin: &ChildStdin) -> Result<(), String> {
    use std::os::fd::AsRawFd as _;

    let fd = stdin.as_raw_fd();
    // SAFETY: fd is the live child-stdin descriptor and F_GETFL does not
    // mutate memory.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err("cannot inspect benchmark child stdin flags".to_owned());
    }
    // SAFETY: fd remains live and O_NONBLOCK is a valid status flag.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err("cannot make benchmark child stdin nonblocking".to_owned());
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_nonblocking(_stdin: &ChildStdin) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn write_all_deadline(
    stdin: &mut ChildStdin,
    bytes: &[u8],
    duration: Duration,
) -> Result<(), String> {
    use std::os::fd::AsRawFd as _;

    let deadline = Instant::now() + duration;
    let mut offset = 0usize;
    while offset < bytes.len() {
        match stdin.write(&bytes[offset..]) {
            Ok(0) => return Err("benchmark child stdin closed during a request".to_owned()),
            Ok(written) => offset = offset.saturating_add(written),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err("benchmark child stdin write deadline expired".to_owned());
                }
                let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
                let mut descriptor = libc::pollfd {
                    fd: stdin.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: descriptor points to one initialized pollfd for the
                // duration of the call.
                let polled = unsafe { libc::poll(&mut descriptor, 1, millis) };
                if polled == 0 {
                    return Err("benchmark child stdin write deadline expired".to_owned());
                }
                if polled < 0 {
                    return Err("cannot poll benchmark child stdin".to_owned());
                }
                if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    return Err("benchmark child stdin became unavailable".to_owned());
                }
            }
            Err(error) => return Err(format!("cannot write JSON-RPC request: {error}")),
        }
    }
    match stdin.flush() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            Err("benchmark child stdin flush deadline expired".to_owned())
        }
        Err(error) => Err(format!("cannot flush JSON-RPC request: {error}")),
    }
}

#[cfg(not(unix))]
fn write_all_deadline(
    stdin: &mut ChildStdin,
    bytes: &[u8],
    _duration: Duration,
) -> Result<(), String> {
    stdin
        .write_all(bytes)
        .and_then(|()| stdin.flush())
        .map_err(|error| format!("cannot write JSON-RPC request: {error}"))
}

#[cfg(unix)]
fn kill_process_group(process_group: i32) -> Result<(), String> {
    // SAFETY: the negative PID targets the group created in pre_exec. ESRCH
    // means the complete group already exited.
    let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Err("cannot terminate benchmark child process group".to_owned());
    }
    Ok(())
}

/// Whether any member of the process group is still runnable.
///
/// A killed descendant that was reparented to a non-reaping init (a
/// container without an init process) stays a zombie: it holds no
/// descriptors and cannot run, but `kill(-pgid, 0)` keeps succeeding. Linux
/// procfs distinguishes that state; elsewhere signal zero is authoritative.
#[cfg(target_os = "linux")]
fn process_group_has_live_member(process_group: i32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return true;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `pid (comm) state ppid pgrp ...`; comm may contain spaces.
        let Some(rest) = stat.rsplit_once(") ").map(|(_, rest)| rest) else {
            continue;
        };
        let mut fields = rest.split_ascii_whitespace();
        let state = fields.next();
        let pgrp = fields.nth(1).and_then(|value| value.parse::<i32>().ok());
        if pgrp == Some(process_group) && state != Some("Z") {
            return true;
        }
    }
    false
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_group_has_live_member(_process_group: i32) -> bool {
    true
}

#[cfg(unix)]
fn require_process_group_empty(process_group: i32, deadline: Instant) -> Result<(), String> {
    loop {
        // SAFETY: signal zero only checks whether the owned process group has
        // members and does not change process state.
        let result = unsafe { libc::kill(-process_group, 0) };
        if result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        if !process_group_has_live_member(process_group) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("benchmark child process group remained after teardown".to_owned());
        }
        kill_process_group(process_group)?;
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn verify_pid_domain(
    pid: u32,
    cgroup_baseline: &BTreeSet<u32>,
    namespace: &str,
    service_uid: u32,
    service_gid: u32,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;

    let expected_cgroup = std::fs::read_to_string("/proc/self/cgroup")
        .map_err(|_| "cannot inspect benchmark cgroup".to_owned())?;
    let child_cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .map_err(|_| "cannot inspect child cgroup".to_owned())?;
    if child_cgroup != expected_cgroup {
        return Err("benchmark child escaped the supervisor cgroup".to_owned());
    }
    let target = std::fs::metadata(format!("/run/netns/{namespace}"))
        .map_err(|_| "cannot inspect protected host namespace".to_owned())?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let members = cgroup_members()?;
        for member in members.difference(cgroup_baseline) {
            let metadata = match std::fs::metadata(format!("/proc/{member}/ns/net")) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.dev() == target.dev() && metadata.ino() == target.ino() {
                let process = match std::fs::metadata(format!("/proc/{member}")) {
                    Ok(process) => process,
                    Err(_) => continue,
                };
                if process.uid() == service_uid && process.gid() == service_gid {
                    return Ok(());
                }
            }
        }
        if Instant::now() >= deadline {
            return Err("benchmark child never entered its attested network namespace".to_owned());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(target_os = "linux"))]
fn verify_pid_domain(
    _pid: u32,
    _cgroup_baseline: &BTreeSet<u32>,
    _namespace: &str,
    _service_uid: u32,
    _service_gid: u32,
) -> Result<(), String> {
    Err("live benchmarks require the protected Linux supervisor".to_owned())
}

#[cfg(target_os = "linux")]
fn cgroup_members() -> Result<BTreeSet<u32>, String> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup")
        .map_err(|_| "cannot inspect benchmark cgroup".to_owned())?;
    let relative = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| "benchmark requires cgroup v2".to_owned())?;
    let members = std::fs::read_to_string(format!("/sys/fs/cgroup{relative}/cgroup.procs"))
        .map_err(|_| "cannot enumerate benchmark cgroup members".to_owned())?;
    members
        .lines()
        .map(|line| {
            line.parse::<u32>()
                .map_err(|_| "benchmark cgroup contains an invalid pid".to_owned())
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn cgroup_members() -> Result<BTreeSet<u32>, String> {
    Err("live benchmarks require the protected Linux supervisor".to_owned())
}

// Both tests exercise the Unix supervisor launch topology.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::{Credential, ServerArtifact};

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_mutable_or_swappable_bootstrap_paths() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let root = std::env::temp_dir().join(format!(
            "any-mcp-bootstrap-adversary-{}-{}",
            std::process::id(),
            getrandom::u64().unwrap_or(0)
        ));
        std::fs::create_dir(&root).expect("create bootstrap fixture directory");
        let mutable = root.join("mutable-bootstrap");
        std::fs::write(&mutable, b"fixture").expect("write mutable bootstrap fixture");
        std::fs::set_permissions(&mutable, std::fs::Permissions::from_mode(0o777))
            .expect("make bootstrap fixture mutable");
        assert!(validate_immutable_root_executable(&mutable).is_err());
        let swappable = root.join("swappable-bootstrap");
        symlink(&mutable, &swappable).expect("create bootstrap path-swap fixture");
        assert!(validate_immutable_root_executable(&swappable).is_err());
        std::fs::remove_dir_all(root).expect("remove bootstrap adversary fixture");
    }

    #[test]
    fn protected_parent_command_contains_only_fd_credential_mappings() {
        let server = Server {
            executable: "/fixture/server".to_owned(),
            arguments: vec!["mcp".to_owned()],
            executable_sha256: "a".repeat(64),
            environment: BTreeMap::from([(
                "ANYTYPE_API_BASE_URL".to_owned(),
                "http://127.0.0.1:31009".to_owned(),
            )]),
            credentials: vec![Credential {
                source_fd: 3,
                child_environment: "ANYTYPE_API_KEY".to_owned(),
            }],
            artifact: ServerArtifact::Local {
                revision: "b".repeat(40),
                features: Vec::new(),
            },
        };
        let mut domain = LaunchDomain {
            sudo: "/fixture/sudo".to_owned(),
            ip: "/fixture/ip".to_owned(),
            setpriv: "/fixture/setpriv".to_owned(),
            namespace: "local-arm".to_owned(),
            bootstrap: "/fixture/benchmark".to_owned(),
            service_uid: 1000,
            service_gid: 1000,
            preserved_credential_fds: 1,
        };
        let command = protected_command(&server, &domain).expect("protected command");
        assert_eq!(command.get_program(), "/fixture/sudo");
        assert_eq!(command.get_envs().count(), 0);
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            [
                "-n",
                "-C",
                "4",
                "--",
                "/fixture/ip",
                "netns",
                "exec",
                "local-arm",
                "/fixture/setpriv",
                "--reuid=1000",
                "--regid=1000",
                "--clear-groups",
                "/fixture/benchmark",
                "credential-exec",
                "--close-from",
                "4",
                "--target-sha256",
                &"a".repeat(64),
                "--public-env",
                "ANYTYPE_API_BASE_URL",
                "http://127.0.0.1:31009",
                "--credential-fd",
                "ANYTYPE_API_KEY",
                "3",
                "--",
                "/fixture/server",
                "mcp",
            ]
        );
        domain.namespace = "upstream-arm".to_owned();
        let upstream = protected_command(&server, &domain).expect("upstream command");
        assert_eq!(
            upstream.get_args().nth(7),
            Some(std::ffi::OsStr::new("upstream-arm"))
        );
    }
}
