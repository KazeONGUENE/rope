# Ecosystem Overlay JSONL Contract - v1

**Status:** DRAFT v1 (2026-08-13)
**Owner:** Datachain Rope agent
**Consumers:** `rope-explorer` (`ecosystem_overlay.rs` loader), `rope-ecosystem-discovery` (writer)
**Location:** `/var/lib/rope-explorer/ecosystem-overlay.jsonl` (path overridable via `ECOSYSTEM_OVERLAY_PATH`)

---

## 0. Purpose

The `/ecosystem` page on dcscan.io is fed by three data sources today:

1. **EDC-registered projects** - live self-registration by stakeholder wallets via `console.datachain.network` (spec v2.0 §8). Zero today.
2. **Canonical hand-curated entries** - `crates/rope-explorer/src/ecosystem_canonical.rs::canonical_entries()`. Reviewed by the operator; ~28 entries.
3. **Overlay entries (NEW - this spec)** - discovered autonomously by `rope-ecosystem-discovery` from handover markdown, on-chain contract deployments, and partner API scans. Fills the gap between "operator hasn't gotten around to editing `ecosystem_canonical.rs` yet" and "self-registration in EDC".

Overlay entries let the ecosystem page reflect new projects the moment they appear in a handover, a partner API response, or an on-chain deployment - without requiring an operator commit + `dc-explorer` rebuild.

## 1. File format

**Wire format:** JSON Lines (JSONL) - one JSON object per line, LF-delimited, UTF-8. No enclosing array. Comments not permitted. Every line MUST parse independently.

**Rationale:**
- Atomic append via `write(2)` when line length is under `PIPE_BUF` (4096 bytes). No fsync-then-rename dance for incremental additions.
- Streaming reads via `BufRead::lines()` - no need to hold the whole file in memory.
- Corrupt lines (parse-failure) are skipped with a `tracing::warn!` and do not poison the whole file.
- Trivial to inspect with `jq` or `head`/`tail`.

**Location:**
- Default: `/var/lib/rope-explorer/ecosystem-overlay.jsonl`
- Env override: `ECOSYSTEM_OVERLAY_PATH` (absolute path)
- Writer creates parent dir with `mkdir -p` and sets mode `0755` on the dir, `0644` on the file.

**Size limit:**
- Loader hard-caps at `1_000` entries per file (guards against runaway writer bugs).
- Individual line hard-cap: `8 KB` (larger lines are dropped with a warn).
- File hard-cap: `8 MB` (larger files are truncated at load time with an error).

## 2. Entry schema

Each line is a JSON object with the following fields. `Type` uses TypeScript-ish notation.

### 2.1 Required fields

| Field | Type | Notes |
|---|---|---|
| `id` | `string` | Lowercase slug, `[a-z0-9-]+`, 3-64 chars. Same shape as `CanonicalEntry.id`. MUST be unique in the file (last-wins if a duplicate id appears - later append overrides earlier). |
| `name` | `string` | Human-readable name, 3-128 chars. |
| `archetype` | `string` | One of the archetypes declared in `ecosystem_canonical::canonical_archetypes()`. Currently: `predictive_maintenance`, `environmental_monitoring`, `hybrid`, `dex`, `asset_tokenization`, `identity_wallet`, `sso`, `block_explorer`, `ai_agent`, `governance`, `foundation`, `bridge`, `biodiversity`, `health`, `investment`, `infrastructure`. Unknown archetypes are dropped with a warn (the frontend badge map is closed). |
| `status` | `string` | One of `live`, `development`, `sandbox`, `archived`. Other values dropped. |
| `discovered_by` | `string` | Which discovery scanner produced this entry. One of `handover-scanner`, `onchain-scanner`, `partner-api-scanner`, `manual` (for hand-authored overlays). |
| `discovery_source` | `string` | Human-readable pointer to where the discovery came from, e.g. `handover-file:.cursor/rules/handover-from-tanastok-treasury-...mdc`, `contract:0x...`, `api:https://tanastok.io/api/v1/tokenized-assets`. Used for audit + demotion. |
| `discovered_at` | `number` | Unix timestamp (seconds) when the scanner first observed this entry. |

### 2.2 Optional fields (all default to `null` / empty)

