# Quipu Canon v1.2 — DEPLOY kickoff

Sister to [`../kickoff-quipu-canon-v1.2/`](../kickoff-quipu-canon-v1.2/).
That script shipped v1.2 emission CODE to GitHub. This one ships that
code to PRODUCTION by spawning one Cursor agent per workspace and
telling each one to **discover its own deploy method from its own
filesystem and use it**.

The laptop is the source of truth. We do not hardcode deploy commands
because, as Tanastok proved on 2026-05-03, the same project may carry
five competing deploy scripts (`deploy.sh`, `deploy-blue-green.sh`,
`deploy-existing-build.sh`, `deploy-direct.sh`, `deploy-standalone.sh`,
`deploy-clean-production.sh`) and only one — the most recently modified
+ most recently logged — reflects current practice. An outer script
that picks "the canonical one" by name guesses wrong half the time.

## Quick start

```bash
cd tools/deploy-kickoff-quipu-canon-v1.2
npm install
export CURSOR_API_KEY=cursor_...

# Phase 1 — see WHAT each agent would do, without running anything:
npm run discover-only

# Inspect each workspace's .cursor/quipu-canon-v1.2-deploy-plan.md, decide
# whether the agent picked the right method, then:

# Phase 2 — actually deploy:
npm run kickoff
```

## The two-phase flow (recommended)

This is the safe sequence, and the one I'd take if I were running it
the first time on this fleet:

### Phase 1: `npm run discover-only`

Each agent runs steps 1–4 of the primer:

1. **Verify precondition** — `.cursor/quipu-canon-v1.2-emission-shipped.marker` must exist; if not, agent writes a blocker and exits.
2. **Discover** the deploy method from this workspace's filesystem (mtime of `deploy*.sh`, `package.json` scripts, Makefile, Vercel/Netlify/Fly/Render configs, `Dockerfile`, `.github/workflows/`, `deploy/` subdir, recent git log entries with deploy-ish messages, `deployment*.log` execution traces).
3. **Choose** the one most consistent with current practice using explicit tie-breakers (mtime > naming, log-evidence > no-evidence, live-platform-headers > shell-script, etc.).
4. **Write** the chosen method to `.cursor/quipu-canon-v1.2-deploy-plan.md` with chosen command(s), why, what was rejected and why, target host/URL, and verification step.

Then it **stops**. No deploy is executed.

You inspect each plan file:

```bash
for ws in /Users/kazealphonseonguene/Downloads/dcswap \
         /Users/kazealphonseonguene/Downloads/moneymaker \
         /Users/kazealphonseonguene/Downloads/tanastok-app \
         /Users/kazealphonseonguene/Downloads/Careways_health_Connect \
         /Users/kazealphonseonguene/alteros \
         /Users/kazealphonseonguene/Downloads/shametrails \
         /Users/kazealphonseonguene/Downloads/Datawallet+ \
         /Users/kazealphonseonguene/Downloads/DATAWALLET+ReactNative \
         "/Users/kazealphonseonguene/Downloads/LUZRAN GROUP/syndicated.ltd" \
         /Users/kazealphonseonguene/Downloads/NaturaProof-platform; do
  echo "=== $(basename "$ws") ==="
  cat "$ws/.cursor/quipu-canon-v1.2-deploy-plan.md" 2>/dev/null || echo "  (no plan file — check blocker)"
  echo ""
done
```

If a plan looks wrong, edit the workspace state (e.g. delete the stale
`deploy.sh` so it stops being a candidate), or override by writing a
manually-edited `quipu-canon-v1.2-deploy-plan.md` yourself before
phase 2.

### Phase 2: `npm run kickoff`

Same agents pick up where they left off. They re-discover (idempotent)
and proceed through steps 5–7: execute the chosen command, verify, and
write `.cursor/quipu-canon-v1.2-deployed.marker` on success.

## Flags

