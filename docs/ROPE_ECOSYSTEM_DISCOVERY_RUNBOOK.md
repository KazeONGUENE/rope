# rope-ecosystem-discovery - operator runbook

**Status:** code-complete, `cargo test -p rope-ecosystem-discovery` green, `cargo check -p rope-ecosystem-discovery` clean. Not yet deployed to rope-vps. Safe to ship dark: the daemon only writes to a JSONL file; `rope-explorer` does not read that file until an operator sets `ECOSYSTEM_OVERLAY_PATH` on the `dc-explorer` unit.

**Purpose:** autonomously discover new or updated ecosystem projects (on-chain labels, later: handover markdown, later: partner APIs) and merge them into the `https://dcscan.io/ecosystem` directory without a human editing `crates/rope-explorer/src/ecosystem_canonical.rs`. Precedence stays `EDC-registered > canonical (hand-curated) > overlay (auto-discovered)`, so a canonical entry always wins over a duplicate discovered by the scanner - including for `visibility`, which means the four hidden projects (Moneymaker, Picentriq, ReinvoiceOTC, BrainCities 2026) stay hidden even if the scanner "rediscovers" one of them under a different id.

Reference specs:

- `docs/ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md` - the wire contract this daemon writes and `rope-explorer` reads.
- `crates/rope-explorer/src/ecosystem_overlay.rs` - the loader on the reader side (already deployed with dc-explorer §30 of `handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc`).
- `crates/rope-ecosystem-discovery/` - this crate.

---

## 1. Deploy (fresh install on rope-vps)

Every step is idempotent and reversible. The daemon runs entirely alongside the current stack (rope-node, reth-rope, dc-explorer, rope-addr-indexer) and does not touch any of them until §3 flips the read-flag on dc-explorer.

```bash
# 1.1 - build the release binary on jammy (workspace policy for OS-parity
#       across the fleet - see deploy/scripts/deploy-fleet.sh from the
#       2026-06-12 V11 hot-fix).
ssh rope-vps 'cd /home/ubuntu/datachain-rope && \
  cargo build --release -p rope-ecosystem-discovery --bin rope-ecosystem-discovery'
ls -la /home/ubuntu/datachain-rope/target/release/rope-ecosystem-discovery   # binary present

# 1.2 - install the systemd unit + example config.
sudo cp /home/ubuntu/datachain-rope/deploy/rope-ecosystem-discovery.service \
  /etc/systemd/system/rope-ecosystem-discovery.service
sudo cp /home/ubuntu/datachain-rope/deploy/rope-ecosystem-discovery.example.toml \
  /etc/rope-ecosystem-discovery.toml
sudo chmod 0644 /etc/rope-ecosystem-discovery.toml
sudo chown root:root /etc/rope-ecosystem-discovery.toml

# 1.3 - create the data directory that ReadWritePaths= binds over.
sudo mkdir -p /var/lib/rope-ecosystem-discovery
sudo chown ubuntu:ubuntu /var/lib/rope-ecosystem-discovery
sudo chmod 0750 /var/lib/rope-ecosystem-discovery

# 1.4 - sanity: print the resolved config without touching the network
#       or the filesystem. Zero side effects.
sudo -u ubuntu /home/ubuntu/datachain-rope/target/release/rope-ecosystem-discovery \
  --config /etc/rope-ecosystem-discovery.toml --dry-run

# 1.5 - one-shot smoke test (writes overlay once, then exits).
sudo -u ubuntu /home/ubuntu/datachain-rope/target/release/rope-ecosystem-discovery \
  --config /etc/rope-ecosystem-discovery.toml --once
sudo ls -la /var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl   # expect present + non-empty

# 1.6 - enable + start the daemon (writes only, reads still unchanged).
sudo systemctl daemon-reload
sudo systemctl enable --now rope-ecosystem-discovery.service
systemctl is-active rope-ecosystem-discovery.service   # expect: active
```

Watch the first minute of the journal for the following canonical log lines:

```bash
journalctl -u rope-ecosystem-discovery -f
# expect within 2-5 s:
#   INFO rope_ecosystem_discovery: starting rope-ecosystem-discovery daemon
#   INFO rope_ecosystem_discovery: resolved config output_path=... run_interval_secs=900 ...
# then, on every discovery pass:
#   INFO rope_ecosystem_discovery: discovery pass: scanners_run=1 scanners_ok=1 entries_total=N
#   INFO rope_ecosystem_discovery: overlay written: path=... input=N written=N deduped=0 bytes=NNNN
```

