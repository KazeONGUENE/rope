# Canonical-Agents Handover Rollout — DEPLOY kickoff (2026-05-10, **schema v2**)

Sister to [`../kickoff-quipu-canon-v1.2/`](../kickoff-quipu-canon-v1.2/)
and [`../deploy-kickoff-quipu-canon-v1.2/`](../deploy-kickoff-quipu-canon-v1.2/).

Those scripts shipped Quipu Canon v1.2 emission code and then deployed
it to production on 2026-05-03. **This script rolls out the 2026-05-10
refresh of the canonical-agents handover** across the same 10 ecosystem
workspaces, end-to-end.

> **Schema v2 — refactored 2026-05-10 after the tanastok deploy lesson.**
> Read [`LESSONS.md`](./LESSONS.md) before forking this directory for a
> future handover. Schema v2 splits emission and deploy into separate
> phases, pins each project's audited deploy command, adds a pre-flight
> gate (disk / SSH keys / human-setup), introduces a `--direct-deploy`
> escape hatch that bypasses the agent layer, and writes a structured
> phase-by-phase marker. The previous version's failure mode was a
> code-grep idempotency hit that wrote the implemented marker even though
> the deploy crashed on ENOSPC and never reached production. v2 makes
> that conflation impossible.

## What this rolls out

The refreshed handover lives at:

```
.cursor/rules/handover-canonical-agents-live-from-rope-2026-05-05.mdc
```

It documents five drift items since 2026-05-05 and lists per-project
action items (Art.17 endpoint integration, per-entity string emission,
`rope_createPersonalLedger` bootstrap, market-cap source swap, etc.).
Each project's section is encoded in [`workspaces.json`](./workspaces.json)
with explicit:

- `actions[]` — the recommendations this project owes
- `emission_signals[]` — code-presence checks (honoured in `--emission` /
  `--full`); a hit here means the code is on disk, NOT that production
  is running it
- `deploy_signals[]` — live-production checks (curl, dcscan stats, RPC,
  EAS update list); honoured in `--deploy` / `--direct-deploy` / `--full`
- `deploy_command` — the audited shell command to ship this project,
  harvested from `.cursor/quipu-canon-v1.2-deploy-plan.md`. `null` = no
  scripted deploy path; deploy is human-owned (write a blocker)
- `production_check` — `{url, expected_status, follow_redirects,
  ssh_state_check?, eas_check?, rpc_check?}` — verbatim verification
  contract used by both the agent (STEP 5) and `--direct-deploy`
- `min_disk_gib` / `ssh_keys_required` — pre-flight gate inputs
- `human_setup_required` / `human_setup_checklist` — defer deploy when
  the workspace needs one-off operator config (e.g. linking an EAS
  project, configuring a git remote)
- `sensitivity` — `normal | health | securities | real-money` — gates
  safety constraints in the primer
- `primer_extra` — a single concise project-specific paragraph

## Two ways to run it

### A. SDK dispatcher (`kickoff.ts`) — recommended

