// =============================================================================
// Reporting — periodic batched MiFID II + DORA digests
// =============================================================================
//
// The HTTP listener accepts MiFID II `event` and DORA `incident`
// submissions and stuffs them into in-memory buffers held by the
// `PeriodicReporter`. Every `reporting_interval` (default 15 min) the
// reporter:
//
//   1. Drains both buffers (under the per-buffer mutex).
//   2. Builds a `MiFidIIDigest` and a `DoraIncidentDigest`.
//   3. Wraps each digest in a `ComplianceTestimonyEnvelope` and ships
//      it to `rope_appendToLedger` via the `AnchorClient`.
//   4. Emits Prometheus counters for {anchored, anchor_failed} per
//      digest kind.
//
// Drained-but-failed digests are NOT re-queued automatically. The
// rationale is that MiFID II / DORA digests are time-bounded — a
// digest covering 14:00-14:15 UTC has no business being re-anchored
// at 15:00 UTC under the same period stamp. We log the failure with
// `tracing::error!` so the operator can investigate and, if needed,
// manually re-anchor via the CLI.
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::anchor::{AnchorClient, AnchorReceipt};
use crate::testimony::{
    ComplianceTestimony, ComplianceTestimonyEnvelope, DoraIncident, DoraIncidentDigest,
    MiFidIIDigest, MiFidIIEvent,
};

/// Buffers and batch logic. Cheap to clone (Arc internally), so the
/// HTTP handler and the periodic loop share one instance.
#[derive(Clone)]
pub struct PeriodicReporter {
    inner: Arc<ReporterInner>,
}

struct ReporterInner {
    anchor: AnchorClient,
    interval: Duration,
    max_events: usize,
    mifid_buffer: Mutex<BufferState<MiFidIIEvent>>,
    dora_buffer: Mutex<BufferState<DoraIncident>>,
    /// Notify channel used by tests + the optional CLI `flush-now`
    /// command to force a tick out-of-cadence.
    flush_now: Notify,
}

struct BufferState<T> {
    /// Inclusive period start of the current batch (UTC seconds).
    period_start: i64,
    items: Vec<T>,
    /// Stats (across the lifetime of the process).
    lifetime_received: u64,
    lifetime_dropped: u64,
}

impl<T> BufferState<T> {
    fn new(now: i64) -> Self {
        Self {
            period_start: now,
            items: Vec::new(),
            lifetime_received: 0,
            lifetime_dropped: 0,
        }
    }
}

/// Outcome of a single tick. Useful for tests and the CLI flush-now
/// command.
#[derive(Debug, Clone)]
pub struct TickOutcome {
    pub mifid_anchored: Option<AnchorReceipt>,
    pub mifid_event_count: u64,
    pub dora_anchored: Option<AnchorReceipt>,
    pub dora_incident_count: u64,
    pub anchor_failures: u64,
}

