//! Config-drift detector — a **new** CERBER capability recommended by
//! `docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md` §5.1, sitting
//! alongside the boot-time dispatcher-completeness check
//! ([`crate::dispatcher_completeness`]).
//!
//! ## The problem this solves
//!
//! Several audit findings were not bugs in logic but bugs in *posture*:
//! the running configuration silently diverged from the security baseline
//! the operator believed was in effect. Examples directly from the audit:
//!
//! - H7: `reth-rope.service` shipped with `--http.api eth,net,web3,admin`
//!   and a wildcard CORS origin — a config value, not a code path — for an
//!   unknown period before it was caught by manual review.
//! - M4/M5: `SECURITY_POLICY.md` described a Timelock/Safe posture that
//!   had drifted from what was actually deployed on-chain.
//! - F1 (bridge audit, 2026-07-20): the Arbitrum vault's `paused` flag
//!   silently stayed `false` while the Rope-side minter it depended on was
//!   wired for `paused=true`, an asymmetry no single-service health check
//!   would surface.
//!
//! None of these are things a request-time [`crate::guard::RequestGuard`]
//! check can catch — they are *ambient* facts about how a process or a
//! contract is currently configured, not properties of an individual
//! inbound request. They need a periodic background comparison against a
//! declared baseline.
//!
//! ## The fix
//!
//! This module is transport- and source-agnostic: it does not know how to
//! fetch a config value (that varies wildly — a CLI flag, an env var, an
//! on-chain view call, a systemd directive) and does not care. Callers
//! supply a [`ConfigBaseline`] (name → expected value, as a string) and a
//! matching *observed* snapshot (name → actual value); [`compare`] finds
//! every field where the two disagree. `rope-explorer` (and, eventually,
//! `rope-node`) is expected to run this on a periodic background timer,
//! logging/alerting whenever [`DriftReport::is_clean`] is `false`.
//!
//! This is deliberately WATCH, not STRIKE: drift is reported, never
//! auto-remediated, because forcibly reverting a live config the operator
//! may have changed on purpose (e.g. a deliberate emergency pause) would
//! itself be a dangerous, high-blast-radius action for an autonomous
//! component to take.

use std::collections::BTreeMap;

/// A declared set of expected `(name, value)` pairs. Both are opaque
/// strings so this module stays agnostic to the underlying config
/// source — normalise booleans, addresses, feature flags, etc. to a
/// canonical string form before constructing the baseline and the
/// observed snapshot so that comparisons are meaningful (e.g. always
/// lower-case hex addresses on both sides).
#[derive(Debug, Clone, Default)]
pub struct ConfigBaseline {
    expected: BTreeMap<String, String>,
}

impl ConfigBaseline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, name: impl Into<String>, expected_value: impl Into<String>) -> Self {
        self.expected.insert(name.into(), expected_value.into());
        self
    }

    pub fn len(&self) -> usize {
        self.expected.len()
    }

    pub fn is_empty(&self) -> bool {
        self.expected.is_empty()
    }
}

/// One detected mismatch between the baseline and the observed snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftEntry {
    pub name: String,
    pub expected: String,
    pub observed: Option<String>,
}

/// Result of a drift comparison.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriftReport {
    /// Fields present in the baseline whose observed value differs from
    /// (or is missing when the baseline requires) the expected value.
    pub drifted: Vec<DriftEntry>,
}

impl DriftReport {
    pub fn is_clean(&self) -> bool {
        self.drifted.is_empty()
    }

    pub fn summary(&self) -> String {
        if self.drifted.is_empty() {
            return "clean".to_string();
        }
        self.drifted
            .iter()
            .map(|d| match &d.observed {
                Some(observed) => format!("{}: expected `{}`, observed `{}`", d.name, d.expected, observed),
                None => format!("{}: expected `{}`, observed <missing>", d.name, d.expected),
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Compare an observed config snapshot against a declared baseline.
///
/// Only fields present in `baseline` are checked — an observed snapshot is
/// permitted to carry extra fields the baseline does not care about.
/// A baseline field absent from `observed` is reported as drift with
/// `observed: None`, distinct from a present-but-wrong value, so operators
/// can immediately tell "this flag disappeared" from "this flag changed".
pub fn compare(
    baseline: &ConfigBaseline,
    observed: &BTreeMap<String, String>,
) -> DriftReport {
    let mut drifted = Vec::new();
    for (name, expected) in &baseline.expected {
        match observed.get(name) {
            Some(actual) if actual == expected => {}
            Some(actual) => drifted.push(DriftEntry {
                name: name.clone(),
                expected: expected.clone(),
                observed: Some(actual.clone()),
            }),
            None => drifted.push(DriftEntry {
                name: name.clone(),
                expected: expected.clone(),
                observed: None,
            }),
        }
    }
    DriftReport { drifted }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn matching_snapshot_is_clean() {
        let baseline = ConfigBaseline::new()
            .with("reth.http_api", "eth,net,web3")
            .with("reth.cors_origin", "https://dcscan.io");
        let snap = observed(&[
            ("reth.http_api", "eth,net,web3"),
            ("reth.cors_origin", "https://dcscan.io"),
        ]);
        let report = compare(&baseline, &snap);
        assert!(report.is_clean());
    }

    #[test]
    fn h7_style_drift_is_detected() {
        // Mirrors the actual H7 finding: admin API + wildcard CORS crept
        // into a config that was supposed to be eth,net,web3 + a pinned
        // origin.
        let baseline = ConfigBaseline::new()
            .with("reth.http_api", "eth,net,web3")
            .with("reth.cors_origin", "https://dcscan.io");
        let snap = observed(&[
            ("reth.http_api", "eth,net,web3,admin"),
            ("reth.cors_origin", "*"),
        ]);
        let report = compare(&baseline, &snap);
        assert!(!report.is_clean());
        assert_eq!(report.drifted.len(), 2);
        assert!(report.summary().contains("admin"));
        assert!(report.summary().contains("*"));
    }

    #[test]
    fn missing_field_reported_distinctly_from_wrong_value() {
        let baseline = ConfigBaseline::new().with("bridge.arbitrum_vault.paused", "true");
        let snap: BTreeMap<String, String> = BTreeMap::new();
        let report = compare(&baseline, &snap);
        assert_eq!(report.drifted.len(), 1);
        assert_eq!(report.drifted[0].observed, None);
        assert!(report.summary().contains("<missing>"));
    }

    #[test]
    fn extra_observed_fields_not_in_baseline_are_ignored() {
        let baseline = ConfigBaseline::new().with("a", "1");
        let snap = observed(&[("a", "1"), ("b", "unexpected-but-not-tracked")]);
        assert!(compare(&baseline, &snap).is_clean());
    }

    #[test]
    fn empty_baseline_is_always_clean() {
        let baseline = ConfigBaseline::new();
        let snap = observed(&[("anything", "goes")]);
        assert!(compare(&baseline, &snap).is_clean());
        assert!(baseline.is_empty());
    }
}
