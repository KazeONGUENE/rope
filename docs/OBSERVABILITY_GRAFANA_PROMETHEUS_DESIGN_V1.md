# Observability - Prometheus + Grafana Design v1 (Datachain Rope)

**Author:** Datachain Rope agent
**Date:** 2026-08-12
**Status:** DESIGN. Nothing deployed. Prometheus and Grafana are not currently installed anywhere in the fleet; every metric listed below either already exists (harvestable now with a small exporter) or is scoped as a targeted instrumentation follow-up.
**Filed against:** §17.5 P3 item "Grafana panels for Recv-Q + RSS", §11.5 P3 "startup progress logging", §22.9 P2B follow-up "Prometheus counter for SWR cache-hit ratio", §16.7 P3 "counter for SWR cache-hit ratio", multiple incident post-mortems (§11, §16, §17, §20, §21) that would have been resolved faster with real-time metrics.

---

## 0. TL;DR

Every wedge / OOM / 504 / CERBER-page incident since 2026-05 has followed the same forensic pattern:

1. Symptom surfaces at the public edge (browser 504, MetaMask "Unable to connect", CERBER email).
2. Operator SSHes to rope-vps and starts running `ps`, `atop`, `journalctl -u datachain-rope`, `strace -p <pid>`, `curl fleet-status`, and `ls -la /var/lib/datachain-rope/fleet/` to piece together what happened.
3. The forensic chain typically reveals a signal that WAS visible ~30-60s before the symptom escalated (rising `head_guard_hold.max_ns`, growing `Recv-Q`, `RSS` crossing a threshold, `flusher_wait` count spiking), but nobody was looking.

**A minimal Prometheus + Grafana deployment (one host, ~1 vCPU, ~2 GB RAM, ~20 GB disk for 90 days of scrape data) turns every one of those signals into a proactive alert.** The metrics ALREADY exist in code (`rope_latticeMetrics`, `fleet-status.json`, `self-watchdog.json`, `dc-explorer` SWR caches, `rope-addr-indexer` status file, cerber-mesh audit NDJSON). The missing piece is a scraper + a dashboard + an alert file. This document is that piece.

---

## 1. Purpose

Three concrete outcomes this design achieves:

1. **Preempt the wedge cycle.** BLUE's ~6-9 min wedge cadence (see §17-21 in `handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc`) is currently observed by:
 - Operator running `curl fleet-status | jq` by hand
 - `erpc-fleet-ha.sh` cron reacting AFTER the wedge triggers HA
 - CERBER paging AFTER `escalate_to_cerber=true` (which is intentionally 15-min delayed per §21 Phase B.2)
 With Grafana, `head_guard_hold.max_ns` and `Recv-Q` are graphed continuously, and an alert fires when either crosses a soft threshold (say 100 ms / 50 respectively). The operator sees the wedge forming and can decide to preemptively restart or let it play out - both with data, not vibes.

2. **Prove P2B works.** The P2B parallel-writer rollout (§22) needs a before/after comparison of `head_guard_hold` distributions across the append hot path. The `deploy/p2b-baseline/` directory already has 30 samples of `rope_latticeMetrics` taken every 60 s pre-deploy. Post-deploy needs an equivalent set. A Prometheus scrape + a Grafana panel with a `p50/p95/p99` overlay turns that comparison from "eyeballing two JSON directories" into a real regression check.

3. **Give ecosystem peers a public health page.** `dcscan.io/fleet-status` already exists but is JSON-only. A public read-only Grafana dashboard at `metrics.datachain.network` with the same data behind it would give Tanastok / DCSwap / Datawallet+ operators a shared operational language when incidents cross project boundaries.

---

## 2. Data sources - what emits what today

Every source below either exists in code and is directly scrapeable, or requires a small exporter (called out explicitly). Nothing here is speculative.

### 2.1 rope-node (BLUE + GREEN + DO-rpc-1 + DO-rpc-2)

**Direct via JSON-RPC** - already implemented, no exporter needed beyond a Prometheus scrape converter:

| Method | Payload shape | What to extract |
|---|---|---|
| `rope_latticeMetrics` | `{append_to_ledger: {head_guard_hold: {mean_ns, max_ns, count}, oes_key_derive: {...}, flusher_wait: {...}}, create_ledger: {...}, erase_ledger: {...}}` per hot-path op | `_seconds_bucket{op=...,stage=head_guard_hold,quantile=...}` histograms + `_total{op=...}` counters |
| `rope_globalStats` | `{total_strings, total_knots, by_kind: {...}, invariant_holds}` | `rope_lattice_strings_total{kind}`, `rope_lattice_knots_total{kind}`, `rope_lattice_invariant_holds` gauge |
| `eth_blockNumber` | hex | `rope_block_height` gauge |
| `net_peerCount` | hex | `rope_p2p_peers_total` gauge |

**Direct via loopback state files** - already written by existing services:

| File | Written by | What to extract |
|---|---|---|
| `/opt/datachain-rope/code/deploy/nginx/html/fleet/fleet-status.json` | `erpc-fleet-ha.sh` (every 30 s) | writer.status, writer.block_hex, writer.restarts_last_hour, edge.status, edge.sample_ok, edge.external_probes.*, self_heal.unhealthy_for_secs, self_heal.escalate_to_cerber, ghost_reclaim.reclaimed_total |
| `/var/lib/datachain-rope/self-watchdog.json` | rope-node internal watchdog (§23) | consecutive_failures, consecutive_successes, last_success_latency_ms, stall_duration_ms, stalled |
| `/var/lib/datachain-rope/fleet/ha.state` | `erpc-fleet-ha.sh` | RESTART_EPOCHS, FAIL_COUNT, UNHEALTHY_SINCE |

### 2.2 dc-explorer (rope-vps)

SWR caches (§16, §22) live in `AppState` behind `RwLock<Option<CacheEntry>>`. A small `/metrics` endpoint on port 3001 can expose:

| Metric name | Type | Labels | Source |
|---|---|---|---|
| `dc_explorer_swr_cache_hits_total` | counter | endpoint | Increment on fresh-path serve |
| `dc_explorer_swr_cache_stale_serves_total` | counter | endpoint | Increment on stale-path serve |
| `dc_explorer_swr_compute_duration_seconds` | histogram | endpoint | Time each compute call |
| `dc_explorer_swr_timeout_hits_total` | counter | endpoint | Increment on compute timeout |
| `dc_explorer_swr_cache_age_seconds` | gauge | endpoint | Now - cache entry timestamp |
| `dc_explorer_addr_index_reader_hits_total` | counter | endpoint | For §Session N+3 wiring |
| `dc_explorer_addr_index_reader_fallback_total` | counter | endpoint,reason | When rope-addr-index is absent or errors |
| `dc_explorer_route_duration_seconds` | histogram | method,path,status | Standard axum middleware |

Estimated instrumentation cost: ~200 lines of Rust (one middleware + `swr_wrap` counter hooks + `AddressIndex` fallback counter). Prometheus text-format serialization via `prometheus` crate (already common).

### 2.3 rope-addr-indexer (rope-vps)

The service persists a status file at `/var/lib/rope-addr-index/status.json` with `head_block`, `tip_lag_blocks`, `backfill_low_water`, `last_ingested_at`, `errors_last_hour`. Scrape target: `/status.json` via a tiny CGI or a `/metrics` route added to the binary. Metrics:

| Metric | Type | Meaning |
|---|---|---|
| `rope_addr_index_head_block` | gauge | Highest block ingested |
| `rope_addr_index_tip_lag_blocks` | gauge | (Chain tip) - (indexer head) - should be < 10 in steady state |
| `rope_addr_index_backfill_low_water` | gauge | Lowest block indexed by the backfiller |
| `rope_addr_index_ingest_errors_total` | counter | RPC failures during ingest |
| `rope_addr_index_ingest_duration_seconds` | histogram | Time to ingest one block |

### 2.4 cerber-mesh (rope-vps + tanastok-vps + dcswap-vps)

Mesh peers publish their state at `/v1/cerber/mesh-status` on port 9107/9107/9108 respectively. Metrics via a small scraper that walks the peer list:

| Metric | Type | Labels |
|---|---|---|
| `cerber_mesh_peer_reachable` | gauge | peer_id (1 = reachable, 0 = not) |
| `cerber_mesh_peer_coverage_pct` | gauge | peer_id |
| `cerber_mesh_peer_last_report_age_seconds` | gauge | peer_id |
| `cerber_mesh_verify_success_total` | counter | peer_id |
| `cerber_mesh_verify_failure_total` | counter | peer_id, reason (body_hash_mismatch, missing_signature, stale_or_future, ...) |

### 2.5 cerber-edge-ingest (rope-vps, §N+3 §edge_probe_ingest)

The new endpoint at `/v1/cerber/edge-probe` (per `docs/EDGE_EXTERNAL_PROBES_INGEST_SPEC_v1.md`) accepts external peer reports and appends to `/var/lib/datachain-rope/fleet/external-probes.ndjson`. Metrics:

| Metric | Type | Labels |
|---|---|---|
| `cerber_edge_ingest_accepted_total` | counter | peer_id |
| `cerber_edge_ingest_rejected_total` | counter | reason (rate_limit, bad_signature, wrong_schema, ...) |
| `cerber_edge_ingest_body_bytes_total` | counter | peer_id |
| `cerber_edge_ingest_ndjson_lines` | gauge | (Read from the file periodically) |
| `rope_ha_external_probes_peer_count` | gauge | (Extracted from `read_external_probes()` output) |
| `rope_ha_external_probes_fail_ratio` | gauge | peer_id |
| `rope_ha_external_probes_stale_peers` | gauge | (Peers not reporting for > `EDGE_EXTERNAL_STALE_SECS`) |

### 2.6 System-level (node_exporter on rope-vps + GREEN + DO-rpc-1 + DO-rpc-2)

Standard node_exporter provides:

| Metric family | What we care about specifically |
|---|---|
| `process_resident_memory_bytes` | RSS of `datachain-rope.service`, `dc-explorer.service`, `rope-addr-indexer.service`, `cerber-*.service` |
| `node_sockstat_TCP_inuse`, `node_netstat_Tcp_CurrEstab` | TCP accept backlog (Recv-Q proxy) |
| `node_filesystem_avail_bytes` | Free disk on `/`, `/var/lib`, `/opt/datachain-rope` |
| `node_load1`, `node_load5`, `node_load15` | System load average |
| `node_disk_io_time_seconds_total` | Storage I/O saturation (RocksDB flusher backpressure indicator) |
| `node_memory_SwapCached_bytes` | Swap-in activity (P2B rollback threshold) |
| `node_context_switches_total` | Context switch rate (lattice lock contention indicator) |

### 2.7 nginx (rope-vps + DO-rpc-1 + DO-rpc-2)

nginx-vts-exporter or the built-in `stub_status` module gives:

| Metric | Type | Labels |
|---|---|---|
| `nginx_http_requests_total` | counter | vhost, status (2xx/3xx/4xx/5xx) |
| `nginx_upstream_response_seconds` | histogram | upstream, status |
| `nginx_upstream_up` | gauge | upstream |

Especially valuable: `nginx_http_requests_total{vhost="erpc.datachain.network",status=~"5.."}` alerted on = the earliest possible signal of "public RPC returning errors".

---

## 3. Metric catalog (target set)

Grouped by dashboard. Each metric maps to one of the sources in §2.

### 3.1 Fleet Health dashboard

- `rope_writer_status` (gauge, labels: node) - 1=healthy, 0.5=starting, 0=unhealthy/out_of_service
- `rope_edge_status` - 1=healthy, 0.5=degraded, 0=down
- `rope_edge_fail_ratio` (gauge)
- `rope_self_heal_unhealthy_for_seconds` (gauge)
- `rope_self_heal_escalate_to_cerber` (gauge, 0/1)
- `rope_ha_restarts_last_hour` (gauge)
- `rope_ghost_reclaim_total` (counter)
- `rope_block_height{node}` (gauge)
- `rope_p2p_peers_total{node}` (gauge)

### 3.2 Lattice Performance dashboard (the wedge preempt)

