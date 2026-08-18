use std::{ffi::OsString, future::Future, time::Duration};

use anyhow::{Result, bail};
use tokio::time::Instant;

#[derive(Clone, Copy, Debug)]
pub(super) struct WorkflowDeadline {
    expires_at: Option<Instant>,
    configured: Duration,
}

impl WorkflowDeadline {
    pub(super) const fn disabled() -> Self {
        Self {
            expires_at: None,
            configured: Duration::ZERO,
        }
    }

    pub(super) fn from_env(
        name: &'static str,
        default: Duration,
        maximum: Duration,
        zero_disables: bool,
    ) -> Result<Self> {
        let configured = parse_timeout_value(
            name,
            std::env::var_os(name),
            default,
            maximum,
            zero_disables,
        )?;
        let expires_at = configured
            .map(|duration| {
                Instant::now()
                    .checked_add(duration)
                    .ok_or_else(|| anyhow::anyhow!("{name} cannot be represented as a deadline"))
            })
            .transpose()?;
        Ok(Self {
            expires_at,
            configured: configured.unwrap_or(Duration::ZERO),
        })
    }

    #[cfg(test)]
    pub(super) fn after(configured: Option<Duration>) -> Self {
        Self {
            expires_at: configured.and_then(|duration| Instant::now().checked_add(duration)),
            configured: configured.unwrap_or(Duration::ZERO),
        }
    }

    pub(super) fn expires_at(self) -> Option<Instant> {
        self.expires_at
    }

    pub(super) fn ensure_remaining(self, label: &str) -> Result<()> {
        if self
            .expires_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            bail!(
                "{label} timed out after {} seconds",
                self.configured.as_secs()
            );
        }
        Ok(())
    }

    pub(super) async fn run<F, T>(self, label: &str, future: F) -> Result<T>
    where
        F: Future<Output = T>,
    {
        let Some(deadline) = self.expires_at else {
            return Ok(future.await);
        };
        if Instant::now() >= deadline {
            bail!(
                "{label} timed out after {} seconds",
                self.configured.as_secs()
            );
        }
        tokio::time::timeout_at(deadline, future)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "{label} timed out after {} seconds",
                    self.configured.as_secs()
                )
            })
    }
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
        assert!(
            parse_timeout_value(
                "ANYR_WORKFLOW_TIMEOUT_SECS",
                Some("3601".into()),
                Duration::from_mins(30),
                Duration::from_hours(1),
                true,
            )
            .is_err()
        );
        assert!(
            parse_timeout_value(
                "ANYR_INIT_CLI_TIMEOUT_SECS",
                Some("601".into()),
                Duration::from_mins(2),
                Duration::from_mins(10),
                false,
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn timeout_values_reject_non_unicode() {
        use std::os::unix::ffi::OsStringExt as _;

        let invalid = OsString::from_vec(vec![0xff]);
        assert!(
            parse_timeout_value(
                "TEST",
                Some(invalid),
                Duration::from_secs(1),
                Duration::from_secs(2),
                false,
            )
            .is_err()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn aggregate_pages_share_one_absolute_deadline() {
        let deadline = WorkflowDeadline::after(Some(Duration::from_secs(5)));
        deadline
            .run("workflow", tokio::time::sleep(Duration::from_secs(3)))
            .await
            .expect("first page");
        let error = deadline
            .run("workflow", tokio::time::sleep(Duration::from_secs(3)))
            .await
            .expect_err("second page exceeds original deadline");
        assert!(error.to_string().contains("timed out after 5 seconds"));
    }
}