impl PeriodicReporter {
    pub fn new(anchor: AnchorClient, interval: Duration, max_events: usize) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            inner: Arc::new(ReporterInner {
                anchor,
                interval,
                max_events: max_events.max(1),
                mifid_buffer: Mutex::new(BufferState::new(now)),
                dora_buffer: Mutex::new(BufferState::new(now)),
                flush_now: Notify::new(),
            }),
        }
    }

    /// Submit a MiFID II trade event into the in-memory buffer. Drops
    /// the event (and increments `lifetime_dropped`) if the buffer
    /// already holds `max_events` entries — a defensive cap so a
    /// runaway emitter cannot OOM the agent.
    pub fn record_mifid_event(&self, event: MiFidIIEvent) {
        let mut b = self.inner.mifid_buffer.lock();
        b.lifetime_received += 1;
        if b.items.len() >= self.inner.max_events {
            b.lifetime_dropped += 1;
            tracing::warn!(
                target: "compliance::reporting",
                buffer = "mifid",
                cap = self.inner.max_events,
                "dropping event — batch buffer full"
            );
            return;
        }
        b.items.push(event);
    }

    pub fn record_dora_incident(&self, incident: DoraIncident) {
        let mut b = self.inner.dora_buffer.lock();
        b.lifetime_received += 1;
        if b.items.len() >= self.inner.max_events {
            b.lifetime_dropped += 1;
            tracing::warn!(
                target: "compliance::reporting",
                buffer = "dora",
                cap = self.inner.max_events,
                "dropping incident — batch buffer full"
            );
            return;
        }
        b.items.push(incident);
    }

    pub fn buffer_stats(&self) -> ReporterStats {
        let m = self.inner.mifid_buffer.lock();
        let d = self.inner.dora_buffer.lock();
        ReporterStats {
            mifid_pending: m.items.len() as u64,
            mifid_lifetime_received: m.lifetime_received,
            mifid_lifetime_dropped: m.lifetime_dropped,
            dora_pending: d.items.len() as u64,
            dora_lifetime_received: d.lifetime_received,
            dora_lifetime_dropped: d.lifetime_dropped,
            interval_secs: self.inner.interval.as_secs(),
        }
    }

    /// Force one tick to fire out-of-cadence. Returns immediately —
    /// the actual flush happens inside the running `run()` loop. If no
    /// loop is running, the notification is queued and consumed on
    /// the next start.
    pub fn flush_now(&self) {
        self.inner.flush_now.notify_one();
    }

    /// Drain the buffers and anchor the resulting digests. Public so
    /// tests can drive a single tick without spawning the loop.
    pub async fn tick_once(&self) -> TickOutcome {
        let now = chrono::Utc::now().timestamp();

        // ---- MiFID II ----
        let (mifid_events, mifid_period_start) = {
            let mut b = self.inner.mifid_buffer.lock();
            let drained: Vec<MiFidIIEvent> = std::mem::take(&mut b.items);
            let start = b.period_start;
            b.period_start = now;
            (drained, start)
        };
        let mifid_event_count = mifid_events.len() as u64;
        let mut anchor_failures = 0u64;
        let mifid_anchored = if mifid_events.is_empty() {
            None
        } else {
            let digest = MiFidIIDigest::build(mifid_period_start, now, &mifid_events);
            let envelope = ComplianceTestimonyEnvelope::seal(
                "compliance",
                self.inner.anchor.agent_wallet(),
                ComplianceTestimony::MiFidIIDigest(digest),
                now,
            );
            match self.inner.anchor.anchor(&envelope).await {
                Ok(receipt) => {
                    tracing::info!(
                        target: "compliance::reporting",
                        digest = "mifid_ii",
                        events = mifid_event_count,
                        knot = %receipt.knot_string_id,
                        "MiFID II digest anchored"
                    );
                    Some(receipt)
                }
                Err(e) => {
                    anchor_failures += 1;
                    tracing::error!(
                        target: "compliance::reporting",
                        digest = "mifid_ii",
                        events = mifid_event_count,
                        error = %e,
                        "MiFID II digest anchor FAILED — events lost from on-chain audit"
                    );
                    None
                }
            }
        };

        // ---- DORA ----
        let (dora_incidents, dora_period_start) = {
            let mut b = self.inner.dora_buffer.lock();
            let drained: Vec<DoraIncident> = std::mem::take(&mut b.items);
            let start = b.period_start;
            b.period_start = now;
            (drained, start)
        };
        let dora_incident_count = dora_incidents.len() as u64;
        let dora_anchored = if dora_incidents.is_empty() {
            None
        } else {
            let digest = DoraIncidentDigest::build(dora_period_start, now, &dora_incidents);
            let envelope = ComplianceTestimonyEnvelope::seal(
                "compliance",
                self.inner.anchor.agent_wallet(),
                ComplianceTestimony::DoraIncidentDigest(digest),
                now,
            );
            match self.inner.anchor.anchor(&envelope).await {
                Ok(receipt) => {
                    tracing::info!(
                        target: "compliance::reporting",
                        digest = "dora",
                        incidents = dora_incident_count,
                        knot = %receipt.knot_string_id,
                        "DORA incident digest anchored"
                    );
                    Some(receipt)
                }
                Err(e) => {
                    anchor_failures += 1;
                    tracing::error!(
                        target: "compliance::reporting",
                        digest = "dora",
                        incidents = dora_incident_count,
                        error = %e,
                        "DORA incident digest anchor FAILED — incidents lost from on-chain audit"
                    );
                    None
                }
            }
        };

        TickOutcome {
            mifid_anchored,
            mifid_event_count,
            dora_anchored,
            dora_incident_count,
            anchor_failures,
        }
    }

    /// Long-running loop. Runs until the cancel notify is fired.
    /// Designed to be `tokio::spawn`ed.
    pub async fn run(self, cancel: Arc<Notify>) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(self.inner.interval) => {
                    let _ = self.tick_once().await;
                }
                _ = self.inner.flush_now.notified() => {
                    tracing::info!(target: "compliance::reporting", "flush_now() requested — ticking");
                    let _ = self.tick_once().await;
                }
                _ = cancel.notified() => {
                    tracing::info!(target: "compliance::reporting", "cancel signal received — flushing one final time");
                    let _ = self.tick_once().await;
                    break;
                }
            }
        }
    }
}

