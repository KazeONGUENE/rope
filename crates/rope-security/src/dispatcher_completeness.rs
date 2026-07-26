//! Boot-time dispatcher-completeness check — a **new** CERBER capability
//! recommended by `docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md` §5.1
//! (finding C7 / M11).
//!
//! ## The problem this solves
//!
//! `rope-node`'s `rpc_auth::DESTRUCTIVE_METHODS` is a hand-maintained
//! constant array. Its own regression test,
//! `rpc_auth_destructive_list_locked`, only asserts the list does not
//! *shrink* — it has no way to notice that a brand-new mutating method was
//! added to the dispatcher and never triaged into the list at all. That
//! exact gap is what let `rope_registerDevice`, `rope_ingestTelemetry`, and
//! `rope_subscribeAgentToWallet` ship live, unauthenticated, and
//! state-mutating (finding C7).
//!
//! ## The fix
//!
//! This module is deliberately generic and has no opinion about *which*
//! methods exist — the caller (`rope-node`) supplies:
//!
//! 1. `all_registered` — every method name the dispatcher actually matches
//!    against, **derived mechanically from the dispatcher's own source
//!    text by a `build.rs` script** rather than hand-copied. This is the
//!    part that makes the check "dynamic, not list-locked": the input to
//!    [`verify`] can never silently drift out of sync with the real
//!    dispatcher, because it is regenerated on every build directly from
//!    `rpc_server.rs`.
//! 2. `buckets` — the curated classification lists (e.g. `DESTRUCTIVE_METHODS`,
//!    a `GOVERNANCE_SELF_AUTHENTICATED_METHODS` list for methods that carry
//!    their own independent signature check, and a `SAFE_READ_ONLY_METHODS`
//!    allowlist). Every bucket together must partition `all_registered`.
//!
//! [`verify`] fails (returns `Err`) if any registered method is not present
//! in *any* bucket (an unclassified mutator — the C7 class of bug) or is
//! present in *more than one* bucket (an authoring mistake that makes the
//! security posture of that method ambiguous). The caller decides what to
//! do with a failure; `rope-node` calls this at process startup and, by
//! default, refuses to bind its public listener until the report is clean
//! (fail-closed, per the audit's explicit recommendation), with a
//! documented escape hatch for operators who need to bring a node up while
//! triaging a newly-flagged method.

use std::collections::HashSet;

/// Result of a failed completeness check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletenessReport {
    /// Methods present in `all_registered` but absent from every bucket.
    /// Each of these is a potential unauthenticated mutator — treat any
    /// non-empty list here as a security incident, not a lint warning.
    pub unclassified: Vec<String>,
    /// Methods present in more than one bucket. Not exploitable on its own,
    /// but it means the buckets disagree about that method's authentication
    /// requirement, which is exactly the kind of ambiguity that produces a
    /// C7-shaped bug later.
    pub duplicates: Vec<String>,
}

impl CompletenessReport {
    pub fn is_clean(&self) -> bool {
        self.unclassified.is_empty() && self.duplicates.is_empty()
    }

    /// Single-line human-readable summary suitable for a `tracing::error!`
    /// or a startup-abort message.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.unclassified.is_empty() {
            parts.push(format!(
                "{} unclassified method(s): [{}]",
                self.unclassified.len(),
                self.unclassified.join(", ")
            ));
        }
        if !self.duplicates.is_empty() {
            parts.push(format!(
                "{} method(s) classified in more than one bucket: [{}]",
                self.duplicates.len(),
                self.duplicates.join(", ")
            ));
        }
        if parts.is_empty() {
            "clean".to_string()
        } else {
            parts.join("; ")
        }
    }
}

/// Verify that every method in `all_registered` appears in exactly one of
/// `buckets`. See module docs for the full rationale.
pub fn verify(all_registered: &[&str], buckets: &[&[&str]]) -> Result<(), CompletenessReport> {
    let mut membership_count: std::collections::HashMap<&str, u32> =
        std::collections::HashMap::new();
    for bucket in buckets {
        for method in *bucket {
            *membership_count.entry(*method).or_insert(0) += 1;
        }
    }

    let duplicates: Vec<String> = membership_count
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(m, _)| m.to_string())
        .collect();

    let classified: HashSet<&str> = membership_count.keys().copied().collect();
    let unclassified: Vec<String> = all_registered
        .iter()
        .filter(|m| !classified.contains(*m))
        .map(|s| s.to_string())
        .collect();

    if unclassified.is_empty() && duplicates.is_empty() {
        Ok(())
    } else {
        let mut report = CompletenessReport {
            unclassified,
            duplicates,
        };
        report.unclassified.sort();
        report.duplicates.sort();
        Err(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_trivially_clean() {
        assert!(verify(&[], &[]).is_ok());
    }

    #[test]
    fn fully_classified_registry_is_clean() {
        let destructive: &[&str] = &["rope_untieKnot", "rope_appendToLedger"];
        let safe: &[&str] = &["rope_globalStats", "rope_listStrings"];
        let all = ["rope_untieKnot", "rope_appendToLedger", "rope_globalStats", "rope_listStrings"];
        assert!(verify(&all, &[destructive, safe]).is_ok());
    }

    #[test]
    fn unclassified_method_is_caught() {
        let destructive: &[&str] = &["rope_untieKnot"];
        let safe: &[&str] = &["rope_globalStats"];
        // rope_registerDevice is present in the "live dispatcher" but was
        // never triaged into either bucket — this is exactly the C7 bug
        // this check exists to catch.
        let all = ["rope_untieKnot", "rope_globalStats", "rope_registerDevice"];
        let err = verify(&all, &[destructive, safe]).unwrap_err();
        assert_eq!(err.unclassified, vec!["rope_registerDevice".to_string()]);
        assert!(err.duplicates.is_empty());
        assert!(!err.is_clean());
        assert!(err.summary().contains("rope_registerDevice"));
    }

    #[test]
    fn duplicate_classification_is_caught() {
        let destructive: &[&str] = &["rope_untieKnot"];
        // Authoring mistake: same method in two buckets with different
        // (and contradictory) authentication postures.
        let safe: &[&str] = &["rope_untieKnot", "rope_globalStats"];
        let all = ["rope_untieKnot", "rope_globalStats"];
        let err = verify(&all, &[destructive, safe]).unwrap_err();
        assert_eq!(err.duplicates, vec!["rope_untieKnot".to_string()]);
        assert!(err.unclassified.is_empty());
    }

    #[test]
    fn multiple_unclassified_methods_all_reported_sorted() {
        let safe: &[&str] = &["rope_globalStats"];
        let all = ["rope_globalStats", "rope_z_new_mutator", "rope_a_new_mutator"];
        let err = verify(&all, &[safe]).unwrap_err();
        assert_eq!(
            err.unclassified,
            vec!["rope_a_new_mutator".to_string(), "rope_z_new_mutator".to_string()]
        );
    }
}
