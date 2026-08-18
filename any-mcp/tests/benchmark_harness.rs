// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

use std::process::Command;

use serde_json::json;

const RESULT_SCHEMA: &str = include_str!("schema/benchmark-result-v1.json");

#[test]
fn bounded_host_measures_a_chunked_fake_child() {
    let executable = env!("CARGO_BIN_EXE_any-mcp-benchmark");
    let output = Command::new(executable)
        .arg("self-test-host")
        .arg(executable)
        .output()
        .expect("run benchmark host self-test");

    assert!(
        output.status.success(),
        "self-test failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"benchmark-host-v1 valid\n");
}

#[test]
fn bounded_host_rejects_floods_depth_and_cross_chunk_secrets() {
    let executable = env!("CARGO_BIN_EXE_any-mcp-benchmark");
    for mode in [
        "flood",
        "deep",
        "secret",
        "hang",
        "malformed",
        "eof",
        "id-mismatch",
        "stderr-flood",
        "nonzero",
    ] {
        let output = Command::new(executable)
            .args(["self-test-reject", executable, mode])
            .output()
            .expect("run benchmark rejection self-test");
        assert!(
            output.status.success(),
            "{mode} self-test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout,
            format!("benchmark-host-v1 rejected-{mode}\n").as_bytes()
        );
    }
}

#[test]
fn bounded_host_times_out_blocked_stdin_and_cleans_pipe_descendant() {
    let executable = env!("CARGO_BIN_EXE_any-mcp-benchmark");
    #[cfg(unix)]
    {
        let blocked = Command::new(executable)
            .args(["self-test-blocked-stdin", executable])
            .output()
            .expect("run blocked-stdin self-test");
        assert!(
            blocked.status.success(),
            "blocked-stdin self-test failed: {}",
            String::from_utf8_lossy(&blocked.stderr)
        );
        assert_eq!(
            blocked.stdout,
            b"benchmark-host-v1 rejected-blocked-stdin\n"
        );

        let descendant = Command::new(executable)
            .args(["self-test-descendant", executable])
            .output()
            .expect("run inherited-pipe descendant self-test");
        assert!(
            descendant.status.success(),
            "descendant self-test failed: {}",
            String::from_utf8_lossy(&descendant.stderr)
        );
        assert_eq!(descendant.stdout, b"benchmark-host-v1 cleaned-descendant\n");
    }
}

#[test]
fn post_spawn_protection_failure_reaps_the_child() {
    let executable = env!("CARGO_BIN_EXE_any-mcp-benchmark");
    let output = Command::new(executable)
        .args(["self-test-protection-cleanup", executable])
        .output()
        .expect("run protection cleanup self-test");
    assert!(
        output.status.success(),
        "protection cleanup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"benchmark-host-v1 cleaned-protection-failure\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn namespace_probe_uses_one_exact_fd_preserving_sudo_prefix() {
    let executable = env!("CARGO_BIN_EXE_any-mcp-benchmark");
    let output = Command::new(executable)
        .args(["self-test-sudo-argv", executable])
        .output()
        .expect("run fake-sudo argv assertion");
    assert!(
        output.status.success(),
        "fake-sudo assertion failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"benchmark-supervisor-v1 sudo-argv-valid\n");
}

#[cfg(target_os = "linux")]
#[test]
fn protected_host_uses_exact_namespace_and_fd_bootstrap_topology() {
    let executable = env!("CARGO_BIN_EXE_any-mcp-benchmark");
    let output = Command::new(executable)
        .args(["self-test-host-namespace", executable])
        .output()
        .expect("run protected-host namespace assertion");
    assert!(
        output.status.success(),
        "protected-host assertion failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"benchmark-host-v1 namespace-argv-valid\n");
}

#[cfg(target_os = "linux")]
#[test]
fn credential_fds_are_cloexec_except_for_the_designated_bootstrap() {
    let executable = env!("CARGO_BIN_EXE_any-mcp-benchmark");
    let output = Command::new(executable)
        .arg("self-test-fd-isolation")
        .output()
        .expect("run FD isolation assertion");
    assert!(
        output.status.success(),
        "FD isolation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"benchmark-supervisor-v1 fd-isolation-valid\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn interrupted_launcher_stops_its_exact_service_before_returning() {
    use std::{
        fs,
        os::{fd::AsRawFd as _, unix::fs::PermissionsExt as _, unix::process::CommandExt as _},
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };

    let root = std::env::temp_dir().join(format!(
        "any-mcp-benchmark-launcher-{}-{}",
        std::process::id(),
        getrandom::u64().unwrap_or(0)
    ));
    let bin = root.join("bin");
    let run_parent = root.join("runs");
    let state = root.join("state");
    fs::create_dir_all(&bin).expect("create fake binary directory");
    fs::create_dir_all(&run_parent).expect("create fake run-root parent");
    fs::create_dir_all(&state).expect("create fake service state");
    fs::set_permissions(&run_parent, fs::Permissions::from_mode(0o700))
        .expect("protect fake run-root parent");

    write_executable(
        &bin.join("systemd-run"),
        r#"#!/bin/sh
if [ -e /proc/$$/fd/9 ]; then : > "$BENCH_FAKE_STATE/leaked-fd"; exit 97; fi
printf '%s\n' systemd-run >> "$BENCH_FAKE_STATE/helpers"
for argument in "$@"; do
  case "$argument" in
    --property=Description=*) printf '%s\n' "${argument#--property=Description=}" > "$BENCH_FAKE_STATE/description" ;;
  esac
done
: > "$BENCH_FAKE_STATE/requested"
if [ "${BENCH_FAKE_NEVER_REGISTER:-0}" = 1 ]; then
  sleep 30 &
  printf '%s\n' "$!" > "$BENCH_FAKE_STATE/runner-descendant"
  wait
fi
if [ "${BENCH_FAKE_DELAY:-0}" = 1 ]; then sleep 1; fi
: > "$BENCH_FAKE_STATE/active"
sleep 0.1
exit "${BENCH_FAKE_RUN_STATUS:-143}"
"#,
    );
    write_executable(
        &bin.join("systemctl"),
        r#"#!/bin/sh
if [ -e /proc/$$/fd/9 ]; then : > "$BENCH_FAKE_STATE/leaked-fd"; exit 97; fi
printf '%s\n' systemctl >> "$BENCH_FAKE_STATE/helpers"
printf '%s\n' "$*" >> "$BENCH_FAKE_STATE/systemctl-calls"
if [ "$2" = show ]; then
  if [ "${BENCH_FAKE_SHOW_FAIL:-0}" = 1 ]; then exit 1; fi
  if [ "${BENCH_FAKE_SHOW_HANG:-0}" = 1 ]; then sleep 30; fi
  if [ "${BENCH_FAKE_CONFIRM_FAIL:-0}" = 1 ] && [ "$4" != --property=Description ]; then exit 1; fi
  if [ "${BENCH_FAKE_CONFIRM_HANG:-0}" = 1 ] && [ "$4" != --property=Description ]; then sleep 30; fi
  if [ "$4" = --property=Description ] && [ -f "$BENCH_FAKE_STATE/active" ]; then
    cat "$BENCH_FAKE_STATE/description"
  elif [ "$4" = --property=LoadState ]; then
    if [ -f "$BENCH_FAKE_STATE/active" ]; then printf 'loaded\n'; else printf 'not-found\n'; fi
  elif [ "$4" = --property=ActiveState ]; then
    if [ -f "$BENCH_FAKE_STATE/active" ]; then printf 'active\n'; else printf 'inactive\n'; fi
  fi
  exit 0
fi
if [ "$2" = stop ]; then
  printf '%s\n' "$3" >> "$BENCH_FAKE_STATE/stopped"
  rm -f "$BENCH_FAKE_STATE/active"
  exit 0
fi
exit 1
"#,
    );
    write_executable(
        &bin.join("sudo"),
        r#"#!/bin/sh
if [ -e /proc/$$/fd/9 ]; then : > "$BENCH_FAKE_STATE/leaked-fd"; exit 97; fi
printf '%s\n' sudo >> "$BENCH_FAKE_STATE/helpers"
if [ "$1" = -n ]; then shift; fi
if [ "$1" = -- ]; then shift; fi
exec "$@"
"#,
    );
    write_executable(
        &bin.join("ip"),
        r#"#!/bin/sh
if [ -e /proc/$$/fd/9 ]; then : > "$BENCH_FAKE_STATE/leaked-fd"; exit 97; fi
printf '%s\n' ip >> "$BENCH_FAKE_STATE/helpers"
printf '%s\n' "$*" >> "$BENCH_FAKE_STATE/ip-calls"
if [ "${BENCH_FAKE_IP_HANG:-0}" = 1 ]; then sleep 30; fi
case "$*" in
  *" route show default")
    if [ "${BENCH_FAKE_ROUTE_HANG:-0}" = 1 ]; then sleep 30; fi
    if [ "${BENCH_FAKE_ROUTE_FAIL:-0}" = 1 ]; then exit 1; fi
    ;;
esac
if [ "${BENCH_FAKE_DELETE_FAIL:-0}" = 1 ] && [ "$1" = netns ] && [ "$2" = delete ]; then
  exit 1
fi
exit 0
"#,
    );
    write_executable(
        &bin.join("setpriv"),
        r#"#!/bin/sh
if [ -e /proc/$$/fd/9 ]; then : > "$BENCH_FAKE_STATE/leaked-fd"; exit 97; fi
exit 0
"#,
    );
    for name in [
        "realpath", "uname", "stat", "id", "od", "tr", "awk", "sleep", "mktemp", "chmod",
        "timeout", "cat", "rm", "setsid",
    ] {
        write_checked_forwarder(&bin.join(name), name);
    }

    let config = root.join("config.json");
    let credential = root.join("credential");
    fs::write(&config, b"{}\n").expect("write fake config");
    fs::write(&credential, b"fixture-secret\n").expect("write fake credential");
    let credential = fs::File::open(&credential).expect("open fake credential");
    let descriptor = credential.as_raw_fd();
    let executable = env!("CARGO_BIN_EXE_any-mcp-benchmark");
    let launcher = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/scripts/run-benchmark-cgroup.sh"
    );
    let existing_path = std::env::var("PATH").expect("test PATH");
    let make_command = |delayed: bool| {
        let mut command = Command::new(launcher);
        command
            .args([
                run_parent.to_str().expect("UTF-8 run parent"),
                "fixture",
                executable,
                config.to_str().expect("UTF-8 config"),
            ])
            .env("PATH", format!("{}:{existing_path}", bin.display()))
            .env("BENCH_FAKE_STATE", &state)
            .env("BENCH_FAKE_DELAY", if delayed { "1" } else { "0" })
            .env("ANY_MCP_BENCHMARK_CREDENTIAL_FDS", "9")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: dup2 is async-signal-safe and only duplicates the already
        // open fixture into the explicitly reserved descriptor 9.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(descriptor, 9) == 9 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        command
    };
    let mut child = make_command(false).spawn().expect("start launcher fixture");
    let ready_deadline = Instant::now() + Duration::from_secs(5);
    while !state.join("active").exists() {
        assert!(
            Instant::now() < ready_deadline,
            "fake service did not start"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let exit_deadline = Instant::now() + Duration::from_secs(12);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll interrupted launcher") {
            break status;
        }
        if Instant::now() >= exit_deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("interrupted launcher did not finish cleanup");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    assert!(!state.join("active").exists(), "service outlived wrapper");
    assert!(
        !state.join("leaked-fd").exists(),
        "ancillary helper inherited caller credential fd"
    );
    let helpers = fs::read_to_string(state.join("helpers")).expect("helper invocation log");
    for expected in [
        "realpath",
        "uname",
        "stat",
        "id",
        "od",
        "tr",
        "mktemp",
        "chmod",
        "timeout",
        "setsid",
        "sleep",
        "systemd-run",
        "systemctl",
        "sudo",
        "ip",
    ] {
        assert!(
            helpers.lines().any(|observed| observed == expected),
            "ancillary helper fixture did not exercise {expected}"
        );
    }
    let stopped = fs::read_to_string(state.join("stopped")).expect("service stop record");
    assert!(stopped.starts_with("any-mcp-benchmark-"));
    assert!(stopped.trim_end().ends_with(".service"));

    for name in ["active", "description", "requested", "stopped"] {
        let _ = fs::remove_file(state.join(name));
    }
    let mut immediate = make_command(true)
        .spawn()
        .expect("start registration-race fixture");
    let request_deadline = Instant::now() + Duration::from_secs(5);
    while !state.join("requested").exists() {
        assert!(
            Instant::now() < request_deadline,
            "fake service request did not start"
        );
        thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: immediate.id is the live wrapper owned by this test.
    assert_eq!(
        unsafe { libc::kill(immediate.id() as i32, libc::SIGTERM) },
        0
    );
    let immediate_deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if immediate
            .try_wait()
            .expect("poll registration-race wrapper")
            .is_some()
        {
            break;
        }
        if Instant::now() >= immediate_deadline {
            let _ = immediate.kill();
            let _ = immediate.wait();
            panic!("registration-race wrapper did not finish cleanup");
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !state.join("active").exists(),
        "service registered after wrapper interruption"
    );
    assert!(
        !state.join("leaked-fd").exists(),
        "registration helper inherited caller credential fd"
    );

    for name in ["active", "description", "requested", "stopped"] {
        let _ = fs::remove_file(state.join(name));
    }
    let deletion_status = make_command(false)
        .env("BENCH_FAKE_RUN_STATUS", "0")
        .env("BENCH_FAKE_DELETE_FAIL", "1")
        .status()
        .expect("run namespace-deletion failure fixture");
    assert!(
        !deletion_status.success(),
        "namespace deletion failure was swallowed"
    );
    assert!(
        !state.join("leaked-fd").exists(),
        "cleanup helper inherited caller credential fd"
    );

    let hang_start = Instant::now();
    let setup_status = make_command(false)
        .env("BENCH_FAKE_IP_HANG", "1")
        .status()
        .expect("run bounded namespace-setup fixture");
    assert!(!setup_status.success(), "hung namespace setup succeeded");
    assert!(
        hang_start.elapsed() < Duration::from_secs(8),
        "namespace setup exceeded its timeout bound"
    );
    assert!(
        !state.join("leaked-fd").exists(),
        "setup helper inherited caller credential fd"
    );

    for mode in ["fail", "hang"] {
        let _ = fs::remove_file(state.join("requested"));
        let route_start = Instant::now();
        let mut route = make_command(false);
        route.env(
            if mode == "fail" {
                "BENCH_FAKE_ROUTE_FAIL"
            } else {
                "BENCH_FAKE_ROUTE_HANG"
            },
            "1",
        );
        let route_status = route.status().expect("run route-isolation failure fixture");
        assert!(!route_status.success(), "failed route probe was accepted");
        assert!(
            route_start.elapsed() < Duration::from_secs(8),
            "route-isolation probe exceeded its timeout bound"
        );
        assert!(
            !state.join("requested").exists(),
            "systemd service was requested after route-probe failure"
        );
    }

    for name in [
        "active",
        "description",
        "requested",
        "stopped",
        "runner-descendant",
    ] {
        let _ = fs::remove_file(state.join(name));
    }
    let never_deletes_before = namespace_delete_count(&state.join("ip-calls"));
    let never_start = Instant::now();
    let never_status = make_command(false)
        .env("BENCH_FAKE_NEVER_REGISTER", "1")
        .status()
        .expect("run never-registering service fixture");
    assert!(!never_status.success());
    assert!(
        never_start.elapsed() < Duration::from_secs(16),
        "never-registering service escaped its deadline"
    );
    assert!(!state.join("active").exists());
    assert_eq!(
        namespace_delete_count(&state.join("ip-calls")),
        never_deletes_before + 2,
        "never-registering fixture leaked an owned namespace"
    );
    let descendant = fs::read_to_string(state.join("runner-descendant"))
        .expect("never-registering descendant pid")
        .trim()
        .parse::<i32>()
        .expect("numeric descendant pid");
    // SAFETY: signal zero only tests the fixture PID recorded by the owned
    // never-registering process group.
    assert_ne!(unsafe { libc::kill(descendant, 0) }, 0);

    for name in [
        "active",
        "description",
        "requested",
        "stopped",
        "runner-descendant",
    ] {
        let _ = fs::remove_file(state.join(name));
    }
    let uncertain_deletes_before = namespace_delete_count(&state.join("ip-calls"));
    let uncertain_start = Instant::now();
    let uncertain_status = make_command(false)
        .env("BENCH_FAKE_NEVER_REGISTER", "1")
        .env("BENCH_FAKE_SHOW_HANG", "1")
        .status()
        .expect("run never-registering uncertain-state fixture");
    assert!(!uncertain_status.success());
    assert!(
        uncertain_start.elapsed() < Duration::from_secs(16),
        "registration and cleanup timeouts accumulated without a bound"
    );
    let uncertain_descendant = fs::read_to_string(state.join("runner-descendant"))
        .expect("uncertain-state descendant pid")
        .trim()
        .parse::<i32>()
        .expect("numeric uncertain-state descendant pid");
    // SAFETY: signal zero only tests the owned fixture PID after teardown.
    assert_ne!(unsafe { libc::kill(uncertain_descendant, 0) }, 0);
    assert_eq!(
        namespace_delete_count(&state.join("ip-calls")),
        uncertain_deletes_before,
        "uncertain unit absence allowed namespace deletion"
    );

    for mode in ["fail", "hang"] {
        for name in ["active", "description", "requested", "stopped"] {
            let _ = fs::remove_file(state.join(name));
        }
        let deletes_before = namespace_delete_count(&state.join("ip-calls"));
        let show_start = Instant::now();
        let mut show = make_command(false);
        show.env(
            if mode == "fail" {
                "BENCH_FAKE_CONFIRM_FAIL"
            } else {
                "BENCH_FAKE_CONFIRM_HANG"
            },
            "1",
        );
        let show_status = show.status().expect("run uncertain-unit-state fixture");
        assert!(!show_status.success());
        assert!(
            show_start.elapsed() < Duration::from_secs(12),
            "unit-state uncertainty escaped its cleanup deadline"
        );
        assert_eq!(
            namespace_delete_count(&state.join("ip-calls")),
            deletes_before,
            "namespaces were deleted without proving unit teardown"
        );
    }
    fs::remove_dir_all(root).expect("remove launcher fixture");
}

#[cfg(target_os = "linux")]
fn write_executable(path: &std::path::Path, contents: &str) {
    use std::{fs, os::unix::fs::PermissionsExt as _};

    fs::write(path, contents).expect("write fake executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .expect("make fake executable runnable");
}

#[cfg(target_os = "linux")]
fn write_checked_forwarder(path: &std::path::Path, name: &str) {
    let contents = format!(
        "#!/bin/sh\nif [ -e /proc/$$/fd/9 ]; then : > \"$BENCH_FAKE_STATE/leaked-fd\"; exit 97; fi\nprintf '%s\\n' '{name}' >> \"$BENCH_FAKE_STATE/helpers\"\nexec '/run/current-system/sw/bin/{name}' \"$@\"\n"
    );
    write_executable(path, &contents);
}

#[cfg(target_os = "linux")]
fn namespace_delete_count(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("netns delete"))
        .count()
}

#[test]
fn result_schema_is_closed_and_accepts_blocked_preflight() {
    let schema = serde_json::from_str(RESULT_SCHEMA).expect("parse benchmark result schema");
    let validator = jsonschema::draft202012::options()
        .build(&schema)
        .expect("compile benchmark result schema");
    let event = json!({
        "schema_version": 1,
        "event": "blocked",
        "run_id": "fixture-1",
        "mode": "production-verified-official",
        "track": "protocol",
        "reason": "operator inputs are absent",
        "run_root": "any-mcp-benchmark.fixture"
    });
    assert!(validator.is_valid(&event));
    let mut open_event = event;
    open_event["unexpected"] = json!(true);
    assert!(!validator.is_valid(&open_event));
}

#[test]
fn serialized_summary_matches_the_published_closed_schema() {
    let executable = env!("CARGO_BIN_EXE_any-mcp-benchmark");
    let output = Command::new(executable)
        .arg("self-test-summary")
        .output()
        .expect("run summary schema fixture");
    assert!(output.status.success());
    let event = serde_json::from_slice(&output.stdout).expect("parse summary fixture");
    let schema = serde_json::from_str(RESULT_SCHEMA).expect("parse benchmark result schema");
    let validator = jsonschema::draft202012::options()
        .build(&schema)
        .expect("compile benchmark result schema");
    assert!(validator.is_valid(&event));
}

#[test]
fn serialized_pair_matches_the_published_closed_schema() {
    let executable = env!("CARGO_BIN_EXE_any-mcp-benchmark");
    let output = Command::new(executable)
        .arg("self-test-pair")
        .output()
        .expect("run pair schema fixture");
    assert!(output.status.success());
    let event = serde_json::from_slice(&output.stdout).expect("parse pair fixture");
    let schema = serde_json::from_str(RESULT_SCHEMA).expect("parse benchmark result schema");
    let validator = jsonschema::draft202012::options()
        .build(&schema)
        .expect("compile benchmark result schema");
    assert!(validator.is_valid(&event));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn live_mode_fails_closed_outside_linux() {
    let executable = env!("CARGO_BIN_EXE_any-mcp-benchmark");
    let output = Command::new(executable)
        .args(["run", "/tmp", "/tmp/missing-benchmark-config.json"])
        .output()
        .expect("run fail-closed benchmark command");

    assert!(!output.status.success());
}
