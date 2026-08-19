use std::{
    ffi::OsString,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, bail};
use anytype::process_watcher::ProcessWatcherTimeouts;
use tokio::time::Instant;

const WORKFLOW_DEFAULT: Duration = Duration::from_hours(1);
const WORKFLOW_MAXIMUM: Duration = Duration::from_hours(2);
const EVENT_CONNECT_MAXIMUM: Duration = Duration::from_mins(2);
const PROCESS_START_MAXIMUM: Duration = Duration::from_mins(5);
const PROCESS_IDLE_MAXIMUM: Duration = Duration::from_mins(5);
const PROCESS_DONE_MAXIMUM: Duration = Duration::from_hours(1);

/// One absolute backup or restore workflow deadline and its validated watcher limits.
#[derive(Clone, Copy, Debug)]
pub struct WorkflowDeadline {
    expires_at: Option<Instant>,
    configured: Duration,
    process_timeouts: ProcessWatcherTimeouts,
}

/// Caller-owned authority for one publication commit.
///
/// Preparation workers never receive this value, so a worker detached by a
/// timeout cannot change a caller-visible destination.
#[derive(Clone, Debug)]
pub(super) struct PublicationCommit {
    expires_at: Option<Instant>,
    configured: Duration,
    timeout_message: &'static str,
    state: Arc<AtomicU8>,
}

impl PublicationCommit {
    pub(super) fn ensure_remaining(&self) -> Result<()> {
        if self
            .expires_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            bail!(
                "{} after {} seconds",
                self.timeout_message,
                self.configured.as_secs()
            );
        }
        Ok(())
    }

    pub(super) fn commit<T>(self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        if self
            .expires_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            let _ = self
                .state
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
        }
        if self
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            bail!(
                "{} after {} seconds",
                self.timeout_message,
                self.configured.as_secs()
            );
        }
        let result = operation();
        self.state.store(3, Ordering::Release);
        result
    }
}