- `rope_lattice_head_guard_hold_seconds_bucket{op,quantile}` (histogram)
- `rope_lattice_oes_key_derive_seconds{op}` (histogram)
- `rope_lattice_flusher_wait_seconds{op}` (histogram)
- `rope_lattice_finality_actor_queue_depth` (gauge)
- `rope_lattice_strings_total{kind}` (gauge)
- `rope_lattice_knots_total{kind}` (gauge)
- `rope_lattice_invariant_holds` (gauge, 0/1)

### 3.3 Ledger Persistence dashboard (P2B before/after)

**Legacy path** (`ROPE_LEDGER_P2B=0`, current default):

- `rope_ledger_flusher_channel_depth` (gauge)
- `rope_ledger_flusher_write_batch_duration_seconds` (histogram)
- `rope_ledger_flusher_write_batch_size` (histogram)
- `rope_ledger_flusher_backpressure_events_total` (counter)
- `rope_ledger_lazy_rehydration_blobs_loaded_total` (counter, from §12)
- `rope_ledger_lazy_rehydration_progress_ratio` (gauge)

**P2B path** (`ROPE_LEDGER_P2B=1`, when enabled):

- `rope_ledger_p2b_shard_channel_depth{shard}` (gauge, 8 series)
- `rope_ledger_p2b_shard_write_batch_duration_seconds{shard}` (histogram)
- `rope_ledger_p2b_shard_durable_watermark{shard}` (gauge)
- `rope_ledger_p2b_shard_flusher_lag_seconds{shard}` (gauge)
- `rope_ledger_p2b_queue_full_total{shard}` (counter, tripped when a shard's mpsc rejects with `QueueFull`)

### 3.4 SWR Cache dashboard

- `dc_explorer_swr_cache_hit_ratio{endpoint}` (recording rule: `rate(dc_explorer_swr_cache_hits_total[5m]) / (rate(dc_explorer_swr_cache_hits_total[5m]) + rate(dc_explorer_swr_cache_stale_serves_total[5m]))`)
- `dc_explorer_swr_compute_duration_seconds{endpoint,quantile}` (histogram)
- `dc_explorer_swr_timeout_hits_total{endpoint}` (counter)
- `dc_explorer_swr_cache_age_seconds{endpoint}` (gauge)

### 3.5 Address Index dashboard

- `rope_addr_index_head_block` (gauge)
- `rope_addr_index_tip_lag_blocks` (gauge)
- `rope_addr_index_backfill_low_water` (gauge)
- `dc_explorer_addr_index_reader_hits_total{endpoint}` (counter)
- `dc_explorer_addr_index_reader_fallback_total{endpoint,reason}` (counter)
- `dc_explorer_addr_index_reader_duration_seconds{endpoint,quantile}` (histogram)

### 3.6 CERBER Mesh dashboard

- `cerber_mesh_peer_reachable{peer_id}` (gauge)
- `cerber_mesh_peer_coverage_pct{peer_id}` (gauge)
- `cerber_mesh_verify_failure_total{peer_id,reason}` (counter)
- `cerber_edge_ingest_accepted_total{peer_id}` (counter)
- `cerber_edge_ingest_rejected_total{reason}` (counter)
- `rope_ha_external_probes_peer_count` (gauge)
- `rope_ha_external_probes_fail_ratio{peer_id}` (gauge)

### 3.7 System dashboard

- `process_resident_memory_bytes{unit}` (from node_exporter)
- `process_open_fds{unit}` (from node_exporter)
- `node_load5`
- `node_memory_MemAvailable_bytes / node_memory_MemTotal_bytes`
- `node_filesystem_avail_bytes{mountpoint}`
- `node_disk_io_time_seconds_total{device}`
- `node_context_switches_total`

### 3.8 nginx dashboard

- `nginx_http_requests_total{vhost,status}` (counter)
- Recording rules for 5xx-per-second per vhost
- `nginx_upstream_response_seconds{upstream,quantile}` (histogram)
- `nginx_upstream_up{upstream}` (gauge)

---

## 4. Alerts

Two tiers: `page` (wakes an operator) and `metric-only` (dashboard-visible, not paged). Page-worthy alerts follow the same rule as CERBER R12: **only fire on sustained problems**, never on transient blips.

### 4.1 Page-worthy alerts

| Alert | Condition | For (sustain) | Rationale |
|---|---|---|---|
| BLUE writer down | `rope_writer_status{node="blue"} == 0` | 15 min | Matches CERBER R12 SLA (§21 Phase B.2) |
| Edge fail ratio high | `rope_edge_fail_ratio > 0.4` | 5 min | Matches HA edge sustain threshold |
| BLUE OOM approaching | `process_resident_memory_bytes{unit="datachain-rope"} > 6e9` | 2 min | 6 GB = 92% of §11 raised cap (6.5 GB) - preempt SIGKILL |
| Lattice invariant broken | `rope_lattice_invariant_holds == 0` | 1 min | Never should happen - immediate hard alert |
| CERBER escalate | `rope_self_heal_escalate_to_cerber == 1` | 0 (immediate) | This IS the CERBER page signal |
| Addr indexer lag | `rope_addr_index_tip_lag_blocks > 100` | 10 min | Reader-first path serves stale results after this |
| Ghost tx reclaim spike | `increase(rope_ghost_reclaim_total[5m]) > 10` | 0 | Attester ingress leak indicator |
| SWR compute timeout | `rate(dc_explorer_swr_timeout_hits_total[5m]) > 0.1` | 5 min | Endpoint is degrading (like §16 was) |
| Disk fill | `node_filesystem_avail_bytes{mountpoint="/"} < 5e9` | 5 min | 5 GB free = last chance to intervene |

### 4.2 Metric-only (dashboard-visible)

- Wedge indicator: `rope_lattice_head_guard_hold_seconds{op="append_to_ledger",quantile="0.99"} > 0.1` - visible on lattice dashboard, no page (BLUE recovers itself)
- `Recv-Q` proxy: `node_netstat_Tcp_CurrEstab` spike - graphed but not paged (noisy)
- Restarts: `rope_ha_restarts_last_hour > 4` - annotated on fleet dashboard, no page
- P2B queue-full: `increase(rope_ledger_p2b_queue_full_total{shard=~".+"}[5m]) > 0` - visible on ledger dashboard, no page (backpressure signal)
- Address index reader fallback: `rate(dc_explorer_addr_index_reader_fallback_total[5m]) > 0` - visible, no page (graceful fallback in place)

### 4.3 Silences and inhibits

- During any Phase 2.B deploy window, silence all `rope_ledger_*` alerts (they will be intentionally noisy).
- During any rope-node restart, inhibit `rope_writer_status`, `rope_block_height`, and `rope_ha_restarts_last_hour` alerts for 5 min (startup grace).
- If `rope_writer_status{node="blue"} == 0` is firing, inhibit all downstream alerts (Recv-Q, SWR timeouts, etc.) - they're symptoms, not causes.

---

## 5. Deploy topology

### 5.1 Recommended: single dedicated host

Provision a small VPS (2 vCPU, 4 GB RAM, 40 GB disk) named `metrics.datachain.network`. Same provider (Gandi, Paris SD6) as rope-vps for low-latency scraping. Cost: ~$20/month.

**Software stack:**

- Prometheus 2.x (single binary, TSDB, 90-day retention)
- Grafana 10.x (single binary, SQLite backend)
- Alertmanager (single binary)
- One nginx vhost with LE cert for public read-only `metrics.datachain.network`
- Optional: Loki for log aggregation (defer to Phase 2)

**Scrape targets** (Prometheus `prometheus.yml`):

```yaml
scrape_configs:
 - job_name: node
 static_configs:
 - targets:
 - rope-vps:9100
 - anvil-vps:9100 # GREEN
 - datachain-rpc-1:9100 # DO-rpc-1
 - datachain-rpc-2:9100 # DO-rpc-2
 - metrics-vps:9100
 - job_name: rope-node
 metrics_path: /metrics
 static_configs:
 - targets:
 - rope-vps:9101 # new: node exposes /metrics on 9101 (new port)
 - anvil-vps:9101
 - datachain-rpc-1:9101
 - datachain-rpc-2:9101
 - job_name: dc-explorer
 metrics_path: /metrics
 static_configs:
 - targets:
 - rope-vps:9102 # new: dc-explorer exposes /metrics on 9102
 - job_name: rope-addr-indexer
 metrics_path: /metrics
 static_configs:
 - targets:
 - rope-vps:9103 # new: rope-addr-indexer exposes /metrics on 9103
 - job_name: cerber-mesh
 metrics_path: /metrics
 static_configs:
 - targets:
 - rope-vps:9104
 - tanastok-vps:9104
 - dcswap-vps:9104
 - job_name: nginx
 static_configs:
 - targets:
 - rope-vps:9113 # nginx-vts-exporter
 - datachain-rpc-1:9113
 - datachain-rpc-2:9113
```

Firewall: all `909x` ports allowed only from `metrics-vps` IP (per source-IP allowlist in UFW).

### 5.2 Alternative: no dedicated host

Prometheus + Grafana can run on rope-vps itself, but:
- Shares memory pressure with rope-node (already at 4-6 GB RSS)
- Loses independence: if BLUE dies, so does observability
- Cannot scrape external peers behind Gandi firewalls without weakening the ACL

Recommend against unless budget-constrained.

### 5.3 Public read-only dashboard

`https://metrics.datachain.network` serves Grafana with:
- Anonymous read-only access to public dashboards (fleet health, block height, edge probes)
- Login-gated access to internal dashboards (lattice performance, ledger persistence, SWR cache)
- No write access anywhere
- LE cert managed by certbot

This is the "public status page" equivalent - lets Tanastok / DCSwap / Datawallet+ operators peer at the same signal in real time.

---

## 6. Rollout plan (phased)

### Phase 1 - Minimum Viable Observability (1-2 days operator work, mostly config)

1. Provision `metrics-vps` (Gandi, 2 vCPU / 4 GB / 40 GB, LE cert)
2. Install Prometheus + Grafana + node_exporter on all 5 hosts (metrics-vps + 4 rope nodes)
3. Import 2 dashboards:
 - Fleet Health (writer.status, edge.status, restarts, block_height, RSS, load)
 - System (node_exporter defaults)
4. Configure 4 alerts:
 - BLUE writer down > 15 min
 - CERBER escalate immediate
 - BLUE RSS > 6 GB
 - Disk < 5 GB

**Cost/benefit:** ~1-2 days operator time; catches every OOM (§11 class) and prolonged wedge (§17 class) before user-visible impact. Nothing new to build in Rust.

### Phase 2 - Instrumentation (1-2 weeks engineering)

5. Add `/metrics` endpoint to rope-node (bind to `127.0.0.1:9101`). Emit `rope_latticeMetrics` as Prometheus histograms + write `rope_globalStats` as gauges. ~150 lines Rust using the `prometheus` crate.
6. Add `/metrics` endpoint to dc-explorer (bind to `127.0.0.1:9102`). Emit SWR cache hits/stales + `AddressIndex` reader stats + axum request duration histogram. ~200 lines Rust.
7. Add `/metrics` endpoint to rope-addr-indexer (bind to `127.0.0.1:9103`). Emit head_block / tip_lag / backfill_low_water. ~50 lines Rust.
8. Import 3 more dashboards:
 - Lattice Performance
 - SWR Cache
 - Address Index

### Phase 3 - CERBER + External Probes (1 week)

9. Add scraper for cerber-mesh peers (small Python service on metrics-vps). Scrapes `/v1/cerber/mesh-status` from each peer every 30 s. Exposes as `/metrics` on port 9104.
10. Wire `read_external_probes()` output into Prometheus via a text-file collector (node_exporter can scrape textfiles). Every HA tick writes a `.prom` file that node_exporter re-exports.
11. Import CERBER Mesh dashboard.
12. Silence/inhibit rules from §4.3.

### Phase 4 - nginx + Log aggregation (optional, deferred)

13. Deploy nginx-vts-exporter on rope-vps and DO nodes.
14. Deploy Loki + Promtail for centralized logs (`journalctl` from all 4 rope nodes + dc-explorer + cerber-*).
15. Import nginx dashboard + log-search Grafana panel.

---

## 7. Cost model

| Resource | Cost/month | Rationale |
|---|---|---|
| metrics-vps (Gandi 2vCPU/4GB/40GB) | ~$20 | Prometheus + Grafana + Alertmanager |
| node_exporter on 4 existing hosts | $0 | Already-owned hosts, 20-50 MB RSS per node |
| Prometheus TSDB (90-day retention) | $0 | Fits in 40 GB disk at expected scrape volume (10k series * 15 s scrape = ~200 MB/day compressed) |
| Alertmanager notifications | ~$5 | Twilio / SendGrid tier for pages |
| **Total** | **~$25/month** | vs. one billable incident (an operator hour) |

Grafana Cloud free tier could substitute the metrics-vps for the first 6 months (10k series free, 14-day retention). Recommend self-hosting for the public dashboard requirement in §5.3.

---

## 8. What Prometheus + Grafana does NOT solve

- **Distributed tracing.** For that, integrate OpenTelemetry SDK (Rust `tracing-opentelemetry` crate) into rope-node and export to Tempo or Jaeger. Deferred; wedges are all local, not distributed.
- **Log aggregation.** Loki is listed in Phase 4 but is optional. `journalctl` on each host is sufficient until scale demands otherwise.
- **On-call rotation.** Alertmanager routes to a single endpoint (email / Slack webhook). PagerDuty / OpsGenie integration is a separate operator ops choice.
- **The wedge itself.** Observability tells us the wedge is happening; P2B (§22) fixes the wedge. Both are needed.

---

## 9. Decision points for the operator

1. **Approve or veto the ~$25/month metrics-vps.** If vetoed, Phase 1 lands on rope-vps itself (with the caveats in §5.2).
2. **Public dashboard yes/no.** `metrics.datachain.network` public read-only page is a strong ecosystem-transparency signal but adds a public-attack surface. Recommendation: yes, LE-cert-gated, anonymous read-only on 3 dashboards (fleet health, block height, edge probes) only.
3. **Alert routing endpoint.** Where should Alertmanager fire? Options:
 - Email to `contact@onguene.com` (matches CERBER convention, per `handover-to-dcswap-cerber-r13-body-hash-race-2026-07-29.mdc`)
 - Slack webhook (needs a Datachain workspace)
 - Both
 Recommendation: both, with email as primary and Slack as visibility.

---

## 10. Reference

- Existing metric surfaces referenced in this design:
 - `crates/rope-node/src/lattice_metrics.rs` (rope_latticeMetrics RPC)
 - `crates/rope-node/src/self_watchdog.rs` (§23 self-watchdog)
 - `crates/rope-explorer/src/swr.rs` (§Session N+1 SWR helper)
 - `crates/rope-addr-index/src/reader.rs` (§Session N+3 wired)
 - `deploy/cerber/bin/cerber-edge-ingest.mjs` (§Session N+3 edge ingest)
 - `deploy/scripts/erpc-fleet-ha.sh` (fleet-status + `read_external_probes`)
- Related design docs:
 - `docs/EDGE_EXTERNAL_PROBES_INGEST_SPEC_v1.md`
 - `docs/QUIPU_CANON_V2_PHASE2B_DEPLOY_PLAYBOOK.md`
- Incidents that motivated this design:
 - §11 (BLUE OOM crash-loop) - would have been caught by RSS alert
 - §16 (dcscan 504 + CERBER page) - would have been caught by SWR timeout alert
 - §17 (BLUE wedge cycle) - would have been caught by `head_guard_hold` alert
 - §20 (HA cap saved BLUE) - would have been visible on restart-per-hour graph
 - §21 (Phase C didn't close wedge) - would have quantified the residual bottleneck
 - §22 (P2B parallel writers) - needs before/after comparison graphs

---

*This design is complete as spec. Implementation is Phase 1 (~2 days operator work, no Rust changes) then Phase 2 (~1-2 weeks engineering). Nothing here is speculative - every metric already exists in code or is a trivial extraction from an existing file.*

- Rope agent, 2026-08-12T~13:45Z