/// Snapshot of the reporter's buffers, exposed via the HTTP `/health`
/// endpoint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReporterStats {
    pub mifid_pending: u64,
    pub mifid_lifetime_received: u64,
    pub mifid_lifetime_dropped: u64,
    pub dora_pending: u64,
    pub dora_lifetime_received: u64,
    pub dora_lifetime_dropped: u64,
    pub interval_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::testing::MockRopeRpcClient;
    use crate::rpc::RopeRpcClient;
    use crate::testimony::DoraSeverity;
    use serde_json::json;

    fn ev(id: &str) -> MiFidIIEvent {
        MiFidIIEvent {
            trade_id: id.to_string(),
            instrument: "DC-FAT".to_string(),
            venue: "dcswap".to_string(),
            buyer: "0xb".to_string(),
            seller: "0xs".to_string(),
            notional: 1000,
            currency: "USDC".to_string(),
            executed_at: 1700000000,
        }
    }

    fn inc(id: &str) -> DoraIncident {
        DoraIncident {
            incident_id: id.to_string(),
            severity: DoraSeverity::High,
            description: "test".into(),
            detected_at: 1700000000,
            resolved_at: None,
            affected_service: "rope-node".to_string(),
        }
    }

    #[tokio::test]
    async fn tick_with_empty_buffers_anchors_nothing() {
        let mock = Arc::new(MockRopeRpcClient::new());
        let anchor = AnchorClient::new(mock as Arc<dyn RopeRpcClient>, "0xC005");
        let r = PeriodicReporter::new(anchor, Duration::from_secs(1), 1024);
        let outcome = r.tick_once().await;
        assert!(outcome.mifid_anchored.is_none());
        assert!(outcome.dora_anchored.is_none());
        assert_eq!(outcome.anchor_failures, 0);
    }

    #[tokio::test]
    async fn tick_anchors_both_digests_when_buffers_have_data() {
        let mock = Arc::new(MockRopeRpcClient::new());
        // Two appends expected (mifid + dora).
        mock.enqueue_ok(
            "rope_appendToLedger",
            json!({"index": 1, "hash": "0xmifidknot"}),
        );
        mock.enqueue_ok(
            "rope_appendToLedger",
            json!({"index": 1, "hash": "0xdoraknot"}),
        );

        let anchor = AnchorClient::new(mock.clone() as Arc<dyn RopeRpcClient>, "0xC005");
        let r = PeriodicReporter::new(anchor, Duration::from_secs(1), 1024);
        r.record_mifid_event(ev("t1"));
        r.record_mifid_event(ev("t2"));
        r.record_dora_incident(inc("i1"));

        let outcome = r.tick_once().await;
        let mifid = outcome.mifid_anchored.unwrap();
        let dora = outcome.dora_anchored.unwrap();
        assert_eq!(mifid.knot_string_id, "0xmifidknot");
        assert_eq!(dora.knot_string_id, "0xdoraknot");
        assert_eq!(outcome.mifid_event_count, 2);
        assert_eq!(outcome.dora_incident_count, 1);
        assert_eq!(outcome.anchor_failures, 0);

        // Verify the wire format of one of the calls — the metadata
        // must contain a JSON-string-encoded envelope and the agent id.
        let calls = mock.calls_for("rope_appendToLedger");
        assert_eq!(calls.len(), 2);
        let metadata = calls[0].params.as_array().unwrap()[1]
            .get("metadata")
            .unwrap();
        assert_eq!(
            metadata.get("agent_id").unwrap().as_str().unwrap(),
            "compliance"
        );
        assert_eq!(
            metadata.get("testimony_label").unwrap().as_str().unwrap(),
            "mifid_ii_digest"
        );
    }

    #[tokio::test]
    async fn buffer_drains_after_tick() {
        let mock = Arc::new(MockRopeRpcClient::new());
        mock.enqueue_ok("rope_appendToLedger", json!({"index": 1, "hash": "0xknot"}));
        let anchor = AnchorClient::new(mock as Arc<dyn RopeRpcClient>, "0xC005");
        let r = PeriodicReporter::new(anchor, Duration::from_secs(1), 1024);
        r.record_mifid_event(ev("t1"));
        let stats_before = r.buffer_stats();
        assert_eq!(stats_before.mifid_pending, 1);
        let _ = r.tick_once().await;
        let stats_after = r.buffer_stats();
        assert_eq!(stats_after.mifid_pending, 0);
        assert_eq!(stats_after.mifid_lifetime_received, 1);
    }

    #[tokio::test]
    async fn buffer_caps_at_max_events_and_records_drops() {
        let mock = Arc::new(MockRopeRpcClient::new());
        let anchor = AnchorClient::new(mock as Arc<dyn RopeRpcClient>, "0xC005");
        let r = PeriodicReporter::new(anchor, Duration::from_secs(1), 2);
        r.record_mifid_event(ev("t1"));
        r.record_mifid_event(ev("t2"));
        r.record_mifid_event(ev("t3")); // dropped
        let stats = r.buffer_stats();
        assert_eq!(stats.mifid_pending, 2);
        assert_eq!(stats.mifid_lifetime_received, 3);
        assert_eq!(stats.mifid_lifetime_dropped, 1);
    }

    #[tokio::test]
    async fn anchor_failure_is_counted() {
        let mock = Arc::new(MockRopeRpcClient::new());
        // Force an error on the next anchor call.
        mock.enqueue_err(
            "rope_appendToLedger",
            crate::rpc::RpcClientError::RpcError {
                code: -32603,
                message: "node down".into(),
            },
        );
        let anchor = AnchorClient::new(mock as Arc<dyn RopeRpcClient>, "0xC005");
        let r = PeriodicReporter::new(anchor, Duration::from_secs(1), 1024);
        r.record_mifid_event(ev("t1"));
        let outcome = r.tick_once().await;
        assert_eq!(outcome.anchor_failures, 1);
        assert!(outcome.mifid_anchored.is_none());
    }
}