impl WorkflowDeadline {
    /// Reads and validates all backup timeout environment variables.
    ///
    /// Call this constructor before client construction so invalid
    /// configuration cannot precede network or filesystem mutation.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|name| std::env::var_os(name))
    }

    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<OsString>) -> Result<Self> {
        let workflow = parse_timeout_value(
            "ANYBACK_WORKFLOW_TIMEOUT_SECS",
            lookup("ANYBACK_WORKFLOW_TIMEOUT_SECS"),
            WORKFLOW_DEFAULT,
            WORKFLOW_MAXIMUM,
            true,
        )?;
        let defaults = ProcessWatcherTimeouts::default();
        let process_timeouts = ProcessWatcherTimeouts {
            event_stream_connect_timeout: parse_required_timeout_value(
                "ANYBACK_EVENT_STREAM_CONNECT_TIMEOUT",
                lookup("ANYBACK_EVENT_STREAM_CONNECT_TIMEOUT"),
                defaults.event_stream_connect_timeout,
                EVENT_CONNECT_MAXIMUM,
            )?,
            process_start_timeout: parse_required_timeout_value(
                "ANYBACK_PROCESS_START_TIMEOUT",
                lookup("ANYBACK_PROCESS_START_TIMEOUT"),
                defaults.process_start_timeout,
                PROCESS_START_MAXIMUM,
            )?,
            process_idle_timeout: parse_required_timeout_value(
                "ANYBACK_PROCESS_IDLE_TIMEOUT",
                lookup("ANYBACK_PROCESS_IDLE_TIMEOUT"),
                defaults.process_idle_timeout,
                PROCESS_IDLE_MAXIMUM,
            )?,
            process_done_timeout: parse_required_timeout_value(
                "ANYBACK_PROCESS_DONE_TIMEOUT",
                lookup("ANYBACK_PROCESS_DONE_TIMEOUT"),
                defaults.process_done_timeout,
                PROCESS_DONE_MAXIMUM,
            )?,
        };
        let expires_at = workflow
            .map(|duration| {
                Instant::now().checked_add(duration).ok_or_else(|| {
                    anyhow::anyhow!(
                        "ANYBACK_WORKFLOW_TIMEOUT_SECS cannot be represented as a deadline"
                    )
                })
            })
            .transpose()?;
        Ok(Self {
            expires_at,
            configured: workflow.unwrap_or(Duration::ZERO),
            process_timeouts,
        })
    }

    pub(super) fn local_command() -> Self {
        Self {
            expires_at: None,
            configured: Duration::ZERO,
            process_timeouts: ProcessWatcherTimeouts::default(),
        }
    }

    #[cfg(test)]
    pub(super) fn new(
        workflow: Option<Duration>,
        process_timeouts: ProcessWatcherTimeouts,
    ) -> Self {
        Self {
            expires_at: workflow.and_then(|duration| Instant::now().checked_add(duration)),
            configured: workflow.unwrap_or(Duration::ZERO),
            process_timeouts,
        }
    }

    pub(super) fn ensure_read_remaining(self) -> Result<()> {
        if self.expired() {
            bail!("backup workflow timed out; read was aborted");
        }
        Ok(())
    }

    pub(super) fn ensure_mutation_remaining(self) -> Result<()> {
        if self.expired() {
            bail!("restore workflow timed out; mutation outcome is indeterminate");
        }
        Ok(())
    }

    pub(super) fn ensure_restore_preflight_remaining(self) -> Result<()> {
        if self.expired() {
            bail!("restore workflow timed out before mutation dispatch");
        }
        Ok(())
    }

    pub(super) async fn run_read<F, T>(self, future: F) -> Result<T>
    where
        F: Future<Output = T>,
    {
        self.run("backup workflow timed out; read was aborted", future)
            .await
    }

    pub(super) async fn run_export<F, T>(self, future: F) -> Result<T>
    where
        F: Future<Output = T>,
    {
        self.run(
            "backup workflow timed out; read was aborted and a server-side export artifact may exist",
            future,
        )
        .await
    }

    pub(super) async fn run_mutation<F, T>(self, future: F) -> Result<T>
    where
        F: Future<Output = T>,
    {
        self.run(
            "restore workflow timed out; mutation outcome is indeterminate",
            future,
        )
        .await
    }

    pub(super) async fn run_restore_preflight<F, T>(self, future: F) -> Result<T>
    where
        F: Future<Output = T>,
    {
        self.run(
            "restore workflow timed out before mutation dispatch",
            future,
        )
        .await
    }

    pub(super) async fn run_read_publication<P, T, Prepare, Commit>(
        self,
        timeout_message: &'static str,
        prepare: Prepare,
        commit: Commit,
    ) -> Result<T>
    where
        Prepare: FnOnce() -> Result<P> + Send + 'static,
        P: Send + 'static,
        Commit: FnOnce(P, PublicationCommit) -> Result<T>,
    {
        self.run_publication(timeout_message, prepare, commit).await
    }

    pub(super) async fn run_mutation_publication<P, T, Prepare, Commit>(
        self,
        prepare: Prepare,
        commit: Commit,
    ) -> Result<T>
    where
        Prepare: FnOnce() -> Result<P> + Send + 'static,
        P: Send + 'static,
        Commit: FnOnce(P, PublicationCommit) -> Result<T>,
    {
        self.run_publication(
            "restore workflow timed out; mutation outcome is indeterminate",
            prepare,
            commit,
        )
        .await
    }

    pub(super) fn process_timeouts(self) -> Result<ProcessWatcherTimeouts> {
        let Some(remaining) = self.remaining() else {
            return Ok(self.process_timeouts);
        };
        if remaining.is_zero() {
            bail!("restore workflow timed out before mutation dispatch");
        }
        Ok(ProcessWatcherTimeouts {
            event_stream_connect_timeout: self
                .process_timeouts
                .event_stream_connect_timeout
                .min(remaining),
            process_start_timeout: self.process_timeouts.process_start_timeout.min(remaining),
            process_idle_timeout: self.process_timeouts.process_idle_timeout.min(remaining),
            process_done_timeout: self.process_timeouts.process_done_timeout.min(remaining),
        })
    }

    fn expired(self) -> bool {
        self.expires_at
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn remaining(self) -> Option<Duration> {
        self.expires_at
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    async fn run<F, T>(self, message: &str, future: F) -> Result<T>
    where
        F: Future<Output = T>,
    {
        let Some(deadline) = self.expires_at else {
            return Ok(future.await);
        };
        if Instant::now() >= deadline {
            bail!("{message} after {} seconds", self.configured.as_secs());
        }
        tokio::time::timeout_at(deadline, future)
            .await
            .map_err(|_| anyhow::anyhow!("{message} after {} seconds", self.configured.as_secs()))
    }

    async fn run_publication<P, T, Prepare, Commit>(
        self,
        timeout_message: &'static str,
        prepare: Prepare,
        commit: Commit,
    ) -> Result<T>
    where
        Prepare: FnOnce() -> Result<P> + Send + 'static,
        P: Send + 'static,
        Commit: FnOnce(P, PublicationCommit) -> Result<T>,
    {
        let state = Arc::new(AtomicU8::new(0));
        let authority = PublicationCommit {
            expires_at: self.expires_at,
            configured: self.configured,
            timeout_message,
            state: Arc::clone(&state),
        };
        authority.ensure_remaining()?;
        let task = tokio::task::spawn_blocking(prepare);
        let joined = match self.expires_at {
            Some(deadline) => {
                if let Ok(joined) = tokio::time::timeout_at(deadline, task).await {
                    joined
                } else {
                    let _ = state.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
                    bail!(
                        "{timeout_message} after {} seconds",
                        self.configured.as_secs()
                    );
                }
            }
            None => task.await,
        };
        let prepared = joined.map_err(|_| anyhow::anyhow!("local publication worker failed"))??;
        authority.ensure_remaining()?;
        commit(prepared, authority)
    }
}

fn parse_required_timeout_value(
    name: &str,
    raw: Option<OsString>,
    default: Duration,
    maximum: Duration,
) -> Result<Duration> {
    parse_timeout_value(name, raw, default, maximum, false)?
        .ok_or_else(|| anyhow::anyhow!("{name} cannot disable its process safety boundary"))
}

fn parse_timeout_value(
    name: &str,
    raw: Option<OsString>,
    default: Duration,
    maximum: Duration,
    zero_disables: bool,
) -> Result<Option<Duration>> {
    let Some(raw) = raw else {
        return Ok(Some(default));
    };
    let raw = raw
        .into_string()
        .map_err(|_| anyhow::anyhow!("{name} must be valid Unicode ASCII decimal"))?;
    if raw == "0" && zero_disables {
        return Ok(None);
    }
    if raw.is_empty() || raw.starts_with('0') || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("{name} must use canonical ASCII decimal seconds");
    }
    let seconds = raw
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("{name} is outside the supported range"))?;
    if seconds == 0 || seconds > maximum.as_secs() {
        bail!(
            "{name} must be between 1 and {} seconds{}",
            maximum.as_secs(),
            if zero_disables {
                ", or exactly 0 to disable the outer workflow deadline"
            } else {
                ""
            }
        );
    }
    Ok(Some(Duration::from_secs(seconds)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_values_use_canonical_bounded_decimal_grammar() {
        let default = Duration::from_secs(30);
        let maximum = Duration::from_mins(1);
        assert_eq!(
            parse_timeout_value("TEST", None, default, maximum, true).expect("default"),
            Some(default)
        );
        assert_eq!(
            parse_timeout_value("TEST", Some("60".into()), default, maximum, true)
                .expect("maximum"),
            Some(maximum)
        );
        assert_eq!(
            parse_timeout_value("TEST", Some("0".into()), default, maximum, true)
                .expect("disabled"),
            None
        );
        for invalid in ["", "00", "01", "+1", "-1", " 1", "1 ", "1.0", "61"] {
            assert!(
                parse_timeout_value("TEST", Some(invalid.into()), default, maximum, true).is_err(),
                "accepted invalid value {invalid:?}"
            );
        }
        assert!(parse_timeout_value("TEST", Some("0".into()), default, maximum, false).is_err());
        for (name, maximum) in [
            ("ANYBACK_WORKFLOW_TIMEOUT_SECS", 7200),
            ("ANYBACK_EVENT_STREAM_CONNECT_TIMEOUT", 120),
            ("ANYBACK_PROCESS_START_TIMEOUT", 300),
            ("ANYBACK_PROCESS_IDLE_TIMEOUT", 300),
            ("ANYBACK_PROCESS_DONE_TIMEOUT", 3600),
        ] {
            let above = maximum + 1;
            assert!(
                parse_timeout_value(
                    name,
                    Some(above.to_string().into()),
                    Duration::from_secs(1),
                    Duration::from_secs(maximum),
                    name == "ANYBACK_WORKFLOW_TIMEOUT_SECS",
                )
                .is_err(),
                "accepted over-maximum value for {name}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn timeout_values_reject_non_unicode() {
        use std::os::unix::ffi::OsStringExt as _;

        assert!(
            parse_timeout_value(
                "TEST",
                Some(OsString::from_vec(vec![0xff])),
                Duration::from_secs(1),
                Duration::from_secs(2),
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn watcher_limits_are_clamped_to_the_outer_remaining_budget() {
        let deadline = WorkflowDeadline::new(
            Some(Duration::from_secs(1)),
            ProcessWatcherTimeouts {
                event_stream_connect_timeout: Duration::from_secs(10),
                process_start_timeout: Duration::from_secs(20),
                process_idle_timeout: Duration::from_secs(30),
                process_done_timeout: Duration::from_secs(40),
            },
        );
        let timeouts = deadline.process_timeouts().expect("remaining budget");
        assert!(timeouts.event_stream_connect_timeout <= Duration::from_secs(1));
        assert!(timeouts.process_start_timeout <= Duration::from_secs(1));
        assert!(timeouts.process_idle_timeout <= Duration::from_secs(1));
        assert!(timeouts.process_done_timeout <= Duration::from_secs(1));
    }

    #[test]
    fn invalid_timeout_configuration_prevents_later_side_effects() {
        let side_effect_ran = std::cell::Cell::new(false);
        let configured = WorkflowDeadline::from_lookup(|name| {
            (name == "ANYBACK_PROCESS_IDLE_TIMEOUT").then(|| OsString::from("301"))
        });
        let result = configured.map(|_| {
            side_effect_ran.set(true);
        });
        assert!(result.is_err());
        assert!(!side_effect_ran.get());
    }

    #[tokio::test(start_paused = true)]
    async fn expired_process_configuration_is_pre_dispatch() {
        let deadline = WorkflowDeadline::new(
            Some(Duration::from_secs(1)),
            ProcessWatcherTimeouts::default(),
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = deadline
            .process_timeouts()
            .expect_err("expired workflow must reject subscription")
            .to_string();
        assert!(error.contains("before mutation dispatch"));
        assert!(!error.contains("indeterminate"));
    }

    // Paused time keeps the second wait from completing in the same coarse
    // timer tick as the deadline (Windows timers), which `timeout_at` would
    // report as success.
    #[tokio::test(start_paused = true)]
    async fn one_absolute_deadline_is_not_reset_between_waits() {
        let deadline = WorkflowDeadline::new(
            Some(Duration::from_millis(50)),
            ProcessWatcherTimeouts::default(),
        );
        deadline
            .run_read(tokio::time::sleep(Duration::from_millis(30)))
            .await
            .expect("first phase");
        assert!(
            deadline
                .run_read(tokio::time::sleep(Duration::from_millis(30)))
                .await
                .is_err()
        );
    }
}
