// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{
    config::{BenchmarkMode, BenchmarkTrack, COMPLETE_PAIRS},
    protocol::{CallMeasures, CatalogMeasures, RedactedStderr, StartupMeasures},
};

const BOOTSTRAP_SAMPLES: usize = 10_000;
const NONINFERIORITY_MARGIN: f64 = -0.05;
const FAILURE_PENALTY_NS: u64 = 180_000_000_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairRecord {
    pub schema_version: u32,
    pub event: PairEventKind,
    pub run_id: String,
    pub mode: BenchmarkMode,
    pub track: BenchmarkTrack,
    pub scenario: String,
    pub attempt: u16,
    pub pair_index: Option<u16>,
    pub order: ArmOrder,
    pub local: ArmRecord,
    pub upstream: ArmRecord,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum PairEventKind {
    Pair,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ArmOrder {
    LocalUpstream,
    UpstreamLocal,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Implementation {
    Local,
    Upstream,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum FailureCategory {
    Spawn,
    Timeout,
    Protocol,
    ToolError,
    Oracle,
    AnswerMismatch,
    Cleanup,
    AgentContract,
    Environment,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArmRecord {
    pub implementation: Implementation,
    pub success: bool,
    pub timeout: bool,
    pub environmental_invalid: bool,
    pub failure_category: Option<FailureCategory>,
    pub startup: Option<StartupMeasures>,
    pub catalog: Option<CatalogMeasures>,
    pub calls: Vec<CallMeasures>,
    pub ttft_ns: Option<u64>,
    pub end_to_end_ns: Option<u64>,
    pub stderr: Option<RedactedStderr>,
    pub answer_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub complete_pairs: u16,
    pub environmental_invalid_local: u16,
    pub environmental_invalid_upstream: u16,
    pub valid: bool,
    pub invalid_reason: Option<String>,
    pub local: ArmSummary,
    pub upstream: ArmSummary,
    pub paired_success_difference: f64,
    pub paired_success_ci95: [f64; 2],
    pub paired_latency_ratio: Option<f64>,
    pub paired_latency_ratio_ci95: Option<[f64; 2]>,
    pub mcnemar_exact_p: f64,
    pub local_noninferior_at_five_points: bool,
    pub latency_ranking_provisional: bool,
    pub p95_is_descriptive: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum SummaryEventKind {
    Summary,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryEvent {
    pub schema_version: u32,
    pub event: SummaryEventKind,
    pub run_id: String,
    pub mode: BenchmarkMode,
    pub track: BenchmarkTrack,
    pub scenario: String,
    pub statistics: Summary,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArmSummary {
    pub successes: u16,
    pub failures: u16,
    pub timeouts: u16,
    pub success_rate: f64,
    pub success_rate_wilson_ci95: [f64; 2],
    pub success_latency_p50_ns: Option<u64>,
    pub success_latency_p95_ns: Option<u64>,
    pub success_adjusted_p50_ns: u64,
    pub success_adjusted_p95_ns: u64,
}

pub fn summarize(records: &[PairRecord], seed: u64) -> Result<Summary, String> {
    let first_record = records
        .first()
        .ok_or_else(|| "summary has no pair records".to_owned())?;
    for record in records {
        if record.schema_version != 1 || record.event != PairEventKind::Pair {
            return Err("record is not a v1 pair event".to_owned());
        }
        if record.run_id != first_record.run_id
            || record.mode != first_record.mode
            || record.track != first_record.track
            || record.scenario != first_record.scenario
        {
            return Err("pair records do not share one attested run envelope".to_owned());
        }
        if record.local.implementation != Implementation::Local
            || record.upstream.implementation != Implementation::Upstream
        {
            return Err("pair arms have the wrong implementation identity".to_owned());
        }
        validate_any_outcome(&record.local)?;
        validate_any_outcome(&record.upstream)?;
        match record.pair_index {
            Some(_)
                if record.local.environmental_invalid || record.upstream.environmental_invalid =>
            {
                return Err("complete pair contains an environmental-invalid arm".to_owned());
            }
            None if !record.local.environmental_invalid
                && !record.upstream.environmental_invalid =>
            {
                return Err("retry row lacks an environmental-invalid arm".to_owned());
            }
            _ => {}
        }
    }
    let unique_attempts = records
        .iter()
        .map(|record| record.attempt)
        .collect::<BTreeSet<_>>();
    if unique_attempts.len() != records.len() {
        return Err("pair attempts must be unique within one scenario".to_owned());
    }
    let invalid_local = count(records, |outcome| outcome.environmental_invalid);
    let invalid_upstream = count_upstream(records, |outcome| outcome.environmental_invalid);
    let valid_records = records
        .iter()
        .filter(|record| {
            record.pair_index.is_some()
                && !record.local.environmental_invalid
                && !record.upstream.environmental_invalid
        })
        .collect::<Vec<_>>();
    if valid_records.len() != usize::from(COMPLETE_PAIRS) {
        return Err(format!(
            "summary requires exactly {COMPLETE_PAIRS} complete A/B pairs"
        ));
    }
    let local_first = valid_records
        .iter()
        .filter(|record| record.order == ArmOrder::LocalUpstream)
        .count();
    if local_first != usize::from(COMPLETE_PAIRS / 2) {
        return Err("complete pairs require an exactly counterbalanced arm order".to_owned());
    }
    for (expected, record) in valid_records.iter().enumerate() {
        if record.pair_index.map(usize::from) != Some(expected) {
            return Err("complete pair indices must be contiguous".to_owned());
        }
        validate_outcome(&record.local)?;
        validate_outcome(&record.upstream)?;
    }
    let first = valid_records
        .first()
        .ok_or_else(|| "summary has no complete pair metadata".to_owned())?;
    if valid_records.iter().any(|record| {
        record.run_id != first.run_id
            || record.mode != first.mode
            || record.track != first.track
            || record.scenario != first.scenario
    }) {
        return Err("complete pairs do not share one attested run envelope".to_owned());
    }
    let imbalance = invalid_local.abs_diff(invalid_upstream);
    let invalid_reason = if invalid_local > 2 || invalid_upstream > 2 {
        Some("more than two environmental-invalid attempts occurred for one arm".to_owned())
    } else if imbalance > 1 {
        Some("environmental-invalid arm imbalance exceeds one".to_owned())
    } else {
        None
    };
    let local = arm_summary(&valid_records, true);
    let upstream = arm_summary(&valid_records, false);
    let differences = valid_records
        .iter()
        .map(|record| i8::from(record.local.success) - i8::from(record.upstream.success))
        .collect::<Vec<_>>();
    let paired_success_difference = mean_difference(&differences);
    let paired_success_ci95 = bootstrap_ci(&differences, seed);
    let latency_ratios = valid_records
        .iter()
        .map(|record| {
            (
                record.local.end_to_end_ns.unwrap_or(FAILURE_PENALTY_NS),
                record.upstream.end_to_end_ns.unwrap_or(FAILURE_PENALTY_NS),
            )
        })
        .filter(|(local, upstream)| *local > 0 && *upstream > 0)
        .map(|(local, upstream)| (local as f64 / upstream as f64).ln())
        .collect::<Vec<_>>();
    let paired_latency_ratio = geometric_mean(&latency_ratios);
    let paired_latency_ratio_ci95 = bootstrap_log_ratio_ci(&latency_ratios, seed ^ 0xa5a5_5a5a);
    let local_only = valid_records
        .iter()
        .filter(|record| record.local.success && !record.upstream.success)
        .count();
    let upstream_only = valid_records
        .iter()
        .filter(|record| !record.local.success && record.upstream.success)
        .count();
    let timeout_rate = f64::from(local.timeouts.max(upstream.timeouts)) / f64::from(COMPLETE_PAIRS);
    let p95_is_descriptive = local.successes < 100 || upstream.successes < 100;
    Ok(Summary {
        complete_pairs: COMPLETE_PAIRS,
        environmental_invalid_local: invalid_local,
        environmental_invalid_upstream: invalid_upstream,
        valid: invalid_reason.is_none(),
        invalid_reason,
        local,
        upstream,
        paired_success_difference,
        paired_success_ci95,
        paired_latency_ratio,
        paired_latency_ratio_ci95,
        mcnemar_exact_p: mcnemar_exact(local_only, upstream_only),
        local_noninferior_at_five_points: paired_success_ci95[0] > NONINFERIORITY_MARGIN,
        latency_ranking_provisional: timeout_rate > 0.05,
        p95_is_descriptive,
    })
}

pub fn summarize_event(records: &[PairRecord], seed: u64) -> Result<SummaryEvent, String> {
    let first = records
        .iter()
        .find(|record| record.pair_index.is_some())
        .ok_or_else(|| "summary has no complete pair envelope".to_owned())?;
    let statistics = summarize(records, seed)?;
    Ok(SummaryEvent {
        schema_version: 1,
        event: SummaryEventKind::Summary,
        run_id: first.run_id.clone(),
        mode: first.mode,
        track: first.track,
        scenario: first.scenario.clone(),
        statistics,
    })
}

pub fn fixture_summary_event() -> Result<SummaryEvent, String> {
    let records = fixture_pairs();
    summarize_event(&records, 1)
}

pub fn fixture_pair_event() -> PairRecord {
    fixture_pair(0)
}

fn fixture_pairs() -> Vec<PairRecord> {
    (0..COMPLETE_PAIRS).map(fixture_pair).collect::<Vec<_>>()
}

fn fixture_pair(index: u16) -> PairRecord {
    PairRecord {
        schema_version: 1,
        event: PairEventKind::Pair,
        run_id: "schema-fixture".to_owned(),
        mode: BenchmarkMode::ProductionVerifiedOfficial,
        track: BenchmarkTrack::Protocol,
        scenario: "direct-crud".to_owned(),
        attempt: index,
        pair_index: Some(index),
        order: if index.is_multiple_of(2) {
            ArmOrder::LocalUpstream
        } else {
            ArmOrder::UpstreamLocal
        },
        local: fixture_arm(Implementation::Local, 10),
        upstream: fixture_arm(Implementation::Upstream, 12),
    }
}

fn fixture_arm(implementation: Implementation, duration: u64) -> ArmRecord {
    ArmRecord {
        implementation,
        success: true,
        timeout: false,
        environmental_invalid: false,
        failure_category: None,
        startup: None,
        catalog: None,
        calls: Vec::new(),
        ttft_ns: Some(duration / 2),
        end_to_end_ns: Some(duration),
        stderr: None,
        answer_sha256: Some("0".repeat(64)),
    }
}

fn validate_outcome(outcome: &ArmRecord) -> Result<(), String> {
    if outcome.environmental_invalid {
        return Err("environmental-invalid outcome appeared in a complete pair".to_owned());
    }
    if outcome.success != outcome.end_to_end_ns.is_some() {
        return Err("successful outcomes require one end-to-end duration".to_owned());
    }
    if outcome.success && (outcome.timeout || outcome.failure_category.is_some()) {
        return Err("successful outcome contains failure metadata".to_owned());
    }
    if !outcome.success && outcome.failure_category.is_none() {
        return Err("failed outcome requires a closed failure category".to_owned());
    }
    if outcome.environmental_invalid
        != (outcome.failure_category == Some(FailureCategory::Environment))
    {
        return Err("environmental-invalid state differs from its failure category".to_owned());
    }
    if outcome.end_to_end_ns == Some(0) {
        return Err("end-to-end duration must be positive".to_owned());
    }
    Ok(())
}

fn validate_any_outcome(outcome: &ArmRecord) -> Result<(), String> {
    if outcome.environmental_invalid {
        if outcome.success
            || outcome.timeout
            || outcome.failure_category != Some(FailureCategory::Environment)
            || outcome.end_to_end_ns.is_some()
        {
            return Err("environmental-invalid arm has contradictory outcome metadata".to_owned());
        }
        return Ok(());
    }
    validate_outcome(outcome)
}

fn arm_summary(records: &[&PairRecord], local: bool) -> ArmSummary {
    let outcomes = records
        .iter()
        .map(|record| {
            if local {
                &record.local
            } else {
                &record.upstream
            }
        })
        .collect::<Vec<_>>();
    let successes = outcomes.iter().filter(|outcome| outcome.success).count();
    let timeouts = outcomes.iter().filter(|outcome| outcome.timeout).count();
    let mut successful = outcomes
        .iter()
        .filter_map(|outcome| outcome.end_to_end_ns)
        .collect::<Vec<_>>();
    successful.sort_unstable();
    let mut adjusted = outcomes
        .iter()
        .map(|outcome| outcome.end_to_end_ns.unwrap_or(FAILURE_PENALTY_NS))
        .collect::<Vec<_>>();
    adjusted.sort_unstable();
    ArmSummary {
        successes: to_u16(successes),
        failures: COMPLETE_PAIRS.saturating_sub(to_u16(successes)),
        timeouts: to_u16(timeouts),
        success_rate: successes as f64 / f64::from(COMPLETE_PAIRS),
        success_rate_wilson_ci95: wilson_ci(successes, usize::from(COMPLETE_PAIRS)),
        success_latency_p50_ns: percentile(&successful, 50),
        success_latency_p95_ns: percentile(&successful, 95),
        success_adjusted_p50_ns: percentile(&adjusted, 50).unwrap_or(FAILURE_PENALTY_NS),
        success_adjusted_p95_ns: percentile(&adjusted, 95).unwrap_or(FAILURE_PENALTY_NS),
    }
}

fn count(records: &[PairRecord], predicate: impl Fn(&ArmRecord) -> bool) -> u16 {
    to_u16(
        records
            .iter()
            .filter(|record| predicate(&record.local))
            .count(),
    )
}

fn count_upstream(records: &[PairRecord], predicate: impl Fn(&ArmRecord) -> bool) -> u16 {
    to_u16(
        records
            .iter()
            .filter(|record| predicate(&record.upstream))
            .count(),
    )
}

fn percentile(sorted: &[u64], percentile: usize) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let numerator = percentile.saturating_mul(sorted.len()).saturating_add(99);
    let rank = numerator / 100;
    sorted.get(rank.saturating_sub(1)).copied()
}

fn mean_difference(values: &[i8]) -> f64 {
    let total = values.iter().map(|value| i64::from(*value)).sum::<i64>();
    total as f64 / values.len() as f64
}

fn bootstrap_ci(values: &[i8], seed: u64) -> [f64; 2] {
    let mut generator = XorShift64::new(seed);
    let mut samples = Vec::with_capacity(BOOTSTRAP_SAMPLES);
    for _ in 0..BOOTSTRAP_SAMPLES {
        let mut total = 0i64;
        for _ in 0..values.len() {
            let index = generator.index(values.len());
            total += i64::from(values[index]);
        }
        samples.push(total as f64 / values.len() as f64);
    }
    samples.sort_by(f64::total_cmp);
    [samples[249], samples[9_749]]
}

fn wilson_ci(successes: usize, trials: usize) -> [f64; 2] {
    const Z: f64 = 1.959_963_984_540_054;
    let n = trials as f64;
    let proportion = successes as f64 / n;
    let denominator = 1.0 + Z * Z / n;
    let center = (proportion + Z * Z / (2.0 * n)) / denominator;
    let half_width =
        Z * ((proportion * (1.0 - proportion) / n + Z * Z / (4.0 * n * n)).sqrt()) / denominator;
    [
        (center - half_width).max(0.0),
        (center + half_width).min(1.0),
    ]
}

fn geometric_mean(log_ratios: &[f64]) -> Option<f64> {
    if log_ratios.is_empty() {
        return None;
    }
    Some((log_ratios.iter().sum::<f64>() / log_ratios.len() as f64).exp())
}

fn bootstrap_log_ratio_ci(log_ratios: &[f64], seed: u64) -> Option<[f64; 2]> {
    if log_ratios.is_empty() {
        return None;
    }
    let mut generator = XorShift64::new(seed);
    let mut samples = Vec::with_capacity(BOOTSTRAP_SAMPLES);
    for _ in 0..BOOTSTRAP_SAMPLES {
        let mut total = 0.0f64;
        for _ in 0..log_ratios.len() {
            total += log_ratios[generator.index(log_ratios.len())];
        }
        samples.push((total / log_ratios.len() as f64).exp());
    }
    samples.sort_by(f64::total_cmp);
    Some([samples[249], samples[9_749]])
}

fn mcnemar_exact(local_only: usize, upstream_only: usize) -> f64 {
    let discordant = local_only.saturating_add(upstream_only);
    if discordant == 0 {
        return 1.0;
    }
    let tail = local_only.min(upstream_only);
    let denominator = 2f64.powi(i32::try_from(discordant).unwrap_or(i32::MAX));
    let mut coefficient = 1.0f64;
    let mut sum = 1.0f64;
    for k in 1..=tail {
        coefficient *= (discordant.saturating_sub(k).saturating_add(1)) as f64 / k as f64;
        sum += coefficient;
    }
    (2.0 * sum / denominator).min(1.0)
}

fn to_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    fn index(&mut self, length: usize) -> usize {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        let modulus = u64::try_from(length).unwrap_or(u64::MAX);
        usize::try_from(value % modulus).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(implementation: Implementation, success: bool) -> ArmRecord {
        ArmRecord {
            implementation,
            success,
            timeout: false,
            environmental_invalid: false,
            failure_category: (!success).then_some(FailureCategory::AnswerMismatch),
            startup: None,
            catalog: None,
            calls: Vec::new(),
            ttft_ns: success.then_some(5),
            end_to_end_ns: success.then_some(10),
            stderr: None,
            answer_sha256: success.then(|| "0".repeat(64)),
        }
    }

    fn pair(index: u16, local: ArmRecord, upstream: ArmRecord) -> PairRecord {
        PairRecord {
            schema_version: 1,
            event: PairEventKind::Pair,
            run_id: "fixture".to_owned(),
            mode: BenchmarkMode::ProductionVerifiedOfficial,
            track: BenchmarkTrack::Protocol,
            scenario: "direct-crud".to_owned(),
            attempt: index,
            pair_index: Some(index),
            order: if index.is_multiple_of(2) {
                ArmOrder::LocalUpstream
            } else {
                ArmOrder::UpstreamLocal
            },
            local,
            upstream,
        }
    }

    #[test]
    fn includes_failures_in_paired_success_statistics() {
        let records = (0..COMPLETE_PAIRS)
            .map(|index| {
                pair(
                    index,
                    outcome(Implementation::Local, true),
                    outcome(Implementation::Upstream, index >= 12),
                )
            })
            .collect::<Vec<_>>();
        let summary = summarize(&records, 7).expect("complete summary");
        assert_eq!(summary.local.successes, COMPLETE_PAIRS);
        assert_eq!(summary.upstream.failures, 12);
        assert_eq!(summary.paired_success_difference, 0.1);
        assert!(summary.mcnemar_exact_p < 0.001);
        assert!(
            summary
                .paired_latency_ratio
                .is_some_and(|ratio| ratio < 1.0)
        );
        assert!(summary.paired_latency_ratio_ci95.is_some());
        assert!(summary.local.success_rate_wilson_ci95[0] < 1.0);
    }

    #[test]
    fn invalidates_environmental_imbalance() {
        let mut records = (0..COMPLETE_PAIRS)
            .map(|index| {
                pair(
                    index,
                    outcome(Implementation::Local, true),
                    outcome(Implementation::Upstream, true),
                )
            })
            .collect::<Vec<_>>();
        for attempt in COMPLETE_PAIRS..COMPLETE_PAIRS + 2 {
            let mut invalid = pair(
                attempt,
                outcome(Implementation::Local, false),
                outcome(Implementation::Upstream, false),
            );
            invalid.pair_index = None;
            invalid.local.environmental_invalid = true;
            invalid.local.failure_category = Some(FailureCategory::Environment);
            records.push(invalid);
        }
        let summary = summarize(&records, 9).expect("invalid summary still reports evidence");
        assert!(!summary.valid);
    }

    #[test]
    fn rejects_duplicate_attempts_and_uncounterbalanced_order() {
        let records = (0..COMPLETE_PAIRS)
            .map(|index| {
                pair(
                    index,
                    outcome(Implementation::Local, true),
                    outcome(Implementation::Upstream, true),
                )
            })
            .collect::<Vec<_>>();
        let mut duplicate = records.clone();
        duplicate[1].attempt = duplicate[0].attempt;
        assert!(summarize(&duplicate, 11).is_err());

        let mut one_order = records;
        for record in &mut one_order {
            record.order = ArmOrder::LocalUpstream;
        }
        assert!(summarize(&one_order, 11).is_err());
    }

    #[test]
    fn rejects_cross_envelope_and_malformed_retry_rows() {
        let mut records = (0..COMPLETE_PAIRS).map(fixture_pair).collect::<Vec<_>>();
        records[1].run_id = "other-envelope".to_owned();
        assert!(summarize(&records, 13).is_err());

        let mut records = (0..COMPLETE_PAIRS).map(fixture_pair).collect::<Vec<_>>();
        let mut retry = fixture_pair(COMPLETE_PAIRS);
        retry.pair_index = None;
        retry.local.environmental_invalid = true;
        retry.local.failure_category = Some(FailureCategory::Environment);
        retry.local.success = true;
        records.push(retry);
        assert!(summarize(&records, 13).is_err());
    }
}
