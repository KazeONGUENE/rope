# rope-shadow-witness — auto-promotion

Operational scripts that run on **rope-vps (BLUE)** to evaluate the canary
soak and, if it passes, deploy the rope-shadow-witness to BLUE and GREEN
at canary T+7d.

| File | Where it lives | Purpose |
|---|---|---|
| `canary-health-gate.sh` | `/usr/local/bin/canary-health-gate.sh` (BLUE) | SSHes to canary, evaluates 9 soak criteria, exits 0/1 |
| `deploy-shadow-witness.sh` | `/usr/local/bin/deploy-shadow-witness.sh` (BLUE) | Idempotent build+install+smoke on one target |
| `promote-shadow-witness.sh` | `/usr/local/bin/promote-shadow-witness.sh` (BLUE) | Orchestrates: gate -> BLUE -> GREEN |
| `rope-shadow-witness-promote.service` | `/etc/systemd/system/` (BLUE) | One-shot service that runs the orchestrator |
| `rope-shadow-witness-promote.timer` | `/etc/systemd/system/` (BLUE) | Calendar timer firing once at 2026-05-16T09:16:07 UTC |

## Soak criteria (gate)

The gate passes only if **every** row passes. Source of truth: `canary-health-gate.sh` lines 30-50.

1. `service.active == active` — service is running on the canary.
2. `chain.first_observed_at_age_s >= 7 days` — **data-derived** soak.
   Survives binary refresh on the canary, unlike systemd uptime.
3. `rounds.last_hour.failure_pct <= 5` — round failure rate.
4. `logs.last_24h.error_count <= 50` — journald error volume.
5. `chain.observed_strings >= 1` — witness is doing useful work.
6. `chain.observed_knots >= 1` — knots have been tied.
7. `rpc.local_status_ok == true` — `rope_v2_status` responds.
8. `chain.last_observed_at_age_s <= 60 s` — witness is keeping up
   with the upstream (no stalled poll loop).
9. `process.rss_kb < 512 MB` — memory bounded.

## Operator override

If something looks wrong on T+6d and you need to abort the auto-promotion,
just touch the kill-switch on BLUE:

```bash
ssh rope-vps 'echo "$(date -u): aborted by <name> because <reason>" | sudo tee /etc/rope-shadow-witness/promotion-disabled'
```

The systemd unit has `ConditionPathExists=!/etc/rope-shadow-witness/promotion-disabled`,
so even the service start path is gated. The kill-switch can be removed
afterward to re-enable a manual `sudo systemctl start rope-shadow-witness-promote.service`.

## Manual run

```bash
# Just the gate (prints pass/fail, exits 0 either way):
sudo /usr/local/bin/promote-shadow-witness.sh --gate-only

# Force a deploy without running the gate (NOT recommended):
sudo /usr/local/bin/promote-shadow-witness.sh --skip-gate

# Full run (what the timer does):
sudo /usr/local/bin/promote-shadow-witness.sh
```

## Failure modes and recovery

| Outcome | Exit | Effect | Recovery |
|---|---|---|---|
| Kill-switch present | 0 | No-op | Remove file; rerun manually |
| Gate FAIL | 0 | No-op | Inspect canary, fix, re-arm timer |
| BLUE deploy FAIL | 1 | BLUE service stopped; GREEN untouched | `journalctl -u rope-shadow-witness` on BLUE; rerun after fix |
| GREEN deploy FAIL | 2 | BLUE deployed; GREEN service stopped | Two-of-three mesh (canary+BLUE) is healthy; redeploy GREEN later |
| All OK | 0 | Three-witness mesh active | Run determinism cross-check (runbook §7) |

## Why not auto-promote BLUE first then GREEN serially in CI?

Two reasons:

1. The toolchain on each target is at a different glibc level (BLUE 2.39,
   GREEN 2.35, canary 2.35). A single CI build cannot produce one binary
   that runs on all three. Each target builds natively.
2. Auto-promotion at the timer instant is a deliberate **wall-clock
   gate** rather than a green-CI gate. Soak duration is a property of
   real-world observation, not test runtime.
