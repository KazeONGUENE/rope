// =============================================================================
// Prometheus-style counter metrics
// =============================================================================
//
// All counters live behind a single `ComplianceMetrics` struct so the
// HTTP handler and the periodic reporter can share one instance. We
// intentionally keep this module dependency-free of any global static
// (no `lazy_static`, no `prometheus::default_registry()`) — every
// process owns its own registry, which keeps tests independent.
// =============================================================================

use prometheus::{IntCounter, Registry};

/// All compliance-agent counters.
pub struct ComplianceMetrics {
    pub registry: Registry,

    pub gdpr_requests_total: IntCounter,
    pub gdpr_requests_approved: IntCounter,
    pub gdpr_requests_rejected: IntCounter,
    pub gdpr_knots_untied_success: IntCounter,
    pub gdpr_knots_untied_failure: IntCounter,
    pub gdpr_testimony_anchored: IntCounter,
    pub gdpr_testimony_anchor_failed: IntCounter,

    pub mifid_events_received: IntCounter,
    pub mifid_digests_anchored: IntCounter,
    pub mifid_digests_anchor_failed: IntCounter,

    pub dora_incidents_received: IntCounter,
    pub dora_digests_anchored: IntCounter,
    pub dora_digests_anchor_failed: IntCounter,
}

impl ComplianceMetrics {
    pub fn new() -> Self {
        let registry = Registry::new();
        let gdpr_requests_total = mk(
            &registry,
            "gdpr_requests_total",
            "Total Art. 17 requests received",
        );
        let gdpr_requests_approved = mk(
            &registry,
            "gdpr_requests_approved",
            "Art. 17 requests passing structural validation",
        );
        let gdpr_requests_rejected = mk(
            &registry,
            "gdpr_requests_rejected",
            "Art. 17 requests rejected by structural validation",
        );
        let gdpr_knots_untied_success = mk(
            &registry,
            "gdpr_knots_untied_success",
            "rope_untieKnot calls that returned a tombstone audit hash",
        );
        let gdpr_knots_untied_failure = mk(
            &registry,
            "gdpr_knots_untied_failure",
            "rope_untieKnot calls that did not return a tombstone audit hash",
        );
        let gdpr_testimony_anchored = mk(
            &registry,
            "gdpr_testimony_anchored",
            "GdprArticle17 testimony envelopes successfully anchored",
        );
        let gdpr_testimony_anchor_failed = mk(
            &registry,
            "gdpr_testimony_anchor_failed",
            "GdprArticle17 testimony envelopes that failed to anchor",
        );
        let mifid_events_received = mk(
            &registry,
            "mifid_events_received",
            "MiFID II trade events accepted into the batch buffer",
        );
        let mifid_digests_anchored = mk(
            &registry,
            "mifid_digests_anchored",
            "MiFID II digests successfully anchored",
        );
        let mifid_digests_anchor_failed = mk(
            &registry,
            "mifid_digests_anchor_failed",
            "MiFID II digests that failed to anchor",
        );
        let dora_incidents_received = mk(
            &registry,
            "dora_incidents_received",
            "DORA incident reports accepted into the batch buffer",
        );
        let dora_digests_anchored = mk(
            &registry,
            "dora_digests_anchored",
            "DORA incident digests successfully anchored",
        );
        let dora_digests_anchor_failed = mk(
            &registry,
            "dora_digests_anchor_failed",
            "DORA incident digests that failed to anchor",
        );
        Self {
            registry,
            gdpr_requests_total,
            gdpr_requests_approved,
            gdpr_requests_rejected,
            gdpr_knots_untied_success,
            gdpr_knots_untied_failure,
            gdpr_testimony_anchored,
            gdpr_testimony_anchor_failed,
            mifid_events_received,
            mifid_digests_anchored,
            mifid_digests_anchor_failed,
            dora_incidents_received,
            dora_digests_anchored,
            dora_digests_anchor_failed,
        }
    }

    /// Render the registry in Prometheus text format.
    pub fn render(&self) -> String {
        use prometheus::Encoder;
        let encoder = prometheus::TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buf = Vec::new();
        encoder.encode(&metric_families, &mut buf).ok();
        String::from_utf8(buf).unwrap_or_default()
    }
}

impl Default for ComplianceMetrics {
    fn default() -> Self {
        Self::new()
    }
}

fn mk(registry: &Registry, name: &str, help: &str) -> IntCounter {
    let c = IntCounter::new(name, help).expect("metric");
    registry
        .register(Box::new(c.clone()))
        .expect("register metric");
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_render_includes_all_counters() {
        let m = ComplianceMetrics::new();
        m.gdpr_requests_total.inc();
        m.mifid_events_received.inc_by(3);
        let txt = m.render();
        assert!(txt.contains("gdpr_requests_total 1"));
        assert!(txt.contains("mifid_events_received 3"));
        assert!(txt.contains("dora_digests_anchored"));
    }

    #[test]
    fn each_metric_starts_at_zero() {
        let m = ComplianceMetrics::new();
        assert_eq!(m.gdpr_requests_total.get(), 0);
        assert_eq!(m.gdpr_knots_untied_success.get(), 0);
        assert_eq!(m.dora_incidents_received.get(), 0);
    }
}