| Flag | Default | Meaning |
|---|---|---|
| `--dry-run` | off | Don't launch agents at all. Just print the per-workspace plan. No API key needed. |
| `--discover-only` | off | Launch agents, but stop them after step 4 (write deploy plan, don't execute). |
| `--status` | off | Walk `logs/*.jsonl` and summarise past runs. Exits without launching. |
| `--workspace=<name>` | all 10 | Restrict to one workspace. |
| `--concurrency=<N>` | **2** | Max agents in flight at once. Lower than the emission kickoff because deploys touch shared infra (DB migrations, CDN purges, restart waves) and tend to interfere when stacked. Set to 1 for a strict serial deploy. |
| `--model=<id>` | `composer-2` | Cursor model id. Use a thinking model for tricky discoveries. |

## Safety constraints baked into the primer

These are written into every agent's instructions and are not optional:

1. **Hard precondition.** No emission marker → no deploy. Agent writes a blocker and exits.
2. **Idempotency marker.** Existing `quipu-canon-v1.2-deployed.marker` → skip. Operator must explicitly remove it to force a re-deploy.
3. **No editing of deploy infrastructure.** Agents may not modify `deploy*.sh`, `Dockerfile`, `.env*`, systemd units, vercel/netlify/fly configs. They use what's on disk. If something is broken, they write a blocker — they don't silently patch.
4. **No interactive prompts.** If the chosen deploy script needs sudo password / yes-no confirmation that the script doesn't already handle, the agent writes a blocker. We don't blindly answer prompts on production.
5. **No privilege escalation.** Same rule.
6. **No cross-workspace deploys.** Each agent stays in its own `cwd`.
7. **Sensitive-data carve-out.** For `careaway` (health) and `syndicated` (securities), if the chosen deploy includes a DB migration touching real records, the agent stops and writes a blocker — those need human approval, not an agent.
8. **No mobile binary deploys.** `datawallet-rn` will only deploy the JS / OTA / web-preview surface, not push to App Store / Play Store. Native binary releases require a human + store review.

## Recovery scenarios

**The agent picked the wrong deploy script.**
Open the plan file, see what was chosen and why. Either:
- Delete or move the wrong-but-tempting candidate so the agent stops choosing it (`mv deploy.sh deploy.sh.archived`), then `rm .cursor/quipu-canon-v1.2-deploy-plan.md` and re-run.
- Or manually edit `quipu-canon-v1.2-deploy-plan.md` to record the correct method, then re-run — the agent will see your plan and proceed.

**The deploy half-succeeded.**
The agent will NOT have written the deployed marker (verification step 6 must pass first). Inspect `.cursor/quipu-canon-v1.2-deploy-blocked.md` for what failed. Roll the production state back manually using the project's own rollback method, then re-run.

**The agent could not find a deploy method.**
The blocker file will say so. This is honest; the workspace genuinely doesn't ship to anywhere via the on-disk artifacts. Either add a deploy script and re-run, or deploy that one manually and just write the deployed marker yourself with `date -u +%FT%TZ > .cursor/quipu-canon-v1.2-deployed.marker`.

**You re-ran emission after deploy.**
The deployed marker is now stale. Remove it and re-run deploy:

```bash
rm <workspace>/.cursor/quipu-canon-v1.2-deployed.marker
npm run kickoff -- --workspace=<name>
```

## Known per-project notes (also in primer)

| Workspace | Key thing the agent should not get wrong |
|---|---|
| **dcswap** | The `dcswap-ci.yml` workflow runs tests; it is NOT a deploy. Production is at `/opt/dcswap` on dcswap-vps (92.243.26.114). Verify via `dcswap.net/v1/prices`. |
| **moneymaker** | Real-money trading bot. If unsure, blocker. |
| **tanastok** | `deploy-clean-production.sh` (May 2026) > `deploy.sh` (Nov 2025). Many decoys. |
| **naturaproof** | No `.git`. Likely laptop-to-VPS push. |
| **careaway** | Both vercel.json AND netlify.toml — one is dead. Health-data migrations require human approval. |
| **alteros** | Choose between deploy.sh, Dockerfile, GH Actions. Probably Docker. |
| **shametrails** | No deploy automation found at root by my outer audit. Search subdirs first; if genuinely none, blocker. |
| **datawallet-web** | Vite app. Likely dist/ → nginx static dir, or vercel/netlify if a config is hidden. |
| **datawallet-rn** | RN app — only the JS/OTA/web-preview surface, not native binaries. |
| **syndicated** | Securities platform — investor-record migrations need human approval. |

## What this kickoff is NOT

- **It is not a CI/CD system.** It runs once when you tell it to, by
  one operator on one laptop. If you want continuous deploy on git
  push, that's a different design (set up GH Actions per project).
- **It is not a rollback tool.** If the deploy fails halfway, you roll
  back using the project's own rollback method. The blocker file will
  tell you what went wrong but not how to undo it.
- **It is not an audit log of your fleet.** It records what THIS run
  did, not the historical state of every production endpoint. For
  fleet visibility, point at dcscan.io or the per-project dashboards.