If `scanners_ok` matches `scanners_run` and `written > 0`, everything is healthy. The next pass fires after `run_interval_secs` (default 900 s = 15 min).

---

## 2. Verify

```bash
# 2.1 - overlay file present + non-empty.
sudo ls -la /var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl
sudo wc -l /var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl   # one JSON object per line

# 2.2 - schema spot-check: every line must be valid JSON with an id.
sudo cat /var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl \
  | head -n 5 \
  | python3 -c "import json, sys; [json.loads(l) and print(json.loads(l)['id'], '=>', json.loads(l).get('name')) for l in sys.stdin]"

# 2.3 - dry-run against the current config to reprint resolved settings.
sudo -u ubuntu /home/ubuntu/datachain-rope/target/release/rope-ecosystem-discovery \
  --config /etc/rope-ecosystem-discovery.toml --dry-run
```

---

## 3. Turn reads on (operator gate)

The switch is a single environment variable on the `dc-explorer` unit. When set, dc-explorer reads the overlay file on every ecosystem-directory cache refresh and merges its entries at the lowest precedence (below canonical, below EDC-registered). When unset, dc-explorer returns exactly the same response it did before this crate existed.

```bash
# 3.1 - add the flag to whichever env file dc-explorer already reads
#       (production is /opt/datachain-rope/code/deploy/.env on rope-vps).
sudo tee -a /opt/datachain-rope/code/deploy/.env <<'EOF'

# rope-ecosystem-discovery overlay (2026-08-13 handover).
ECOSYSTEM_OVERLAY_PATH=/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl
EOF

# 3.2 - restart dc-explorer to pick it up.
sudo systemctl restart dc-explorer.service
systemctl is-active dc-explorer.service

# 3.3 - external verification (from any workstation): overlay entries
#       appear in the directory with source="overlay:<discovered_by>".
curl -sS https://dcscan.io/api/v1/ecosystem/directory \
  | python3 -c "import json,sys; d=json.load(sys.stdin); print('total:', len(d.get('projects', []))); print('overlay-sourced:', sum(1 for p in d.get('projects', []) if str(p.get('source','')).startswith('overlay')))"
```

**Rollback (instant, no data loss):**

```bash
sudo sed -i '/ECOSYSTEM_OVERLAY_PATH=/d' /opt/datachain-rope/code/deploy/.env
sudo systemctl restart dc-explorer.service
```

dc-explorer immediately reverts to canonical + EDC-only. The discovery daemon keeps running; the overlay file on disk is untouched.

---

## 4. Common ops

### Restart the discovery daemon

Idempotent; safe at any moment. Every pass rewrites the overlay file atomically (tmp + fsync + rename), so a restart mid-pass leaves the previous good overlay in place.

```bash
sudo systemctl restart rope-ecosystem-discovery.service
```

### Change scan cadence

Edit `/etc/rope-ecosystem-discovery.toml`:

```toml
run_interval_secs = 300   # 5 min instead of 15
```

`sudo systemctl restart rope-ecosystem-discovery.service`. Config is only read at startup; there is no hot-reload path (deliberate - cadence changes are rare and the restart is cheap). Minimum enforced by `DiscoveryConfig::validate()` is 60 s.

### Enable / disable individual scanners

Every scanner has an `enabled = true|false` flag. To turn the on-chain scanner off while investigating a rogue label on dcscan:

```toml
[onchain]
enabled = false
```

`sudo systemctl restart rope-ecosystem-discovery.service`. Next pass will run zero scanners, log `discovery pass: scanners_run=0 scanners_ok=0 entries_total=0`, and write an empty overlay file - which dc-explorer will happily read as "no overlay entries", leaving the directory equal to canonical + EDC. This is the safest way to quiesce the scanner without touching the systemd unit.

### Manually append an overlay entry (bypass the scanners)

The overlay JSONL is a plain append target. Any operator with write access to `/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` can add a hand-written line with `discovered_by: "manual"`. The loader accepts it, but the discovery binary never emits `manual` itself, so a subsequent scan will preserve manual entries (they are on-disk and the writer is dedup-aware by id).

```bash
sudo tee -a /var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl <<'EOF'
{"id":"newproject","name":"New Project","archetype":"infrastructure","status":"development","discovered_by":"manual","discovery_source":"operator@rope-vps","discovered_at":1786579200}
EOF
```

Warnings: (a) the id must satisfy the loader's regex (lowercase, digits, hyphens, 3-64 chars); (b) canonical ids and hidden ids override any overlay entry with the same id.