Uses [`@cursor/sdk`](https://www.npmjs.com/package/@cursor/sdk) and
`tsx`.

```bash
cd tools/kickoff-handover-canonical-agents-2026-05-10
npm install
export CURSOR_API_KEY=cursor_...

npm run dry-run        # safe preview, no API key needed
npm run plan-only      # agents complete STEP 0 + STEP 1 only
npm run emission       # agents through STEP 3 (commit + push, no deploy)
npm run deploy         # agents STEP 4-6 only (deploy + verify + marker)
npm run full           # all 7 steps end-to-end
npm run direct-deploy  # bypass agent, run audited deploy_command directly
npm run force-deploy   # alias: npm run deploy with --force-deploy
npm run force          # alias: npm run full with --force
npm run status         # walk logs/, summarise past runs
```

All commands accept extra flags: `npm run deploy -- --workspace=tanastok`.

### B. Bash fallback (`kickoff.sh`)

Uses the `cursor-agent` CLI directly (skips the npm install step).
For `--direct-deploy` and `--dry-run` modes, neither `cursor-agent` nor
`@cursor/sdk` is needed — only `jq`, `python3`, `curl`.

```bash
chmod +x kickoff.sh
./kickoff.sh --dry-run
./kickoff.sh --direct-deploy --workspace=tanastok    # no agent
./kickoff.sh --full                                  # agent-driven
```

The bash version produces the same `logs/run-<iso>.jsonl` machine-readable
output as the TS version, plus a free-form `logs/run-<iso>.log`.

## Modes (both A and B)

| Mode | Steps | Agent? | When to use |
|---|---|---|---|
| `--dry-run` | dispatcher prints per-workspace plan | no | sanity-check wiring |
| `--plan-only` | STEP 0 + STEP 1 | yes | review each project's plan file |
| `--emission` | STEP 0 + STEP 1-3 | yes | implement + commit + push, no deploy |
| `--deploy` | STEP 0 + STEP 4-6 | yes | ship already-committed code |
| `--full` | all 7 steps | yes | end-to-end |
| `--direct-deploy` | pre-flight + run audited script + verify | **no** | recovery path / when agent's STEP 0 falsely short-circuits |
| `--status` | walk logs/, summarise past runs | no | post-mortem |

## Flags

| Flag | Effect |
|---|---|
| `--workspace=<name>` | Run a single workspace (e.g. `tanastok`) |
| `--concurrency=<N>` | Default 2 for agent modes, 1 for `--direct-deploy` |
| `--model=<id>` | Cursor model. Default `composer-2` |
| `--force` | Bypass marker check entirely (any mode) |
| `--force-deploy` | Bypass marker only in deploy / direct-deploy / full |
| `--skip-preflight` | Skip disk / SSH-key / human-setup checks. Use only when pre-flight has a known false positive |

## The 7 steps (encoded in `primer.template.md`)

| # | Step | Side effects | Notes |
|---|---|---|---|
| 0 | **Idempotency check** — mode-aware signal split | none (write marker + exit if match) | v2: emission_signals vs deploy_signals |
| 1 | **Plan** — write the plan file | one file per workspace | |
| 2 | **Implement** — code edits per the action list | edits in workspace files | |
| 3 | **Commit + push** — only if `.git/` exists | commit + remote push | warns if no remote |
| 4 | **Deploy** — run pinned `deploy_command` | production deploy | v2: no more mtime gamble |
| 5 | **Verify** — run `production_check` | none | curl / RPC / EAS / SSH |
| 6 | **Marker** — append structured phase record | one file per workspace | v2: `## Phase: emission` / `## Phase: deploy` |

## Idempotency model (v2)

Three workspace-local files steer behavior:

| File | Written by | Means |
|---|---|---|
| `.cursor/handover-canonical-agents-2026-05-10-plan.md` | STEP 1 | "I read the handover and here's what I propose to do." |
| `.cursor/handover-canonical-agents-2026-05-10-implemented.marker` | STEP 6 | "I shipped this and verified it. Don't redo it." (structured: `## Phase: emission` + `## Phase: deploy`) |
| `.cursor/handover-canonical-agents-2026-05-10-blocked.md` | any failed step or `deferred-deploy` | "I stopped here because X. A human should look." |

The dispatcher reads the marker's structured sections:

- `--emission` mode skips when `## Phase: emission` is present.
- `--deploy` mode skips when `## Phase: deploy` is present (NOT just any
  marker — emission-only markers re-fire the deploy phase, which is the
  correct tanastok recovery behaviour).
- `--full` mode skips when both phases are present.
- Old free-form markers are read as `## Phase: emission` only.

To force a re-run without removing the marker:

```bash
npm run deploy -- --workspace=tanastok --force-deploy
# or
./kickoff.sh --deploy --workspace=tanastok --force-deploy
```

## Pre-flight gate (v2)

Before any agent or deploy script spawns:

| Gate | Effect |
|---|---|
| free disk < `min_disk_gib` | BLOCK (no work runs) |
| free disk < 2× `min_disk_gib` | WARN |
| `ssh_keys_required` path missing | BLOCK (only in deploy / full / direct-deploy) |
| `human_setup_required: true` | WARN — deploy phase will write a deferred-deploy blocker; emission still proceeds |
| `deploy_command: null` | WARN — deploy phase will defer (by design) |
| no git remote | WARN (commit will be local-only) |

`--skip-preflight` overrides all of these.

## Per-project deploy commands (audited)

These are pinned in `workspaces.json` → `deploy_command`, harvested from
each workspace's existing `.cursor/quipu-canon-v1.2-deploy-plan.md`:

| Workspace | deploy_command | Notes |
|---|---|---|
| `dcswap` | `null` | No automated path on disk; deploy is human-owned (rsync to dcswap-vps). |
| `moneymaker` | `./deploy.sh` | Default key `~/.ssh/shametrails_key`. |
| `tanastok` | `./deploy-clean-production.sh` | Blue-green zero-downtime; needs ≥ 5 GiB free for Next.js build. |
| `naturaproof` | `./deploy/deploy.sh` | Workspace has no `.git`. |
| `careaway` | `bash ./deploy-marketplace-admin.sh` | Multi-step (script + scp sync + remote pm2 restart). |
| `alteros` | `./deploy-to-vps.sh` | Full run (NOT `--skip-build`). Known flaky systemd activation. |
| `shametrails` | `eas update --branch production --message "..."` | `human_setup_required: true` until app.json + eas.json + git remote configured (see `human_setup_checklist`). |
| `datawallet-web` | `npm run build && ./deploy-to-vps.sh` | `dist/` is gitignored; build is required. |
| `datawallet-rn` | `./deploy.sh --no-push` | OTA / web-preview only; native binary release is human-only. |
| `syndicated` | `bash deploy/quick-deploy.sh` | Securities — pure config swap. |

## Per-project safety constraints

Hard-coded in the primer based on each workspace's `sensitivity`:

| Sensitivity | Workspaces | What's blocked |
|---|---|---|
| `health` | careaway | DB migrations on patient records |
| `securities` | syndicated | DB migrations on investor records |
| `real-money` | moneymaker | trading-logic / balance / order-book changes |
| `normal` | dcswap, tanastok, naturaproof, alteros, shametrails, datawallet-web, datawallet-rn | (no extra constraints) |

Native binary releases (App Store / Play Store) are blocked everywhere —
the React Native workspace `datawallet-rn` and the Expo workspace
`shametrails` may ship OTA / web-preview only.

## Concurrency and runtime expectations

| Mode | Default concurrency | Expected wall time across 10 projects |
|---|---|---|
| `--dry-run` | n/a (synchronous, no agents) | <5 seconds |
| `--plan-only` | 2 | 5–15 minutes |
| `--emission` | 2 | 30–90 minutes (depends on action complexity) |
| `--deploy` | 2 | 15–45 minutes |
| `--full` | 2 | 45–120 minutes |
| `--direct-deploy` | **1 forced** | varies per project (tanastok ≈ 13 min on its own) |

Concurrency is capped at 2 by default because deploys touch shared
production infra. `--direct-deploy` is forced to 1 unconditionally — two
parallel SSH/rsync streams from the same laptop fight each other and
have caused EAS push and PM2 restart contention.

## What this kickoff is NOT

- **Not a rollback tool.** If a deploy fails halfway, you roll back
  using the project's own rollback method. The blocker file tells you
  what went wrong but not how to undo it.
- **Not CI/CD.** It runs once when you tell it to.
- **Not a fleet-state audit.** For live state see
  `https://dcscan.io/api/v1/stats` and `https://agents.datachain.network/`.
- **Not allowed to modify deploy infrastructure.** Agents use what's
  on disk in each project. If something is broken, the agent writes a
  blocker — it does not silently patch a deploy script. The dispatcher
  itself can (and does) bypass the agent for `--direct-deploy`, but it
  still runs the project's audited script verbatim — never an
  improvised one.

## What's encoded vs what each agent figures out

| Encoded in `workspaces.json` (the dispatcher knows) | Each agent figures out (from its own filesystem) |
|---|---|
| Workspace path | Which files implement the actions |
| Per-project action list | Whether the existing build / lint / test passes |
| Emission signals + deploy signals | Whether those signals actually hit at this moment |
| Audited `deploy_command` | Whether to run the full or `--maintenance` variant (rare) |
| `production_check` contract | Whether the post-deploy state matches the contract |
| Sensitivity tier | Whether a proposed change crosses the sensitivity gate |
| Marker / plan / blocker filenames | Whether the workspace has `.git/` |
| `min_disk_gib` / `ssh_keys_required` | (read by the dispatcher; agent inherits the result) |
| `human_setup_required` flag + checklist | (read by the dispatcher; agent inherits) |

This split is deliberate. The dispatcher is the same across all 10
projects; the per-project intelligence lives in the agent the
dispatcher spawns. v2 moved more state into `workspaces.json` (audited
deploy commands, structured production checks, pre-flight inputs) so
the agent has fewer chances to gamble.

## Files

| File | Purpose |
|---|---|
| `workspaces.json` | Source of truth — paths, actions, signals, deploy commands, production checks |
| `primer.template.md` | The agent prompt template |
| `kickoff.ts` | SDK dispatcher (Option A) |
| `kickoff.sh` | bash CLI fallback (Option B) |
| `LESSONS.md` | **Read first** if you're forking this for a future handover |
| `package.json` / `tsconfig.json` / `.gitignore` | Node project scaffolding |
| `logs/` | Auto-populated per run (`.jsonl` machine-readable + `.log` text) |
| `README.md` | This file |

## Pre-flight checklist before `npm run full`

- [ ] Cursor API key in `CURSOR_API_KEY` env var
- [ ] Refreshed handover at `.cursor/rules/handover-canonical-agents-live-from-rope-2026-05-05.mdc` is current
- [ ] You ran `npm run dry-run` and the per-workspace summary looks right (pre-flight warnings + marker state visible)
- [ ] You ran `npm run plan-only` and inspected each `.cursor/handover-canonical-agents-2026-05-10-plan.md`
- [ ] At least 50 GiB free disk on the laptop (Next.js builds across multiple workspaces eat space fast)
- [ ] SSH keys listed in `workspaces.json` → `ssh_keys_required` are present (`~/.ssh/shametrails_key`, `~/.ssh/careaway_admin_key`)
- [ ] You're OK with deploying to production for all selected workspaces
- [ ] You can intervene quickly if the Rope RPC, dcscan API, or any one project's prod URL goes red

When all eight are checked, `npm run full` (or `./kickoff.sh --full`).

If any single project surfaces unexpectedly during a `--full` run:

```bash
# Inspect:
cat /Users/.../<workspace>/.cursor/handover-canonical-agents-2026-05-10-blocked.md

# Re-deploy just that one (bypass the agent, run audited script):
npm run direct-deploy -- --workspace=<name>

# Or force-replay the agent flow if you suspect a stale marker:
npm run deploy -- --workspace=<name> --force-deploy
```