| Field | Type | Notes |
|---|---|---|
| `tags` | `string[]` | Up to 12 tags. Each 2-32 chars. Lowercase. |
| `region` | `string` | Free text, 3-64 chars. Defaults to `"Global"` if absent. |
| `country` | `string` | ISO 3166-1 alpha-2 (2 chars) OR `"GLOBAL"`. Defaults to `"GLOBAL"`. |
| `wallet` | `string` | EVM address `^0x[a-fA-F0-9]{40}$` OR empty. Stored lowercase. |
| `stakeholder_url` | `string` | Must be `https://...`. `http://` and other schemes rejected. |
| `description` | `string` | Up to 500 chars. Truncated with `...` if longer. |
| `asset_count` | `number` | Non-negative integer, ≤ 10,000,000. |
| `sensor_count` | `number` | Non-negative integer, ≤ 10,000,000. |
| `logo_url` | `string` | Absolute `https://...` URL. `http://` rejected. |
| `created_at` | `number` | Unix timestamp (seconds) when the project itself was created. Falls back to `discovered_at` if absent. |
| `visibility` | `string` | One of `public`, `private_visible`, `private_hidden`. Defaults to `public`. **Overlay entries CANNOT override canonical visibility** - if the `id` also exists in `PRIVATE_HIDDEN_IDS` / `PRIVATE_VISIBLE_IDS` in `ecosystem_canonical.rs`, the canonical visibility wins (see §4 precedence). |

### 2.3 Fields NOT permitted

- `source` - loader always emits `"source": "overlay:<discovered_by>"` regardless of what the writer sets.
- `edc_base` - always `null` for overlay entries (per convention: `edc_base` is only non-null for EDC-registered live cards).

## 3. Example entries

```jsonl
{"id":"newapp-2026","name":"NewApp","archetype":"identity","status":"development","tags":["identity","did"],"region":"Global","country":"GLOBAL","wallet":"","stakeholder_url":"https://newapp.example","description":"An identity app discovered from handover-from-newapp-...mdc","asset_count":0,"sensor_count":0,"logo_url":null,"created_at":1786500000,"discovered_at":1786600000,"discovered_by":"handover-scanner","discovery_source":"handover-file:.cursor/rules/handover-from-newapp-onboarded-2026-08-13.mdc"}
{"id":"newpool-fat-usdc-2","name":"DCSwap Pool FAT/USDC (v2)","archetype":"dex","status":"live","tags":["dex","pool"],"wallet":"0x1234567890abcdef1234567890abcdef12345678","stakeholder_url":"https://dcswap.net","description":"Second FAT/USDC pool deployed 2026-08-13","asset_count":0,"sensor_count":0,"created_at":1786501234,"discovered_at":1786501300,"discovered_by":"onchain-scanner","discovery_source":"contract:0x1234567890abcdef1234567890abcdef12345678"}
```

## 4. Precedence rules

When merging into `EcosystemDirectoryCache.projects`:

1. **EDC-registered entries win first.** Live self-registration is the strongest signal.
2. **Canonical entries win second.** Hand-curated, operator-reviewed.
3. **Overlay entries fill remaining gaps.** Only added if `id` doesn't collide with EDC or canonical.

**Visibility precedence:**
- If an `id` in the overlay also matches `PRIVATE_HIDDEN_IDS` or `PRIVATE_VISIBLE_IDS` in `ecosystem_canonical.rs`, the canonical visibility applies REGARDLESS of what the overlay file says. This prevents an attacker who writes to the overlay file from un-hiding a project the operator wants hidden.
- If an `id` is only in the overlay (not canonical), the `visibility` field in the overlay entry is used, defaulting to `public`.

**Refresh cadence:**
- `refresh_ecosystem_directory_cache` runs every `ECOSYSTEM_DIRECTORY_REFRESH_SECS` (default 300s).
- Overlay file re-read on every refresh - a new line appended is visible on the next tick within ≤5 min without service restart.

## 5. Discovery scanner contract

`rope-ecosystem-discovery` is the reference writer. Contract:

1. **Idempotent by id.** Re-writing the same entry with the same content is a no-op (dedup at load time - last-wins).
2. **Only append** during normal operation. Full rewrite requires a `.tmp` + `fsync` + `rename` dance (see §6).
3. **Verify before write.** Every entry must have `discovered_at`, `discovered_by`, `discovery_source` populated. Missing = drop entry, log warn on writer side.
4. **Never write into PRIVATE_HIDDEN_IDS.** Writers MUST check the canonical hidden list before appending. Overlay entries for hidden projects are silently discarded at load time; writing them wastes disk.
5. **Age out.** Writers SHOULD purge entries older than 90 days that haven't been re-observed. Aging is done via full-rewrite (§6). Loader does not enforce age; the overlay file is the writer's responsibility.

