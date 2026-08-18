// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

mod config;
#[allow(dead_code)]
mod oracle;
mod protocol;
mod run_root;
mod secret;
mod stats;

use std::{
    io::{BufRead as _, Write as _},
    path::Path,
    process::ExitCode,
    sync::Arc,
};

use config::{Server, ServerArtifact};
use protocol::JsonRpcHost;
use secret::SecretSet;
use zeroize::Zeroizing;

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("any-mcp benchmark: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    match arguments.as_slice() {
        [command, path] if command == "validate-config" => {
            let _ = config::Config::read(Path::new(path))?;
            println!("benchmark-config-v1 valid");
            Ok(())
        }
        [command, launcher, run_parent, prefix, benchmark, config]
            if command == "launcher-bootstrap" =>
        {
            launcher_bootstrap(launcher, run_parent, prefix, benchmark, config)
        }
        [command, executable] if command == "self-test-host" => {
            self_test_host(executable)
        }
        [command, executable] if command == "self-test-host-namespace" => {
            self_test_host_namespace(executable)
        }
        [command, executable, mode] if command == "self-test-reject" => {
            self_test_reject(executable, mode)
        }
        [command, executable] if command == "self-test-blocked-stdin" => {
            self_test_blocked_stdin(executable)
        }
        [command, executable] if command == "self-test-descendant" => {
            self_test_descendant(executable)
        }
        [command, executable] if command == "self-test-protection-cleanup" => {
            self_test_protection_cleanup(executable)
        }
        [command] if command == "self-test-summary" => {
            let event = stats::fixture_summary_event()?;
            serde_json::to_writer(std::io::stdout(), &event)
                .map_err(|error| format!("cannot encode summary fixture: {error}"))?;
            println!();
            Ok(())
        }
        [command] if command == "self-test-pair" => {
            serde_json::to_writer(std::io::stdout(), &stats::fixture_pair_event())
                .map_err(|error| format!("cannot encode pair fixture: {error}"))?;
            println!();
            Ok(())
        }
        [command, executable] if command == "self-test-sudo-argv" => {
            self_test_sudo_argv(executable)
        }
        [command] if command == "self-test-fd-isolation" => self_test_fd_isolation(),
        [command, open, closed] if command == "fd-probe" => fd_probe(open, closed),
        _ if arguments.len() == 19
            && arguments.first().map(String::as_str) == Some("-n")
            && arguments.get(13).map(String::as_str) == Some("probe-fds") =>
        {
            if arguments.get(7).map(String::as_str) == Some("stall-ns") {
                fake_sudo_stall()
            } else {
                fake_sudo_assert(&arguments)
            }
        }
        [command, rest @ ..] if command == "credential-exec" => credential_exec(rest),
        _ if arguments.first().map(String::as_str) == Some("-n") => {
            fake_sudo_host(&arguments)
        }
        [command, mode] if command == "fake-child" => fake_child(mode),
        [command, run_root, config, local_netns, upstream_netns] if command == "supervise" => {
            supervise(
                Path::new(run_root),
                Path::new(config),
                local_netns,
                upstream_netns,
            )
        }
        [command, count, namespace, expected_uid, expected_gid, expected_unit]
            if command == "probe-fds" =>
        {
            probe_fds(count, namespace, expected_uid, expected_gid, expected_unit)
        }
        _ => Err(
            "usage: any-mcp-benchmark {validate-config CONFIG|supervise RUN_ROOT CONFIG LOCAL_NETNS UPSTREAM_NETNS}"
                .to_owned(),
        ),
    }
}