### Nuke and re-scan (rare)

The overlay file is a projection, not a source of truth. Removing it is safe:

```bash
sudo systemctl stop rope-ecosystem-discovery.service
sudo rm /var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl
sudo systemctl start rope-ecosystem-discovery.service
# Next pass (within run_interval_secs) rewrites it from scratch.
```

`dc-explorer` gracefully handles a missing overlay file (loader returns an empty entry list, no error).

---

## 5. Diagnostics

| Symptom | Where to look | Likely cause |
|---|---|---|
| Service crash-loops on start | `journalctl -u rope-ecosystem-discovery -n 100 --no-pager` | Bad config (typo in `output_path`, `run_interval_secs < 60`, on-chain `dcscan_base` not `http://` or `https://`), or filesystem-sandbox mismatch (`ReadWritePaths=` must contain the parent of `output_path`) |
| `scanners_ok < scanners_run` in the journal | Same journal | One or more scanners returned an error. Look for the `scanner=<name> failed: <error>` warn line right above the `discovery pass:` info line. Common cause: dcscan.io `/api/v1/labels` timed out; the pass continues with the other scanners |
| Overlay file empty after a pass | `sudo cat /var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` | Either every scanner returned zero entries (check the individual scanner blocks in the config), or every entry was rejected as invalid (check the journal for `overlay rejected id=... reason=...` warn lines) |
| Overlay grows unbounded | `sudo wc -l /var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` | The writer is dedup-aware by id, so this should not happen. If it does, look for a scanner emitting a moving target id (e.g., a timestamp baked into the id); file a bug |
| dc-explorer serves stale overlay after config change | Journal for dc-explorer + a spot `curl` against `/api/v1/ecosystem/directory` | dc-explorer caches the merged directory for a few minutes. Restart `dc-explorer.service` to force an immediate refresh |
| Hidden project (Moneymaker, Picentriq, ReinvoiceOTC, BrainCities 2026) reappears in the directory | `curl https://dcscan.io/api/v1/ecosystem/directory | jq '.projects[] | select(.id == "<id>")'` | The canonical `visibility_for()` override is broken. This is a `rope-explorer` bug, NOT a discovery-side bug - the loader always applies canonical visibility precedence. File it against `crates/rope-explorer/src/ecosystem_canonical.rs` |

---

## 6. Cost profile

- **CPU**: negligible. One `reqwest` GET against `dcscan.io/api/v1/labels` per pass (~2-10 kB response), plus a few thousand `slugify` + `is_known_archetype` calls in-memory. Typical pass finishes in < 1 s.
- **RAM**: cap is 512 MB in the systemd unit. Steady-state is ~10-30 MB (one `reqwest` client + one `Vec<OverlayEntry>` per pass; both dropped between passes).
- **Disk**: overlay file is bounded by the number of ecosystem entities. Today ~50-100 entries, well under 100 kB total.
- **Net**: ~10 kB per pass in steady state (dcscan.io labels endpoint). Handover + partner-API scanners, when enabled, add local file reads and per-partner HTTPS requests respectively.

---

## 7. Why this is safe to ship dark

The loader in `rope-explorer` treats the overlay as the lowest-precedence source. Every canonical entry (including every hidden entry) wins. An EDC-registered entry always wins. If the loader cannot parse a line, it drops that line and continues (fail-open on the overlay, fail-secure on canonical). If the overlay file does not exist, the loader returns an empty list. If the file is corrupted, the loader returns an empty list and logs a warning. None of these failure modes can revive one of the four hidden projects, and none of them can override an operator's canonical curation.

Rollback is a two-line env edit on `dc-explorer`; there is no on-chain state, no user-facing state, and no coordination requirement with any other project. The discovery daemon can stay running forever with `ECOSYSTEM_OVERLAY_PATH` unset - the overlay is written to disk and ignored.

---

## 8. Cross-reference

- Loader source: `crates/rope-explorer/src/ecosystem_overlay.rs`
- Wire contract: `docs/ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md`
- Discovery crate: `crates/rope-ecosystem-discovery/`
- Systemd unit: `deploy/rope-ecosystem-discovery.service`
- Example config: `deploy/rope-ecosystem-discovery.example.toml`
- Canonical registry (source of truth for `visibility_for()`): `crates/rope-explorer/src/ecosystem_canonical.rs`
- Prior handover that shipped the loader + hid the four projects: `.cursor/rules/handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc` §30