## 6. Safe rewrite

To atomically replace the file (e.g. for age-out, dedup, or bulk update):

```
1. Write new content to /var/lib/rope-explorer/ecosystem-overlay.jsonl.tmp-<pid>
2. fsync(fd)
3. close(fd)
4. rename(.tmp-<pid>, ecosystem-overlay.jsonl)
5. fsync(dirfd)
```

Loader takes a shared read lock (`flock(LOCK_SH)`); writer takes an exclusive lock during rename (`flock(LOCK_EX)`). All lock-holding is bounded (<100 ms typical).

## 7. Error handling

| Error | Loader behavior |
|---|---|
| File does not exist | Return empty overlay list; log `tracing::debug!` once at startup. |
| Permission denied | Return empty overlay list; log `tracing::warn!` on every refresh (visibility). |
| File > 8 MB | Return empty overlay list; log `tracing::error!`; recommend operator rewrite. |
| Line > 8 KB | Drop the line; log `tracing::warn!` with `entry_id?` if parseable. Continue reading. |
| Malformed JSON | Drop the line; log `tracing::warn!` with line number. Continue reading. |
| Missing required field | Drop entry; log `tracing::warn!` with `id` if present. Continue reading. |
| Unknown archetype | Drop entry; log `tracing::warn!` with `id + archetype`. Continue reading. |
| Duplicate id in same file | Last-wins; log `tracing::debug!`. |
| `id` collides with canonical/EDC | Silently drop; do not log (this is the common case for a slow-to-remove overlay after promotion to canonical). |

## 8. Test coverage

Loader ships with these unit tests (all in `ecosystem_overlay::tests`):

- `load_missing_file_returns_empty_ok`
- `load_empty_file_returns_empty_ok`
- `load_single_valid_entry_returns_one_card`
- `load_appends_source_overlay_prefix`
- `load_ignores_malformed_json_lines`
- `load_drops_entries_missing_required_fields`
- `load_drops_entries_with_unknown_archetype`
- `load_drops_entries_with_unknown_status`
- `load_drops_entries_over_max_line_length`
- `load_caps_at_max_entries_per_file`
- `load_last_write_wins_on_duplicate_id`
- `load_hidden_id_visibility_is_enforced_from_canonical`
- `load_rejects_http_scheme_in_urls`
- `load_normalizes_wallet_address_lowercase`
- `load_truncates_long_descriptions`
- `load_ignores_entries_over_max_file_size`

## 9. Frontend integration

No frontend changes required. The overlay loader emits card-shaped JSON that goes into `EcosystemDirectoryCache.projects` alongside EDC and canonical cards. The existing `renderCard()` / `applyFilters()` / `visibilityBadge()` logic handles overlay entries identically to canonical entries.

Overlay entries can be distinguished by:
- `source: "overlay:<discovered_by>"` (e.g. `"overlay:handover-scanner"`)
- `edc_base: null` (same as canonical)
- Presence of `discovered_at`, `discovered_by`, `discovery_source` fields (canonical entries never have these)

The `builtin:datachain-canonical-registry` synthetic source row in `cache.sources` will be joined by a new `builtin:ecosystem-overlay` row when overlay entries are loaded, showing `projects: <count>` and `note: "auto-discovered by rope-ecosystem-discovery; canonical + EDC entries take precedence on id collision"`.

## 10. Migration path

- **Today:** overlay file does not exist. Loader returns empty; no behavior change on `/api/v1/ecosystem/directory`.
- **After discovery script deploys:** writer creates + populates the file. Loader picks up entries within ≤5 min per refresh tick.
- **When an overlay entry stabilizes:** operator promotes it into `ecosystem_canonical.rs::canonical_entries()` and commits + rebuilds. Overlay entry then collides on `id` and is silently dropped from the merged directory. Writer's age-out (§5.5) eventually removes it from the file too.
- **Rollback:** delete the overlay file OR unset `ECOSYSTEM_OVERLAY_PATH` (defaulting to a non-existent path). No `dc-explorer` restart needed - loader re-checks the file on every refresh.

---

*This spec is v1. Breaking changes require a `v2` doc and a version marker in the file header comment (JSONL doesn't support comments, so any breaking change requires a new file path).*