#[cfg(target_os = "linux")]
fn launcher_bootstrap(
    launcher: &str,
    run_parent: &str,
    prefix: &str,
    benchmark: &str,
    config: &str,
) -> Result<(), String> {
    use std::os::unix::process::CommandExt as _;

    let launcher = Path::new(launcher);
    if !launcher.is_absolute() || !launcher.is_file() {
        return Err("benchmark launcher path is invalid".to_owned());
    }
    let current = std::env::current_exe()
        .map_err(|_| "cannot resolve benchmark launcher bootstrap".to_owned())?;
    let expected = Path::new(benchmark)
        .canonicalize()
        .map_err(|_| "cannot canonicalize benchmark launcher bootstrap".to_owned())?;
    if current != expected {
        return Err("benchmark launcher bootstrap identity changed".to_owned());
    }
    let descriptors = parse_launcher_credential_fds()?;
    for descriptor in &descriptors {
        validate_open_fd(*descriptor)?;
        mark_fd_cloexec(*descriptor)?;
    }
    let parent = std::process::id();
    let mut command = std::process::Command::new(launcher);
    command
        .args([run_parent, prefix, benchmark, config])
        .env("ANY_MCP_BENCHMARK_FDS_ISOLATED", "1")
        .env("ANY_MCP_BENCHMARK_FD_SOURCE_PID", parent.to_string());
    // SAFETY: prctl and getppid are async-signal-safe. The parent check closes
    // the fork-to-prctl race before the shell can execute ancillary helpers.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0
                || libc::getppid() != i32::try_from(parent).unwrap_or(-1)
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let status = command
        .status()
        .map_err(|_| "cannot start isolated benchmark launcher".to_owned())?;
    if !status.success() {
        return Err("isolated benchmark launcher failed".to_owned());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn parse_launcher_credential_fds() -> Result<Vec<i32>, String> {
    let encoded = std::env::var("ANY_MCP_BENCHMARK_CREDENTIAL_FDS")
        .map_err(|_| "credential descriptor list is absent".to_owned())?;
    let mut observed = std::collections::BTreeSet::new();
    let mut descriptors = Vec::new();
    for item in encoded.split(',') {
        let descriptor = item
            .parse::<i32>()
            .map_err(|_| "credential descriptor list is invalid".to_owned())?;
        if !(3..=64).contains(&descriptor) || !observed.insert(descriptor) {
            return Err("credential descriptor list is invalid".to_owned());
        }
        descriptors.push(descriptor);
    }
    if descriptors.is_empty() || descriptors.len() > 8 {
        return Err("credential descriptor list is invalid".to_owned());
    }
    Ok(descriptors)
}

#[cfg(not(target_os = "linux"))]
fn launcher_bootstrap(
    _launcher: &str,
    _run_parent: &str,
    _prefix: &str,
    _benchmark: &str,
    _config: &str,
) -> Result<(), String> {
    Err("protected benchmark launcher requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn supervise(
    run_root: &Path,
    config_path: &Path,
    local_netns: &str,
    upstream_netns: &str,
) -> Result<(), String> {
    let config = config::Config::read(config_path)?;
    let count = inherited_credential_count()?;
    let bootstrap = std::env::current_exe()
        .map_err(|_| "protected bootstrap executable is unavailable".to_owned())?;
    let service_uid = required_numeric_env("ANY_MCP_BENCHMARK_SERVICE_UID")?;
    let service_gid = required_numeric_env("ANY_MCP_BENCHMARK_SERVICE_GID")?;
    with_validated_launch_identity(&bootstrap, service_uid, service_gid, |identity| {
        validate_credential_fds(&config, count)?;
        validate_arm_namespaces(local_netns, upstream_netns, count, identity)
    })?;
    live_preflight(run_root, config_path)
}

#[cfg(target_os = "linux")]
struct ValidatedLaunchIdentity {
    bootstrap: std::path::PathBuf,
    service_uid: u32,
    service_gid: u32,
}

#[cfg(target_os = "linux")]
fn with_validated_launch_identity<T>(
    bootstrap: &Path,
    service_uid: u32,
    service_gid: u32,
    action: impl FnOnce(&ValidatedLaunchIdentity) -> Result<T, String>,
) -> Result<T, String> {
    protocol::validate_non_root_service_identity(service_uid, service_gid)?;
    protocol::validate_immutable_root_executable(bootstrap)?;
    action(&ValidatedLaunchIdentity {
        bootstrap: bootstrap.to_owned(),
        service_uid,
        service_gid,
    })
}

#[cfg(not(target_os = "linux"))]
fn supervise(
    _run_root: &Path,
    _config_path: &Path,
    _local_netns: &str,
    _upstream_netns: &str,
) -> Result<(), String> {
    Err("live benchmarks require the protected Linux supervisor".to_owned())
}

#[cfg(target_os = "linux")]
fn inherited_credential_count() -> Result<usize, String> {
    let listen_pid = std::env::var("LISTEN_PID")
        .map_err(|_| "systemd credential owner pid is absent".to_owned())?
        .parse::<u32>()
        .map_err(|_| "systemd credential owner pid is invalid".to_owned())?;
    if listen_pid != std::process::id() {
        return Err("systemd credentials belong to another process".to_owned());
    }
    let configured = std::env::var("ANY_MCP_BENCHMARK_CREDENTIAL_COUNT")
        .map_err(|_| "credential descriptor count is absent".to_owned())?
        .parse::<usize>()
        .map_err(|_| "credential descriptor count is invalid".to_owned())?;
    let systemd = std::env::var("LISTEN_FDS")
        .map_err(|_| "systemd did not preserve credential descriptors".to_owned())?
        .parse::<usize>()
        .map_err(|_| "systemd credential descriptor count is invalid".to_owned())?;
    if configured == 0 || configured > 8 || configured != systemd {
        return Err("preserved credential descriptor count does not match".to_owned());
    }
    Ok(configured)
}

#[cfg(target_os = "linux")]
fn validate_credential_fds(config: &config::Config, count: usize) -> Result<(), String> {
    let mut assignments = vec![config.oracle.credential_fd];
    assignments.extend(config.local.credentials.iter().map(|item| item.source_fd));
    assignments.extend(
        config
            .upstream
            .credentials
            .iter()
            .map(|item| item.source_fd),
    );
    if let Some(agent) = &config.agent {
        assignments.push(agent.credential_fd);
    }
    let expected = validate_credential_fd_assignments(&assignments, count)?;
    for descriptor in expected {
        validate_open_fd(descriptor)?;
        mark_fd_cloexec(descriptor)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_credential_fd_assignments(
    assignments: &[i32],
    count: usize,
) -> Result<std::collections::BTreeSet<i32>, String> {
    let configured = assignments
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    if configured.len() != assignments.len() {
        return Err("credential descriptors must have one cross-role assignment".to_owned());
    }
    let expected = (0..count)
        .map(|offset| {
            i32::try_from(3usize.saturating_add(offset))
                .map_err(|_| "credential descriptor range is invalid".to_owned())
        })
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    if configured != expected {
        return Err(
            "config must use every preserved credential descriptor exactly once".to_owned(),
        );
    }
    Ok(expected)
}

#[cfg(target_os = "linux")]
fn mark_fd_cloexec(descriptor: i32) -> Result<(), String> {
    // SAFETY: fcntl only inspects and updates the designated inherited FD.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
    {
        return Err("cannot isolate inherited credential descriptor".to_owned());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn preserve_fds_for_exec(command: &mut std::process::Command, descriptors: Vec<i32>) {
    use std::os::unix::process::CommandExt as _;

    // SAFETY: the closure uses only async-signal-safe fcntl calls before exec.
    unsafe {
        command.pre_exec(move || {
            for descriptor in &descriptors {
                let flags = libc::fcntl(*descriptor, libc::F_GETFD);
                if flags < 0
                    || libc::fcntl(*descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

#[cfg(target_os = "linux")]
fn self_test_fd_isolation() -> Result<(), String> {
    use std::os::fd::AsRawFd as _;

    let first =
        std::fs::File::open("/dev/null").map_err(|_| "cannot open first FD fixture".to_owned())?;
    let second =
        std::fs::File::open("/dev/null").map_err(|_| "cannot open second FD fixture".to_owned())?;
    mark_fd_cloexec(first.as_raw_fd())?;
    mark_fd_cloexec(second.as_raw_fd())?;
    let executable =
        std::env::current_exe().map_err(|_| "cannot resolve FD fixture executable".to_owned())?;
    let args = [
        "fd-probe".to_owned(),
        first.as_raw_fd().to_string(),
        second.as_raw_fd().to_string(),
    ];
    let closed = std::process::Command::new(&executable)
        .args(&args)
        .env_clear()
        .status()
        .map_err(|_| "cannot run closed-FD fixture".to_owned())?;
    if closed.success() {
        return Err("unrelated child inherited credential descriptors".to_owned());
    }
    let mut designated = std::process::Command::new(&executable);
    designated.args(&args).env_clear();
    preserve_fds_for_exec(&mut designated, vec![first.as_raw_fd()]);
    let status = designated
        .status()
        .map_err(|_| "cannot run designated-FD fixture".to_owned())?;
    if !status.success() {
        return Err("designated bootstrap did not receive its exact descriptor".to_owned());
    }
    println!("benchmark-supervisor-v1 fd-isolation-valid");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn self_test_fd_isolation() -> Result<(), String> {
    Err("credential FD isolation requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn fd_probe(open: &str, closed: &str) -> Result<(), String> {
    let open = open
        .parse::<i32>()
        .map_err(|_| "FD probe is invalid".to_owned())?;
    let closed = closed
        .parse::<i32>()
        .map_err(|_| "FD probe is invalid".to_owned())?;
    // SAFETY: F_GETFD only inspects the two fixture descriptors.
    if unsafe { libc::fcntl(open, libc::F_GETFD) } < 0
        || unsafe { libc::fcntl(closed, libc::F_GETFD) } >= 0
    {
        return Err("FD probe observed the wrong descriptor set".to_owned());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn fd_probe(_open: &str, _closed: &str) -> Result<(), String> {
    Err("credential FD isolation requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn validate_open_fd(descriptor: i32) -> Result<(), String> {
    // SAFETY: F_GETFD only inspects the caller-provided descriptor.
    if unsafe { libc::fcntl(descriptor, libc::F_GETFD) } < 0 {
        return Err("preserved credential descriptor is not open".to_owned());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn probe_fds(
    count: &str,
    namespace: &str,
    expected_uid: &str,
    expected_gid: &str,
    expected_unit: &str,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;

    let count = count
        .parse::<usize>()
        .map_err(|_| "credential probe count is invalid".to_owned())?;
    if count == 0 || count > 8 {
        return Err("credential probe count is outside its bound".to_owned());
    }
    for offset in 0..count {
        let descriptor = i32::try_from(3usize.saturating_add(offset))
            .map_err(|_| "credential probe descriptor is invalid".to_owned())?;
        validate_open_fd(descriptor)?;
    }
    let expected_uid = expected_uid
        .parse::<u32>()
        .map_err(|_| "credential probe uid is invalid".to_owned())?;
    let expected_gid = expected_gid
        .parse::<u32>()
        .map_err(|_| "credential probe gid is invalid".to_owned())?;
    // SAFETY: these libc calls only read the process credentials.
    if unsafe { libc::geteuid() } != expected_uid || unsafe { libc::getegid() } != expected_gid {
        return Err("credential probe retained privileged credentials".to_owned());
    }
    let current_namespace = std::fs::metadata("/proc/self/ns/net")
        .map_err(|_| "cannot inspect credential probe namespace".to_owned())?;
    let expected_namespace = std::fs::metadata(format!("/run/netns/{namespace}"))
        .map_err(|_| "cannot inspect expected credential probe namespace".to_owned())?;
    if current_namespace.dev() != expected_namespace.dev()
        || current_namespace.ino() != expected_namespace.ino()
    {
        return Err("credential probe entered the wrong network namespace".to_owned());
    }
    let cgroup = std::fs::read_to_string("/proc/self/cgroup")
        .map_err(|_| "cannot inspect credential probe cgroup".to_owned())?;
    if expected_unit.is_empty()
        || !expected_unit
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || !cgroup.lines().any(|line| line.contains(expected_unit))
    {
        return Err("credential probe escaped the protected service cgroup".to_owned());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn probe_fds(
    _count: &str,
    _namespace: &str,
    _expected_uid: &str,
    _expected_gid: &str,
    _expected_unit: &str,
) -> Result<(), String> {
    Err("credential descriptors require the protected Linux supervisor".to_owned())
}

#[cfg(target_os = "linux")]
fn validate_arm_namespaces(
    local: &str,
    upstream: &str,
    credential_count: usize,
    identity: &ValidatedLaunchIdentity,
) -> Result<(), String> {
    if local == upstream {
        return Err("benchmark arms require distinct network namespaces".to_owned());
    }
    for (name, environment_name) in [
        (local, "ANY_MCP_BENCHMARK_LOCAL_NETNS"),
        (upstream, "ANY_MCP_BENCHMARK_UPSTREAM_NETNS"),
    ] {
        if name.is_empty()
            || name.len() > 96
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            || std::env::var(environment_name).as_deref() != Ok(name)
        {
            return Err("arm network namespace identity is invalid".to_owned());
        }
    }
    let local_metadata = std::fs::metadata(format!("/run/netns/{local}"))
        .map_err(|_| "local arm network namespace is unavailable".to_owned())?;
    let upstream_metadata = std::fs::metadata(format!("/run/netns/{upstream}"))
        .map_err(|_| "upstream arm network namespace is unavailable".to_owned())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if local_metadata.dev() == upstream_metadata.dev()
            && local_metadata.ino() == upstream_metadata.ino()
        {
            return Err("benchmark arms resolved to the same network namespace".to_owned());
        }
    }
    let sudo = required_absolute_env("ANY_MCP_BENCHMARK_SUDO")?;
    let ip = required_absolute_env("ANY_MCP_BENCHMARK_IP")?;
    let setpriv = required_absolute_env("ANY_MCP_BENCHMARK_SETPRIV")?;
    let unit = std::env::var("ANY_MCP_BENCHMARK_UNIT")
        .map_err(|_| "protected service identity is absent".to_owned())?;
    let nonce = std::env::var("ANY_MCP_BENCHMARK_RUN_NONCE")
        .map_err(|_| "protected run nonce is absent".to_owned())?;
    if unit != format!("any-mcp-benchmark-{nonce}.service") {
        return Err("protected service identity is invalid".to_owned());
    }
    let close_from = 3usize.saturating_add(credential_count).to_string();
    for name in [local, upstream] {
        let status = run_namespace_probe(NamespaceProbe {
            sudo: &sudo,
            ip: &ip,
            setpriv: &setpriv,
            executable: &identity.bootstrap,
            namespace: name,
            credential_count,
            close_from: &close_from,
            service_uid: identity.service_uid,
            service_gid: identity.service_gid,
            unit: &unit,
        })?;
        if !status.success() {
            return Err("credential descriptors did not survive the namespace boundary".to_owned());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct NamespaceProbe<'a> {
    sudo: &'a Path,
    ip: &'a Path,
    setpriv: &'a Path,
    executable: &'a Path,
    namespace: &'a str,
    credential_count: usize,
    close_from: &'a str,
    service_uid: u32,
    service_gid: u32,
    unit: &'a str,
}

#[cfg(target_os = "linux")]
fn run_namespace_probe(probe: NamespaceProbe<'_>) -> Result<std::process::ExitStatus, String> {
    use std::os::unix::process::CommandExt as _;

    let mut command = std::process::Command::new(probe.sudo);
    command
        .env_clear()
        .args(["-n", "-C", probe.close_from, "--"])
        .arg(probe.ip)
        .args(["netns", "exec", probe.namespace])
        .arg(probe.setpriv)
        .args([
            &format!("--reuid={}", probe.service_uid),
            &format!("--regid={}", probe.service_gid),
            "--clear-groups",
        ])
        .arg(probe.executable)
        .args([
            "probe-fds",
            &probe.credential_count.to_string(),
            probe.namespace,
            &probe.service_uid.to_string(),
            &probe.service_gid.to_string(),
            probe.unit,
        ]);
    let descriptors = (3..3i32.saturating_add(i32::try_from(probe.credential_count).unwrap_or(0)))
        .collect::<Vec<_>>();
    // SAFETY: the closure uses only async-signal-safe fcntl calls before exec.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            for descriptor in &descriptors {
                let flags = libc::fcntl(*descriptor, libc::F_GETFD);
                if flags < 0
                    || libc::fcntl(*descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| "cannot execute network-namespace credential probe".to_owned())?;
    let process_group = i32::try_from(child.id())
        .map_err(|_| "namespace probe pid is outside the supported range".to_owned())?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(_) => {
                terminate_probe_tree(&mut child, process_group)?;
                return Err("cannot poll network-namespace credential probe".to_owned());
            }
        };
        if let Some(status) = status {
            if probe_process_group_exists(process_group)? {
                terminate_probe_tree(&mut child, process_group)?;
                return Err("network-namespace probe left an owned descendant".to_owned());
            }
            return Ok(status);
        }
        if std::time::Instant::now() >= deadline {
            terminate_probe_tree(&mut child, process_group)?;
            return Err("network-namespace credential probe exceeded its deadline".to_owned());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn terminate_probe_tree(child: &mut std::process::Child, process_group: i32) -> Result<(), String> {
    // SAFETY: the process group was created by this supervisor immediately
    // before exec and is identified by the owned child PID.
    let killed = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if killed != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Err("cannot terminate network-namespace probe tree".to_owned());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut reaped = false;
    let mut poll_failed = false;
    loop {
        if !reaped {
            match child.try_wait() {
                Ok(status) => reaped = status.is_some(),
                Err(_) => {
                    poll_failed = true;
                    reaped = true;
                }
            }
        }
        if reaped && !probe_process_group_exists(process_group)? {
            return if poll_failed {
                Err("cannot reap network-namespace credential probe".to_owned())
            } else {
                Ok(())
            };
        }
        if std::time::Instant::now() >= deadline {
            return Err("network-namespace probe tree survived teardown".to_owned());
        }
        // SAFETY: repeated SIGKILL handles descendants racing with teardown.
        let _ = unsafe { libc::kill(-process_group, libc::SIGKILL) };
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn probe_process_group_exists(process_group: i32) -> Result<bool, String> {
    // SAFETY: signal zero only inspects the owned probe process group.
    let result = unsafe { libc::kill(-process_group, 0) };
    classify_probe_group_signal(result, std::io::Error::last_os_error().raw_os_error())
}

#[cfg(target_os = "linux")]
fn classify_probe_group_signal(result: i32, error: Option<i32>) -> Result<bool, String> {
    if result == 0 || error == Some(libc::EPERM) {
        return Ok(true);
    }
    if error == Some(libc::ESRCH) {
        return Ok(false);
    }
    Err("cannot inspect network-namespace probe process group".to_owned())
}

#[cfg(target_os = "linux")]
fn self_test_sudo_argv(executable: &str) -> Result<(), String> {
    let executable = Path::new(executable);
    let status = run_namespace_probe(NamespaceProbe {
        sudo: executable,
        ip: Path::new("/fixture/ip"),
        setpriv: executable,
        executable,
        namespace: "fixture-ns",
        credential_count: 0,
        close_from: "3",
        // SAFETY: the test only reads the current process credentials.
        service_uid: unsafe { libc::geteuid() },
        // SAFETY: the test only reads the current process credentials.
        service_gid: unsafe { libc::getegid() },
        unit: "fixture.service",
    })?;
    if !status.success() {
        return Err("fake sudo rejected the namespace-probe argv".to_owned());
    }
    let stalled = run_namespace_probe(NamespaceProbe {
        sudo: executable,
        ip: Path::new("/fixture/ip"),
        setpriv: executable,
        executable,
        namespace: "stall-ns",
        credential_count: 0,
        close_from: "3",
        // SAFETY: the test only reads the current process credentials.
        service_uid: unsafe { libc::geteuid() },
        // SAFETY: the test only reads the current process credentials.
        service_gid: unsafe { libc::getegid() },
        unit: "fixture.service",
    });
    if stalled.is_ok() {
        return Err("stalled namespace probe escaped its deadline".to_owned());
    }
    println!("benchmark-supervisor-v1 sudo-argv-valid");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn self_test_sudo_argv(_executable: &str) -> Result<(), String> {
    Err("namespace probes require Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn fake_sudo_assert(arguments: &[String]) -> Result<(), String> {
    let current = std::env::current_exe()
        .map_err(|_| "fake sudo cannot resolve its executable".to_owned())?;
    // SAFETY: the fixture only reads the current process credentials.
    let uid = unsafe { libc::geteuid() }.to_string();
    // SAFETY: the fixture only reads the current process credentials.
    let gid = unsafe { libc::getegid() }.to_string();
    if arguments.get(1).map(String::as_str) != Some("-C")
        || arguments.get(2).map(String::as_str) != Some("3")
        || arguments.get(3).map(String::as_str) != Some("--")
        || arguments.get(4).map(String::as_str) != Some("/fixture/ip")
        || arguments.get(5).map(String::as_str) != Some("netns")
        || arguments.get(6).map(String::as_str) != Some("exec")
        || arguments.get(7).map(String::as_str) != Some("fixture-ns")
        || arguments.get(8).map(Path::new) != Some(current.as_path())
        || arguments.get(9) != Some(&format!("--reuid={uid}"))
        || arguments.get(10) != Some(&format!("--regid={gid}"))
        || arguments.get(11).map(String::as_str) != Some("--clear-groups")
        || arguments.get(12).map(Path::new) != Some(current.as_path())
        || arguments.get(14).map(String::as_str) != Some("0")
        || arguments.get(15).map(String::as_str) != Some("fixture-ns")
        || arguments.get(16) != Some(&uid)
        || arguments.get(17) != Some(&gid)
        || arguments.get(18).map(String::as_str) != Some("fixture.service")
    {
        return Err("fake sudo observed an invalid namespace-probe argv".to_owned());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn fake_sudo_assert(_arguments: &[String]) -> Result<(), String> {
    Err("namespace probes require Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn fake_sudo_stall() -> Result<(), String> {
    // SAFETY: both fixture processes perform no allocation or lock-taking
    // after fork; the supervisor owns and tears down their process group.
    unsafe {
        let child = libc::fork();
        if child < 0 {
            return Err("cannot create stalled namespace-probe descendant".to_owned());
        }
        loop {
            libc::pause();
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn fake_sudo_stall() -> Result<(), String> {
    Err("namespace probes require Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn fake_sudo_host(arguments: &[String]) -> Result<(), String> {
    let current = std::env::current_exe()
        .map_err(|_| "fake sudo cannot resolve its executable".to_owned())?;
    let expected_uid = format!("--reuid={}", unsafe { libc::geteuid() });
    let expected_gid = format!("--regid={}", unsafe { libc::getegid() });
    if arguments.len() < 16
        || arguments.first().map(String::as_str) != Some("-n")
        || arguments.get(1).map(String::as_str) != Some("-C")
        || arguments.get(2).map(String::as_str) != Some("5")
        || arguments.get(3).map(String::as_str) != Some("--")
        || arguments.get(4).map(String::as_str) != Some("/fixture/ip")
        || arguments.get(5).map(String::as_str) != Some("netns")
        || arguments.get(6).map(String::as_str) != Some("exec")
        || arguments.get(7).map(String::as_str) != Some("fixture-ns")
        || arguments.get(8).map(Path::new) != Some(current.as_path())
        || arguments.get(9) != Some(&expected_uid)
        || arguments.get(10) != Some(&expected_gid)
        || arguments.get(11).map(String::as_str) != Some("--clear-groups")
        || arguments.get(12).map(Path::new) != Some(current.as_path())
        || arguments.get(13).map(String::as_str) != Some("credential-exec")
    {
        return Err("fake sudo observed an invalid protected-host argv".to_owned());
    }
    credential_exec(&arguments[14..])
}

#[cfg(not(target_os = "linux"))]
fn fake_sudo_host(_arguments: &[String]) -> Result<(), String> {
    Err("protected host namespaces require Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn credential_exec(arguments: &[String]) -> Result<(), String> {
    use std::os::unix::process::CommandExt as _;

    let parsed = BootstrapArguments::parse(arguments)?;
    let mut raw = Vec::with_capacity(parsed.credentials.len());
    for (_, descriptor) in &parsed.credentials {
        raw.push(SecretSet::read_fd(*descriptor)?);
    }
    let secrets = SecretSet::from_values(raw)?;
    secrets.reject_public_values(parsed.public_environment.values().map(String::as_str))?;
    let mut credential_environment = Vec::with_capacity(parsed.credentials.len());
    for ((name, _), index) in parsed.credentials.iter().zip(0..) {
        let value = std::str::from_utf8(secrets.value(index)?)
            .map_err(|_| "bootstrap credential must be valid UTF-8".to_owned())?
            .to_owned();
        credential_environment.push((name, Zeroizing::new(value)));
    }
    for descriptor in 3..parsed.close_from {
        // SAFETY: the bounded descriptor range came from the protected
        // launcher. Invalid or already closed descriptors are ignored.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        if flags >= 0 {
            // SAFETY: F_SETFD only marks the inspected descriptor close-on-exec.
            if unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
                return Err("cannot close bootstrap descriptor at exec".to_owned());
            }
        }
    }
    let target = PinnedArtifact::open_verified(&parsed.target, &parsed.target_sha256)?;
    let target_path = target.exec_path()?;
    let mut target_arguments = parsed.target_arguments;
    let mut pinned_arguments = Vec::new();
    for (index, digest) in parsed.pinned_arguments {
        let path = target_arguments
            .get(index)
            .ok_or_else(|| "pinned target argument index is invalid".to_owned())?;
        let pin = PinnedArtifact::open_verified(path, &digest)?;
        let exec_path = pin.exec_path()?;
        target_arguments[index] = exec_path;
        pinned_arguments.push(pin);
    }
    let mut command = std::process::Command::new(target_path);
    command
        .env_clear()
        .envs(&parsed.public_environment)
        .args(target_arguments);
    // The protected parent command carries only descriptor numbers and
    // allowlisted names. This disposable bootstrap materializes the target's
    // required environment immediately before exec; failure drops Zeroizing
    // values, while success replaces the bootstrap process.
    for (name, value) in &credential_environment {
        command.env(name, value.as_str());
    }
    let error = command.exec();
    drop(pinned_arguments);
    drop(target);
    Err(format!("credential bootstrap exec failed: {error}"))
}

#[cfg(not(target_os = "linux"))]
fn credential_exec(_arguments: &[String]) -> Result<(), String> {
    Err("credential bootstrap requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
struct BootstrapArguments {
    close_from: i32,
    target_sha256: String,
    public_environment: std::collections::BTreeMap<String, String>,
    credentials: Vec<(String, i32)>,
    pinned_arguments: Vec<(usize, String)>,
    target: String,
    target_arguments: Vec<String>,
}

#[cfg(target_os = "linux")]
impl BootstrapArguments {
    fn parse(arguments: &[String]) -> Result<Self, String> {
        const PUBLIC: &[&str] = &[
            "ANYTYPE_API_BASE_URL",
            "ANYTYPE_KEYSTORE",
            "ANYTYPE_KEYSTORE_SERVICE",
            "ANY_MCP_PROFILE",
            "ANY_MCP_TOOLSETS",
            "ANY_MCP_READ_ONLY",
            "RUST_LOG",
        ];
        const CREDENTIAL: &[&str] = &[
            "ANYTYPE_API_KEY",
            "ANYTYPE_KEY_HTTP_TOKEN",
            "ANYTYPE_KEY_ACCOUNT_ID",
            "ANYTYPE_KEY_ACCOUNT_KEY",
            "ANYTYPE_KEY_SESSION_TOKEN",
        ];
        let mut close_from = None;
        let mut target_sha256 = None;
        let mut public_environment = std::collections::BTreeMap::new();
        let mut credentials = Vec::new();
        let mut credential_names = std::collections::BTreeSet::new();
        let mut credential_fds = std::collections::BTreeSet::new();
        let mut pinned_arguments = Vec::new();
        let mut pinned_indices = std::collections::BTreeSet::new();
        let mut index = 0usize;
        while index < arguments.len() && arguments[index] != "--" {
            let option = arguments[index].as_str();
            match option {
                "--close-from" | "--target-sha256" => {
                    let value = arguments
                        .get(index + 1)
                        .ok_or_else(|| "bootstrap option is incomplete".to_owned())?;
                    if option == "--close-from" {
                        let parsed = value
                            .parse::<i32>()
                            .map_err(|_| "bootstrap close boundary is invalid".to_owned())?;
                        if close_from.replace(parsed).is_some() {
                            return Err("bootstrap close boundary is duplicated".to_owned());
                        }
                    } else if target_sha256.replace(value.clone()).is_some() {
                        return Err("bootstrap target digest is duplicated".to_owned());
                    }
                    index = index.saturating_add(2);
                }
                "--public-env" | "--credential-fd" | "--pinned-arg" => {
                    let first = arguments
                        .get(index + 1)
                        .ok_or_else(|| "bootstrap mapping is incomplete".to_owned())?;
                    let second = arguments
                        .get(index + 2)
                        .ok_or_else(|| "bootstrap mapping is incomplete".to_owned())?;
                    match option {
                        "--public-env" => {
                            if !PUBLIC.contains(&first.as_str())
                                || public_environment
                                    .insert(first.clone(), second.clone())
                                    .is_some()
                            {
                                return Err("bootstrap public environment is invalid".to_owned());
                            }
                        }
                        "--credential-fd" => {
                            let descriptor = second.parse::<i32>().map_err(|_| {
                                "bootstrap credential descriptor is invalid".to_owned()
                            })?;
                            if !CREDENTIAL.contains(&first.as_str())
                                || !credential_names.insert(first.clone())
                                || !credential_fds.insert(descriptor)
                            {
                                return Err("bootstrap credential mapping is invalid".to_owned());
                            }
                            credentials.push((first.clone(), descriptor));
                        }
                        "--pinned-arg" => {
                            let argument_index = first.parse::<usize>().map_err(|_| {
                                "bootstrap pinned argument index is invalid".to_owned()
                            })?;
                            if !pinned_indices.insert(argument_index) {
                                return Err("bootstrap pinned argument is duplicated".to_owned());
                            }
                            pinned_arguments.push((argument_index, second.clone()));
                        }
                        _ => return Err("bootstrap mapping option is invalid".to_owned()),
                    }
                    index = index.saturating_add(3);
                }
                _ => return Err("bootstrap contains an unknown option".to_owned()),
            }
        }
        let close_from = close_from
            .filter(|value| (4..=11).contains(value))
            .ok_or_else(|| "bootstrap close boundary is absent or invalid".to_owned())?;
        let target_sha256 = target_sha256
            .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| "bootstrap target digest is absent or invalid".to_owned())?;
        if credentials.iter().any(|(name, descriptor)| {
            *descriptor < 3 || *descriptor >= close_from || public_environment.contains_key(name)
        }) {
            return Err("bootstrap credential mapping escaped its bounds".to_owned());
        }
        if pinned_arguments.len() > 4 || public_environment.len() > 8 || credentials.len() > 4 {
            return Err("bootstrap mappings exceed their bounds".to_owned());
        }
        let target = arguments
            .get(index.saturating_add(1))
            .filter(|value| Path::new(value).is_absolute())
            .cloned()
            .ok_or_else(|| "bootstrap target is absent or invalid".to_owned())?;
        let target_arguments = arguments
            .get(index.saturating_add(2)..)
            .ok_or_else(|| "bootstrap target arguments are invalid".to_owned())?
            .to_vec();
        if arguments.get(index).map(String::as_str) != Some("--")
            || target_arguments.len() > 16
            || pinned_arguments.iter().any(|(index, digest)| {
                *index >= target_arguments.len()
                    || digest.len() != 64
                    || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err("bootstrap target envelope is invalid".to_owned());
        }
        Ok(Self {
            close_from,
            target_sha256,
            public_environment,
            credentials,
            pinned_arguments,
            target,
            target_arguments,
        })
    }
}

#[cfg(target_os = "linux")]
fn required_absolute_env(name: &str) -> Result<std::path::PathBuf, String> {
    let value = std::env::var_os(name)
        .ok_or_else(|| "protected supervisor executable is absent".to_owned())?;
    let path = std::path::PathBuf::from(value);
    if !path.is_absolute() || !path.is_file() {
        return Err("protected supervisor executable is invalid".to_owned());
    }
    Ok(path)
}

#[cfg(target_os = "linux")]
fn required_numeric_env(name: &str) -> Result<u32, String> {
    std::env::var(name)
        .map_err(|_| "protected service credential is absent".to_owned())?
        .parse::<u32>()
        .map_err(|_| "protected service credential is invalid".to_owned())
}

#[cfg(target_os = "linux")]
fn live_preflight(run_root_path: &Path, config_path: &Path) -> Result<(), String> {
    let config = config::Config::read(config_path)?;
    let run_root = run_root::ProtectedRunRoot::open(run_root_path)?;
    let _artifact_pins = verify_artifacts(&config)?;
    let _secrets = SecretSet::from_values(vec![SecretSet::read_fd(config.oracle.credential_fd)?])?;
    run_root.cleanup_arm_files()?;
    let mut output = run_root.create_result(&config.result_file)?;
    let event = serde_json::json!({
        "schema_version": 1,
        "event": "blocked",
        "run_id": config.run_id,
        "mode": config.mode,
        "track": config.track,
        "reason": "measurement execution requires the integrated deadline revision and operator-approved immutable runtime inputs",
        "run_root": run_root.path().file_name().and_then(|name| name.to_str()).unwrap_or("protected")
    });
    run_root::ProtectedRunRoot::append_json(&mut output, &event)?;
    Err(
        "live measurement execution is blocked pending integrated deadlines and operator inputs"
            .to_owned(),
    )
}

#[cfg(target_os = "linux")]
fn verify_artifacts(config: &config::Config) -> Result<Vec<PinnedArtifact>, String> {
    let mut pins = vec![PinnedArtifact::open_verified(
        &config.local.executable,
        &config.local.executable_sha256,
    )?];
    pins.push(PinnedArtifact::open_verified(
        &config.upstream.executable,
        &config.upstream.executable_sha256,
    )?);
    pins.push(PinnedArtifact::open_verified(
        &config.manifest.anytype_cli_path,
        &config.manifest.anytype_cli_sha256,
    )?);
    pins.push(PinnedArtifact::open_verified(
        &config.manifest.heart_path,
        &config.manifest.heart_sha256,
    )?);
    match &config.upstream.artifact {
        ServerArtifact::OfficialNpm {
            integrity,
            tarball_path,
            tarball_sha256,
            tarball_bundle_entry,
            bundle_path,
            bundle_sha256,
            ..
        } => {
            let tarball = PinnedArtifact::open_verified(tarball_path, tarball_sha256)?;
            let bundle = PinnedArtifact::open_verified(bundle_path, bundle_sha256)?;
            verify_npm_sri_pinned(&tarball, integrity)?;
            verify_tarball_bundle_pinned(&tarball, tarball_bundle_entry, bundle_sha256)?;
            tarball.revalidate_path_identity()?;
            bundle.revalidate_path_identity()?;
            pins.push(tarball);
            pins.push(bundle);
        }
        ServerArtifact::Local { .. } => {
            return Err("upstream comparator lacks its official npm attestation".to_owned());
        }
    }
    if let Some(spec) = &config.controlled_spec {
        pins.push(PinnedArtifact::open_verified(&spec.path, &spec.sha256)?);
    }
    for pin in &pins {
        pin.revalidate_path_identity()?;
    }
    verify_ancestry(&config.ancestry)?;
    Ok(pins)
}

#[cfg(test)]
fn verify_file_hash(path: &str, expected: &str) -> Result<(), String> {
    let _pin = PinnedArtifact::open_verified(path, expected)?;
    Ok(())
}

struct PinnedArtifact {
    path: std::path::PathBuf,
    file: std::fs::File,
    metadata: std::fs::Metadata,
}

impl PinnedArtifact {
    fn open_verified(path: &str, expected: &str) -> Result<Self, String> {
        let mut pin = Self::open(path)?;
        let actual = pin.sha256()?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err("attested artifact digest does not match".to_owned());
        }
        Ok(pin)
    }

    fn open(path: &str) -> Result<Self, String> {
        let path = Path::new(path);
        if !path.is_absolute() {
            return Err("attested artifact path must be absolute".to_owned());
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        let file = options
            .open(path)
            .map_err(|_| "cannot open attested artifact".to_owned())?;
        let metadata = file
            .metadata()
            .map_err(|_| "cannot inspect opened artifact".to_owned())?;
        if !metadata.is_file() {
            return Err("attested artifact must be a regular file".to_owned());
        }
        Ok(Self {
            path: path.to_owned(),
            file,
            metadata,
        })
    }

    fn sha256(&mut self) -> Result<String, String> {
        use sha2::Digest as _;
        use std::io::{Read as _, Seek as _};

        self.file
            .rewind()
            .map_err(|_| "cannot rewind attested artifact".to_owned())?;
        let mut digest = sha2::Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = self
                .file
                .read(&mut buffer)
                .map_err(|_| "cannot read attested artifact".to_owned())?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        self.file
            .rewind()
            .map_err(|_| "cannot rewind attested artifact".to_owned())?;
        Ok(hex_digest(&digest.finalize()))
    }

    fn reader(&self) -> Result<std::fs::File, String> {
        use std::io::{Seek as _, SeekFrom};

        let mut reader = self
            .file
            .try_clone()
            .map_err(|_| "cannot duplicate pinned artifact".to_owned())?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|_| "cannot rewind pinned artifact".to_owned())?;
        Ok(reader)
    }

    fn revalidate_path_identity(&self) -> Result<(), String> {
        let reopened = Self::open(
            self.path
                .to_str()
                .ok_or_else(|| "attested artifact path is not UTF-8".to_owned())?,
        )?;
        if !same_file_identity(&self.metadata, &reopened.metadata) {
            return Err("attested artifact path changed after verification".to_owned());
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn exec_path(&self) -> Result<String, String> {
        use std::os::fd::AsRawFd as _;

        self.revalidate_path_identity()?;
        let descriptor = self.file.as_raw_fd();
        // SAFETY: F_GETFD and F_SETFD only inspect or update the live pinned
        // descriptor. It must survive the immediately following exec.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        if flags < 0
            || unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0
        {
            return Err("cannot preserve pinned artifact through exec".to_owned());
        }
        Ok(format!("/proc/self/fd/{descriptor}"))
    }
}

#[cfg(test)]
fn verify_npm_sri(path: &str, integrity: &str) -> Result<(), String> {
    verify_npm_sri_pinned(&PinnedArtifact::open(path)?, integrity)
}

fn verify_npm_sri_pinned(pin: &PinnedArtifact, integrity: &str) -> Result<(), String> {
    use base64::Engine as _;
    use sha2::Digest as _;
    use std::io::Read as _;

    let encoded = integrity
        .strip_prefix("sha512-")
        .ok_or_else(|| "npm integrity is not sha512 SRI".to_owned())?;
    let expected = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "npm integrity contains invalid base64".to_owned())?;
    if expected.len() != 64 {
        return Err("npm integrity has the wrong digest length".to_owned());
    }
    let mut file = pin.reader()?;
    let mut digest = sha2::Sha512::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "cannot read npm tarball for SRI verification".to_owned())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    if digest.finalize().as_slice() != expected {
        return Err("npm tarball does not match its sha512 SRI".to_owned());
    }
    Ok(())
}

#[cfg(test)]
fn verify_tarball_bundle(
    tarball_path: &str,
    target_entry: &str,
    expected_sha256: &str,
) -> Result<(), String> {
    verify_tarball_bundle_pinned(
        &PinnedArtifact::open(tarball_path)?,
        target_entry,
        expected_sha256,
    )
}

fn verify_tarball_bundle_pinned(
    tarball: &PinnedArtifact,
    target_entry: &str,
    expected_sha256: &str,
) -> Result<(), String> {
    use sha2::Digest as _;
    use std::io::Read as _;

    const BLOCK: usize = 512;
    const MAX_EXPANDED: u64 = 256 * 1024 * 1024;
    let file = tarball.reader()?;
    let mut archive = benchmark_flate2::read::GzDecoder::new(file);
    let mut header = [0u8; BLOCK];
    let mut expanded = 0u64;
    let mut observed = None;
    loop {
        match archive.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => return Err("cannot decode npm tarball".to_owned()),
        }
        expanded = expanded.saturating_add(BLOCK as u64);
        if expanded > MAX_EXPANDED {
            return Err("expanded npm tarball exceeds its bound".to_owned());
        }
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let name = tar_name(&header)?;
        let size = tar_octal(&header[124..136])?;
        let regular = matches!(header[156], 0 | b'0');
        let blocks = size.saturating_add((BLOCK - 1) as u64) / BLOCK as u64;
        let padded = blocks.saturating_mul(BLOCK as u64);
        expanded = expanded.saturating_add(padded);
        if expanded > MAX_EXPANDED {
            return Err("expanded npm tarball exceeds its bound".to_owned());
        }
        if name == target_entry {
            if !regular || observed.is_some() {
                return Err("npm bundle provenance entry is not one unique regular file".to_owned());
            }
            let mut digest = sha2::Sha256::new();
            let mut remaining = size;
            let mut buffer = [0u8; 64 * 1024];
            while remaining > 0 {
                let take = usize::try_from(remaining.min(buffer.len() as u64))
                    .map_err(|_| "npm bundle entry size is invalid".to_owned())?;
                archive
                    .read_exact(&mut buffer[..take])
                    .map_err(|_| "npm bundle entry is truncated".to_owned())?;
                digest.update(&buffer[..take]);
                remaining = remaining.saturating_sub(take as u64);
            }
            observed = Some(hex_digest(&digest.finalize()));
            let padding = padded.saturating_sub(size);
            let skipped = std::io::copy(
                &mut std::io::Read::by_ref(&mut archive).take(padding),
                &mut std::io::sink(),
            )
            .map_err(|_| "cannot skip npm bundle padding".to_owned())?;
            if skipped != padding {
                return Err("npm bundle padding is truncated".to_owned());
            }
        } else {
            let skipped = std::io::copy(
                &mut std::io::Read::by_ref(&mut archive).take(padded),
                &mut std::io::sink(),
            )
            .map_err(|_| "cannot skip npm tarball entry".to_owned())?;
            if skipped != padded {
                return Err("npm tarball entry is truncated".to_owned());
            }
        }
    }
    let observed = observed.ok_or_else(|| "npm tarball does not contain its bundle".to_owned())?;
    if !observed.eq_ignore_ascii_case(expected_sha256) {
        return Err("installed bundle is not byte-identical to the npm tarball entry".to_owned());
    }
    Ok(())
}

fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.len() == right.len()
            && left.mode() == right.mode()
            && left.uid() == right.uid()
    }
    #[cfg(not(unix))]
    {
        left.len() == right.len()
            && left.modified().ok() == right.modified().ok()
            && left.permissions().readonly() == right.permissions().readonly()
    }
}

fn tar_name(header: &[u8; 512]) -> Result<String, String> {
    fn field(bytes: &[u8]) -> Result<&str, String> {
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        std::str::from_utf8(&bytes[..end]).map_err(|_| "npm tarball path is not UTF-8".to_owned())
    }
    let name = field(&header[..100])?;
    let prefix = field(&header[345..500])?;
    Ok(if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    })
}

fn tar_octal(bytes: &[u8]) -> Result<u64, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "npm tarball size is invalid".to_owned())?
        .trim_matches(['\0', ' ']);
    u64::from_str_radix(text, 8).map_err(|_| "npm tarball size is invalid".to_owned())
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = std::fmt::Write::write_fmt(&mut result, format_args!("{byte:02x}"));
    }
    result
}

#[cfg(target_os = "linux")]
fn verify_ancestry(ancestry: &config::AncestryAdmission) -> Result<(), String> {
    let repository = Path::new(&ancestry.repository_path)
        .canonicalize()
        .map_err(|_| "cannot canonicalize ancestry repository".to_owned())?;
    if repository != Path::new(&ancestry.repository_path) || !repository.is_dir() {
        return Err("ancestry repository must be one canonical directory".to_owned());
    }
    for ancestor in ancestry.required_ancestors.values() {
        let revset = format!(
            "commit_id({}) & descendants(commit_id({}))",
            ancestry.integrated_revision, ancestor
        );
        let output = bounded_jj_log(&repository, &revset)?;
        if output.trim() != ancestry.integrated_revision {
            return Err("integrated revision lacks a required benchmark carrier".to_owned());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bounded_jj_log(repository: &Path, revset: &str) -> Result<String, String> {
    use std::io::Read as _;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut command = Command::new("/run/current-system/sw/bin/jj");
    command
        .env_clear()
        .args(["-R"])
        .arg(repository)
        .args([
            "log",
            "-r",
            revset,
            "--no-graph",
            "-T",
            "commit_id ++ \"\\n\"",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: setpgid is async-signal-safe between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| "cannot execute ancestry verifier".to_owned())?;
    let pid =
        i32::try_from(child.id()).map_err(|_| "ancestry verifier pid is invalid".to_owned())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| "cannot poll ancestry verifier".to_owned())?
        {
            if !status.success() {
                return Err("ancestry verifier failed".to_owned());
            }
            break;
        }
        if Instant::now() >= deadline {
            // SAFETY: the negative pid targets the verifier's owned group.
            let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
            let _ = child.wait();
            return Err("ancestry verifier exceeded its deadline".to_owned());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| "ancestry verifier stdout is unavailable".to_owned())?
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read ancestry verifier output".to_owned())?;
    if bytes.len() > 4096 {
        return Err("ancestry verifier output exceeds its bound".to_owned());
    }
    String::from_utf8(bytes).map_err(|_| "ancestry verifier output is not UTF-8".to_owned())
}

fn self_test_host(executable: &str) -> Result<(), String> {
    let initialized = self_test_initialized(executable, "ok")?;
    if initialized.startup.is_none() || initialized.catalog.tools_subtree_canonical_bytes == 0 {
        return Err("fake child did not produce startup and catalog measures".to_owned());
    }
    let mut host = initialized.host;
    let (response, measures) = host.call("fixture", serde_json::json!({"value": 1}))?;
    if response.pointer("/result/structuredContent/ok") != Some(&serde_json::Value::Bool(true))
        || measures.interval.first_byte_ns < measures.interval.start_ns
        || measures.interval.end_ns < measures.interval.first_byte_ns
    {
        return Err("fake child produced an invalid measured response".to_owned());
    }
    let _ = host.finish()?;
    println!("benchmark-host-v1 valid");
    Ok(())
}

#[cfg(target_os = "linux")]
fn self_test_host_namespace(executable: &str) -> Result<(), String> {
    let mut server = self_test_server(executable, "ok");
    let mut executable_pin = PinnedArtifact::open(executable)?;
    server.executable_sha256 = executable_pin.sha256()?;
    let secrets = Arc::new(SecretSet::from_values(vec![Zeroizing::new(
        b"scanner-self-test-secret".to_vec(),
    )])?);
    let domain = protocol::LaunchDomain {
        sudo: executable.to_owned(),
        ip: "/fixture/ip".to_owned(),
        setpriv: executable.to_owned(),
        namespace: "fixture-ns".to_owned(),
        bootstrap: executable.to_owned(),
        service_uid: unsafe { libc::geteuid() },
        service_gid: unsafe { libc::getegid() },
        preserved_credential_fds: 2,
    };
    let initialized = JsonRpcHost::spawn_in_namespace_for_test(
        &server,
        secrets,
        &[],
        &domain,
        std::time::Duration::from_secs(3),
        std::time::Duration::from_millis(500),
        std::time::Duration::from_secs(2),
    )?
    .initialize(false)?;
    let _ = initialized.host.finish()?;
    println!("benchmark-host-v1 namespace-argv-valid");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn self_test_host_namespace(_executable: &str) -> Result<(), String> {
    Err("protected host namespaces require Linux".to_owned())
}

fn self_test_reject(executable: &str, mode: &str) -> Result<(), String> {
    if !matches!(
        mode,
        "flood"
            | "deep"
            | "secret"
            | "hang"
            | "malformed"
            | "eof"
            | "id-mismatch"
            | "stderr-flood"
            | "nonzero"
    ) {
        return Err("unknown rejection self-test".to_owned());
    }
    let rejected = match self_test_initialized(executable, mode) {
        Err(_) => true,
        Ok(initialized) if matches!(mode, "secret" | "stderr-flood" | "nonzero") => {
            initialized.host.finish().is_err()
        }
        Ok(_) => false,
    };
    if !rejected {
        return Err("bounded host accepted invalid fake-child output".to_owned());
    }
    println!("benchmark-host-v1 rejected-{mode}");
    Ok(())
}

fn self_test_blocked_stdin(executable: &str) -> Result<(), String> {
    let secrets = Arc::new(SecretSet::from_values(vec![Zeroizing::new(
        b"scanner-self-test-secret".to_vec(),
    )])?);
    let server = self_test_server(executable, "no-read");
    let mut host = JsonRpcHost::spawn_with_deadlines(
        &server,
        secrets,
        &[],
        false,
        std::time::Duration::from_millis(500),
        std::time::Duration::from_millis(500),
        std::time::Duration::from_secs(2),
    )?;
    let large = "x".repeat(1024 * 1024);
    if host
        .call("fixture", serde_json::json!({"large": large}))
        .is_ok()
    {
        return Err("blocked child stdin did not fail before its deadline".to_owned());
    }
    println!("benchmark-host-v1 rejected-blocked-stdin");
    Ok(())
}

fn self_test_descendant(executable: &str) -> Result<(), String> {
    let initialized = self_test_initialized(executable, "descendant")?;
    let mut host = initialized.host;
    let _ = host.call("fixture", serde_json::json!({}))?;
    let _ = host.finish()?;
    println!("benchmark-host-v1 cleaned-descendant");
    Ok(())
}

fn self_test_protection_cleanup(executable: &str) -> Result<(), String> {
    let secrets = Arc::new(SecretSet::from_values(vec![Zeroizing::new(
        b"scanner-self-test-secret".to_vec(),
    )])?);
    let server = self_test_server(executable, "hang");
    if JsonRpcHost::spawn_with_failed_protection_for_test(
        &server,
        secrets,
        &[],
        std::time::Duration::from_millis(500),
        std::time::Duration::from_millis(500),
        std::time::Duration::from_secs(2),
    )
    .is_ok()
    {
        return Err("post-spawn protection failure was not enforced".to_owned());
    }
    println!("benchmark-host-v1 cleaned-protection-failure");
    Ok(())
}

fn self_test_initialized(
    executable: &str,
    mode: &str,
) -> Result<protocol::InitializedHost, String> {
    let secrets = Arc::new(SecretSet::from_values(vec![Zeroizing::new(
        b"scanner-self-test-secret".to_vec(),
    )])?);
    let server = self_test_server(executable, mode);
    JsonRpcHost::spawn_with_deadlines(
        &server,
        secrets,
        &[],
        false,
        std::time::Duration::from_millis(500),
        std::time::Duration::from_millis(500),
        std::time::Duration::from_secs(2),
    )?
    .initialize(false)
}

fn self_test_server(executable: &str, mode: &str) -> Server {
    Server {
        executable: executable.to_owned(),
        arguments: vec!["fake-child".to_owned(), mode.to_owned()],
        executable_sha256: "0".repeat(64),
        environment: Default::default(),
        credentials: Vec::new(),
        artifact: ServerArtifact::Local {
            revision: "self-test".to_owned(),
            features: vec!["benchmark-harness".to_owned()],
        },
    }
}

fn fake_child(mode: &str) -> Result<(), String> {
    if mode == "hang" {
        loop {
            std::thread::park_timeout(std::time::Duration::from_secs(60));
        }
    }
    if mode == "no-read" {
        loop {
            std::thread::park_timeout(std::time::Duration::from_secs(60));
        }
    }
    if mode == "eof" {
        return Ok(());
    }
    if mode == "secret" {
        let encoded = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            b"scanner-self-test-secret",
        );
        let split = encoded.len() / 2;
        let mut stderr = std::io::stderr().lock();
        stderr
            .write_all(&encoded.as_bytes()[..split])
            .and_then(|()| stderr.flush())
            .map_err(|_| "fake child cannot write stderr prefix".to_owned())?;
        std::thread::sleep(std::time::Duration::from_millis(2));
        stderr
            .write_all(&encoded.as_bytes()[split..])
            .and_then(|()| stderr.flush())
            .map_err(|_| "fake child cannot write stderr suffix".to_owned())?;
    }
    if mode == "stderr-flood" {
        let flood = vec![b'e'; 1024 * 1024 + 1];
        std::io::stderr()
            .write_all(&flood)
            .map_err(|_| "fake child cannot write stderr flood".to_owned())?;
    }
    #[cfg(unix)]
    if mode == "descendant" {
        // SAFETY: the child performs no allocation or lock-taking before it
        // parks, and the benchmark owns the complete process group.
        unsafe {
            if libc::fork() == 0 {
                loop {
                    libc::pause();
                }
            }
        }
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|_| "fake child cannot read stdin".to_owned())?;
        let request: serde_json::Value = serde_json::from_str(&line)
            .map_err(|_| "fake child received invalid JSON".to_owned())?;
        let Some(id) = request.get("id") else {
            continue;
        };
        if mode == "flood" {
            let mut flood = vec![b'x'; 2 * 1024 * 1024 + 1];
            flood.push(b'\n');
            stdout
                .write_all(&flood)
                .and_then(|()| stdout.flush())
                .map_err(|_| "fake child cannot write flood".to_owned())?;
            continue;
        }
        if mode == "malformed" {
            stdout
                .write_all(b"not-json\n")
                .and_then(|()| stdout.flush())
                .map_err(|_| "fake child cannot write malformed frame".to_owned())?;
            continue;
        }
        let method = request.get("method").and_then(serde_json::Value::as_str);
        let mut result = match method {
            Some("initialize") => serde_json::json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake", "version": "1"}
            }),
            Some("tools/list") => serde_json::json!({"tools": [{
                "name": "fixture",
                "description": "Deterministic fixture.",
                "inputSchema": {"type": "object", "additionalProperties": false}
            }]}),
            Some("tools/call") => serde_json::json!({
                "content": [{"type": "text", "text": "ok"}],
                "structuredContent": {"ok": true}
            }),
            _ => return Err("fake child received an unexpected method".to_owned()),
        };
        if mode == "deep" {
            for _ in 0..70 {
                result = serde_json::Value::Array(vec![result]);
            }
        }
        let response_id = if mode == "id-mismatch" {
            id.as_u64()
                .map(|value| serde_json::json!(value.saturating_add(1)))
                .unwrap_or_else(|| serde_json::json!("mismatch"))
        } else {
            id.clone()
        };
        let mut encoded = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "result": result
        }))
        .map_err(|_| "fake child cannot encode response".to_owned())?;
        encoded.push(b'\n');
        let split = encoded.len().min(3);
        stdout
            .write_all(&encoded[..split])
            .and_then(|()| stdout.flush())
            .map_err(|_| "fake child cannot write response prefix".to_owned())?;
        std::thread::sleep(std::time::Duration::from_millis(2));
        stdout
            .write_all(&encoded[split..])
            .and_then(|()| stdout.flush())
            .map_err(|_| "fake child cannot write response suffix".to_owned())?;
    }
    if mode == "nonzero" {
        return Err("fake child exits unsuccessfully".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn protected_launch_rejects_root_uid_or_gid() {
        assert!(protocol::validate_non_root_service_identity(0, 1000).is_err());
        assert!(protocol::validate_non_root_service_identity(1000, 0).is_err());
        assert!(protocol::validate_non_root_service_identity(1000, 1000).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn probe_group_permission_denial_is_present_not_empty() {
        assert_eq!(classify_probe_group_signal(-1, Some(libc::EPERM)), Ok(true));
        assert_eq!(
            classify_probe_group_signal(-1, Some(libc::ESRCH)),
            Ok(false)
        );
        assert!(classify_probe_group_signal(-1, Some(libc::EINVAL)).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bootstrap_attestation_precedes_credential_preserving_action() {
        use std::{
            cell::Cell,
            os::unix::fs::{PermissionsExt as _, symlink},
        };

        let root = std::env::temp_dir().join(format!(
            "any-mcp-bootstrap-order-{}-{}",
            std::process::id(),
            getrandom::u64().unwrap_or(0)
        ));
        std::fs::create_dir(&root).expect("create bootstrap-order fixture");
        let mutable = root.join("mutable-bootstrap");
        std::fs::write(&mutable, b"fixture").expect("write mutable bootstrap");
        std::fs::set_permissions(&mutable, std::fs::Permissions::from_mode(0o777))
            .expect("make bootstrap mutable");
        let symlink_path = root.join("symlink-bootstrap");
        symlink(&mutable, &symlink_path).expect("create bootstrap symlink");

        for candidate in [&mutable, &symlink_path] {
            let invoked = Cell::new(false);
            let result = with_validated_launch_identity(candidate, 1000, 1000, |_| {
                invoked.set(true);
                Ok(())
            });
            assert!(result.is_err());
            assert!(!invoked.get(), "unsafe bootstrap reached protected action");
        }
        std::fs::remove_dir_all(root).expect("remove bootstrap-order fixture");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn credential_fd_assignments_are_unique_across_roles() {
        assert!(validate_credential_fd_assignments(&[3, 4, 5], 3).is_ok());
        assert!(validate_credential_fd_assignments(&[3, 4, 4], 3).is_err());
        assert!(validate_credential_fd_assignments(&[3, 4], 3).is_err());
    }

    #[test]
    fn controlled_mode_name_remains_distinct() {
        assert_ne!(
            serde_json::to_string(&config::BenchmarkMode::ProductionVerifiedOfficial)
                .expect("serialize production mode"),
            serde_json::to_string(&config::BenchmarkMode::ControlledFixedSpecWarm)
                .expect("serialize controlled mode")
        );
    }

    #[test]
    fn verifies_npm_sri_and_bundle_provenance() {
        use base64::Engine as _;
        use sha2::Digest as _;

        let bundle = b"#!/usr/bin/env node\nconsole.log('fixture');\n";
        let tarball = build_tgz("package/bin/cli.mjs", bundle);
        let path = std::env::temp_dir().join(format!(
            "any-mcp-benchmark-tarball-{}-{}.tgz",
            std::process::id(),
            getrandom::u64().unwrap_or(0)
        ));
        std::fs::write(&path, &tarball).expect("write tarball fixture");
        let sri = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(&tarball))
        );
        let bundle_sha = hex_digest(&sha2::Sha256::digest(bundle));
        verify_npm_sri(path.to_str().expect("UTF-8 temp path"), &sri).expect("valid SRI");
        verify_tarball_bundle(
            path.to_str().expect("UTF-8 temp path"),
            "package/bin/cli.mjs",
            &bundle_sha,
        )
        .expect("bundle came from tarball");
        assert!(
            verify_npm_sri(
                path.to_str().expect("UTF-8 temp path"),
                &format!("sha512-{}", "A".repeat(88))
            )
            .is_err()
        );
        assert!(
            verify_tarball_bundle(
                path.to_str().expect("UTF-8 temp path"),
                "package/bin/other.mjs",
                &bundle_sha,
            )
            .is_err()
        );
        std::fs::remove_file(path).expect("remove tarball fixture");
    }

    #[test]
    fn attested_file_must_be_a_regular_nonsymlink_with_the_exact_digest() {
        use sha2::Digest as _;

        let suffix = getrandom::u64().unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "any-mcp-benchmark-spec-{}-{suffix}.json",
            std::process::id()
        ));
        let link = std::env::temp_dir().join(format!(
            "any-mcp-benchmark-spec-link-{}-{suffix}.json",
            std::process::id()
        ));
        let contents = br#"{"openapi":"3.0.0","paths":{}}"#;
        std::fs::write(&path, contents).expect("write spec fixture");
        let digest = hex_digest(&sha2::Sha256::digest(contents));
        verify_file_hash(path.to_str().expect("UTF-8 fixture path"), &digest)
            .expect("regular spec fixture is attested");
        assert!(
            verify_file_hash(path.to_str().expect("UTF-8 fixture path"), &"0".repeat(64)).is_err()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&path, &link).expect("create spec symlink");
            assert!(verify_file_hash(link.to_str().expect("UTF-8 link path"), &digest).is_err());
            std::fs::remove_file(&link).expect("remove spec symlink");
        }
        std::fs::remove_file(path).expect("remove spec fixture");
    }

    #[cfg(unix)]
    #[test]
    fn pinned_artifact_rejects_a_path_swap_and_retains_verified_bytes() {
        use sha2::Digest as _;
        use std::io::Read as _;

        let suffix = getrandom::u64().unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "any-mcp-benchmark-pinned-{}-{suffix}",
            std::process::id()
        ));
        let replacement = path.with_extension("replacement");
        let original = b"verified executable bytes";
        std::fs::write(&path, original).expect("write pinned fixture");
        let digest = hex_digest(&sha2::Sha256::digest(original));
        let pin = PinnedArtifact::open_verified(path.to_str().expect("UTF-8 pinned path"), &digest)
            .expect("pin verified artifact");
        std::fs::write(&replacement, b"swapped bytes").expect("write replacement fixture");
        std::fs::rename(&replacement, &path).expect("swap attested path");
        assert!(pin.revalidate_path_identity().is_err());
        let mut retained = Vec::new();
        pin.reader()
            .expect("duplicate pinned artifact")
            .read_to_end(&mut retained)
            .expect("read pinned artifact");
        assert_eq!(retained, original);
        std::fs::remove_file(path).expect("remove swapped fixture");
    }

    fn build_tgz(name: &str, contents: &[u8]) -> Vec<u8> {
        let mut tar = Vec::new();
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        let size = format!("{:011o}\0", contents.len());
        header[124..136].copy_from_slice(size.as_bytes());
        header[156] = b'0';
        tar.extend_from_slice(&header);
        tar.extend_from_slice(contents);
        let padding = (512 - contents.len() % 512) % 512;
        tar.resize(tar.len() + padding, 0);
        tar.resize(tar.len() + 1024, 0);
        let mut encoder = benchmark_flate2::write::GzEncoder::new(
            Vec::new(),
            benchmark_flate2::Compression::default(),
        );
        encoder.write_all(&tar).expect("encode tar fixture");
        encoder.finish().expect("finish tar fixture")
    }
}
