# Canonical-Agents Handover Rollout — agent primer (2026-05-10, schema v2)

This file is the single source of truth for the prompt sent to each
project's Cursor agent during the rollout of the Datachain Rope
canonical-agents handover refresh of 2026-05-10.

The dispatcher (`kickoff.ts` / `kickoff.sh`) substitutes the
`{{PLACEHOLDER}}` tokens at run time per workspace. Do NOT edit those
tokens unless you also update both dispatchers.

> **Schema v2 rationale (the tanastok lesson, 2026-05-10).** The previous
> primer treated emission and deploy as a single phase: any
> already-done signal was enough to write the implemented marker. That
> caused a false-positive on tanastok — a code-grep matched
> `kind: "asset"` in the issuer, the marker got written, and the actual
> blue-green deploy was silently skipped because the build had crashed
> on ENOSPC and never reached production. Schema v2 splits signals into
> `emission_signals` (code presence) and `deploy_signals` (live
> production proof) and pins each project's audited `deploy_command`
> directly in `workspaces.json` so STEP 4 stops gambling on script
> discovery. STEP 6 now writes a structured marker with separate
> emission/build/deploy phase records — re-running in `--deploy` mode
> against an emission-only marker re-fires the deploy phase instead of
> short-circuiting.

---

You are the project AI agent for **{{NAME}}** (cwd: `{{CWD}}`).

The Datachain Rope core agent refreshed its always-applied canonical-agents
handover on **2026-05-10**. The refreshed handover lives at:

`{{HANDOVER_PATH}}`

That handover documents five drift items since the original 2026-05-05
publication and lists per-project action items. Your job is to implement
the action items that this project (`{{NAME}}`) owes, ship them to
production, and write a marker on success.

Operating mode: **{{MODE}}**

Modes:
- `plan-only` — complete STEP 0 + STEP 1 only, then stop. Do NOT change code.
- `emission` — STEP 0 through STEP 3 (idempotency check, plan, implement, commit, push). Do NOT deploy.
- `deploy` — STEP 0, STEP 4, STEP 5, STEP 6 (idempotency check, deploy, verify, marker). Assumes emission already happened.
- `full` — all steps end-to-end: idempotency, plan, implement, commit, push, deploy, verify, marker.

This project's audited deploy command (from
`workspaces.json` → `deploy_command`):

```
{{DEPLOY_COMMAND}}
```

Notes from the audit:

> {{DEPLOY_COMMAND_NOTES}}

If `deploy_command` is `null`, no scripted deploy path is recorded — STEP
4 must STOP and write a deferred-deploy blocker rather than improvise.

This project's production check (from `workspaces.json` →
`production_check`):

```json
{{PRODUCTION_CHECK}}
```

This project's `human_setup_required` flag is **{{HUMAN_SETUP_REQUIRED}}**.

If `human_setup_required` is true, the workspace needs one-off operator
config before any deploy can run. The checklist:

{{HUMAN_SETUP_CHECKLIST}}

In that case, this run should EMIT only (STEP 0–3) and STEP 4 must write
a `deferred-deploy` blocker that copies the checklist verbatim. Do NOT
write the implemented marker — emission alone is not "done".

---

## STEP 0 — IDEMPOTENCY CHECK (mandatory in every mode)

Before doing any work, verify whether the canonical-agents recommendations
for this project are already implemented **for the current mode**. If
they are, write the marker and exit successfully — do **not** redo the
work.

**Mode-aware signal split (schema v2):**

- `plan-only` / `dry-run` — never short-circuit. Always proceed to STEP 1.
- `emission` — short-circuit only if **at least one** `emission_signal`
  hits.
- `deploy` — short-circuit only if **at least one** `deploy_signal` hits
  (i.e. live production already runs the new code).
- `full` — short-circuit only if **at least one** `emission_signal` AND
  **at least one** `deploy_signal` both hit. Hitting only emission means
  the code is shipped but production is still on the old build — DO
  proceed to STEP 4 to ship it.

Check, in order:

1. **Marker file already present?**
   If `{{IMPLEMENTED_MARKER}}` exists in this workspace, parse its
   structured sections (lines beginning with `## Phase: emission` /
   `## Phase: deploy`).
   - In `--emission` mode: skip if a `## Phase: emission` section is
     present and dated on or after `2026-05-10`.
   - In `--deploy` mode: skip if a `## Phase: deploy` section is present
     and dated on or after `2026-05-10` AND its evidence still resolves
     (the `production_check` URL still returns the expected status).
   - In `--full` mode: skip only if BOTH phase sections are present and
     deploy evidence still resolves.
   - Old free-form markers (no `## Phase:` headers) from previous
     rollouts are treated as `## Phase: emission` only — they don't
     cover the deploy phase. Re-run in `--deploy` mode WILL re-fire
     deploy on those.

2. **Already-done signals.** Run the appropriate signal set for the
   current mode:

   **emission_signals** (honoured in `emission` and `full` modes):

{{EMISSION_SIGNALS}}

   **deploy_signals** (honoured in `deploy` and `full` modes):

{{DEPLOY_SIGNALS}}

   Search the codebase for code-presence signals using ripgrep (`rg`).
   Run live-API checks listed (`curl -sS https://...`,
   `eas update:list ...`, RPC calls). If the mode-appropriate set hits
   per the rules above, write `{{IMPLEMENTED_MARKER}}` (see STEP 6 for
   structure) and exit with status 0.

3. **Sensitivity gate.** This project's sensitivity is **{{SENSITIVITY}}**.
   - `normal` — proceed normally.
   - `health` — STOP and write a blocker if any recommendation requires
     a database migration touching patient records. Recommended scope is
     UI / API plumbing only.
   - `securities` — STOP and write a blocker if any recommendation
     requires a database migration touching investor records. Same
     scope rule as `health`.
   - `real-money` — recommendations on this project must not modify
     trading logic, balances, or order books. The expected scope is
     bootstrap-only changes (e.g. one-time ledger creation calls) that
     are protocol-level read-only side-effect-free. If a recommendation
     pushes outside that scope, STOP and write a blocker.

---

## STEP 1 — PLAN (audit trail, mandatory before any code change)

Read the refreshed handover at `{{HANDOVER_PATH}}` in full. Pay
attention to:
- §"Per-project notes" → find the section for `{{NAME}}` (or its
  closest equivalent — e.g. `For DCSwap`, `For Tanastok`, `For Datawallet+`,
  `For Careways_health_Connect & shametrails`, `For Syndicated.ltd`,
  `For NaturaProof`).
- §"Drift since 2026-05-05" — particularly §3 (per-entity string
  emission gap) and §1 (price source change).
- §"Action required from peer agents" → "Optional but recommended".

Then write `{{PLAN_FILE}}` with the following sections:

1. **What this project owes** — copy the action items below verbatim
   and annotate each with whether you can do it in this rollout, defer
   it, or block on it:

{{ACTIONS}}

2. **Implementation approach** — for each action you can do in this
   rollout, name the files you intend to touch and the rough shape of
   the change.
3. **What you will NOT do** — anything outside the sensitivity gate,
   anything requiring infra changes you can't make, anything ambiguous.
4. **Verification plan** — for each action, describe how you will
   confirm it shipped (curl on the `production_check` URL, semantic-agent
   search hit, `rope_globalStats` `stringsByKind` count, `eas update:list`
   timestamp, etc.). Use the `production_check` block from the header
   of this primer as your contract — the marker you write in STEP 6 must
   include literal evidence from that check.
5. **Project-specific note from the dispatcher**: {{PRIMER_EXTRA}}

If `MODE=plan-only`, stop here. Print the path of the plan file to
stdout and exit with status 0.

---

## STEP 2 — IMPLEMENT THE CODE CHANGES

For each action you committed to in STEP 1, make the code change in this
workspace. Hard rules:

- **Only touch files genuinely related to the action.** No drive-by
  refactors. No comment-only churn. No dependency upgrades.
- **Preserve existing tests**; add new tests only if the action has a
  testable surface and the project already has a test convention you can
  follow.
- **Match the project's existing style** (linter config, formatter,
  naming conventions). Run the existing format / lint command if there
  is one.
- **No new top-level dependencies** unless the action explicitly
  requires one (e.g. `fetch`-based POST to compliance-agent does not
  need a new HTTP library if the project already uses `fetch` or `axios`).

The action items, copied here for convenience:

{{ACTIONS}}

For each one, do the actual edit. Show your work to stdout. After all
edits, run the project's existing build / typecheck / test command if
there is one (look for `npm run build`, `npm run typecheck`, `npm test`,
`pnpm build`, `cargo build`, etc.). If the build fails, STOP and write a
blocker — do not commit broken code.

---

## STEP 3 — COMMIT + PUSH (only if the workspace is a git repo)

If `.git/` exists in this workspace:
1. Stage only the files you edited (`git add <those files>` — never
   `git add .`).
2. Commit with a message starting with the literal prefix
   `chore(handover-2026-05-10):` followed by a 1-sentence summary of
   what changed for this project.
3. If the workspace has a remote and the current branch tracks one,
   `git push` to that remote. If there is no remote, note it in the
   marker — that is a known pattern for some workspaces (e.g.
   shametrails, naturaproof) and is NOT a blocker by itself.
4. If the project has no `.git/` directory, this step is a no-op
   (NaturaProof is the known case). Note that in the marker.

If `MODE=emission`, stop after STEP 3 — do not deploy.

---

## STEP 4 — DEPLOY (skip if MODE=emission, run if MODE=deploy or MODE=full)

**Use the audited deploy command from the header of this primer.** Do
NOT improvise. Do NOT pick the most-recently-modified `deploy*.sh` —
that gamble is what got us into the tanastok ENOSPC misadventure on
2026-05-10. The audited command is:

```
{{DEPLOY_COMMAND}}
```

If that value is `null`:
- This project has no scripted, auditable deploy path on this laptop.
- Write a `deferred-deploy` blocker to `{{BLOCKED_REPORT}}` containing:
  the actions implemented, the commit SHA, and the literal text
  "deploy_command is null in workspaces.json — deploy is human-owned
  for this workspace; see deploy_command_notes for the audit trail".
- Do NOT improvise an `ssh`/`rsync` step. Do NOT pick a random
  `deploy*.sh`. Exit with status 0 (emission shipped, deploy deferred).

If `human_setup_required` is true (see header), the workspace is in a
state where the deploy_command cannot run yet. Write a `deferred-deploy`
blocker that copies the human_setup_checklist verbatim, then exit with
status 0.

Otherwise:

1. **Run the pre-flight check first** (the dispatcher will have already
   done this, but do it again as a belt-and-braces): verify free disk
   exceeds the project's `min_disk_gib`, verify each
   `ssh_keys_required` path exists, verify the working tree's HEAD
   matches the commit you just pushed in STEP 3.

2. **Run the audited deploy command verbatim** from the workspace `cwd`.
   Stream output. If it prompts for interactive input you can't
   provide (sudo password, yes/no), STOP and write a blocker — agents
   do not blindly answer interactive prompts on production.

3. **The deploy script may include the build step internally.**
   Tanastok's `deploy-clean-production.sh`, datawallet-web's
   `npm run build && ./deploy-to-vps.sh`, alteros's `./deploy-to-vps.sh`
   all do their own build. Do NOT pre-build separately unless the
   audit notes say so.

4. If the deploy command exits non-zero, parse the tail of stdout for
   common failure modes (ENOSPC, "permission denied", "connection
   refused", "no such file") and quote the relevant line in the
   blocker. Do not retry — that is the human's call.

---

## STEP 5 — VERIFY

Confirm the deploy worked and that the action items are visible in
production. Run the project's `production_check`:

```json
{{PRODUCTION_CHECK}}
```

Verification rules:

- If `url` is set: `curl -sS -o /dev/null -w "%{http_code}\n" -L?<url>`
  (use `-L` if `follow_redirects` is true). Compare to
  `expected_status`. If mismatched, STOP and write a blocker.
- If `ssh_state_check` is set: run that command verbatim. The expected
  output is project-specific and described in `deploy_command_notes`.
- If `eas_check` is set: run `eas update:list --branch <branch> --limit 1
  --json` and confirm the latest update timestamp is within 24h of the
  local commit you just pushed.
- If `rpc_check` is set: run the documented `curl -X POST ... /` against
  https://erpc.datachain.network and confirm the field described.

For deploy_signals defined in `workspaces.json`, also verify each one:
- A live URL response substring match (rendered HTML contains
  'gdpr/article17')
- A `dcscan.io/api/v1/stats` count comparison
  (`stringsByKind.<kind>.strings >= 1`)
- A `semantic-agent.datachain.network/v1/search` hit

If verification fails, STOP and write a blocker. Do NOT write the
implemented marker on a partial or unverified rollout.

---

## STEP 6 — WRITE STRUCTURED MARKER (schema v2)

On verified success, write `{{IMPLEMENTED_MARKER}}` in this exact
structure (markdown):

```markdown
<ISO 8601 timestamp>

## Phase: emission
- Commit SHA: <git rev-parse --short HEAD>
- Files touched: <comma-separated>
- Emission signals satisfied:
  - <signal 1>: <how/where it matched>
  - <signal 2>: ...
- Notes: <one-line description of what changed>

## Phase: deploy
- Deploy command: <the audited command verbatim>
- Started: <ISO 8601>
- Finished: <ISO 8601>
- Deploy signals satisfied:
  - <signal 1>: <evidence — e.g. "https://tanastok.io/home → HTTP 200, 141ms (curl response saved to logs/)">
  - <signal 2>: <evidence>
- Active slot / target: <if applicable: green/blue, channel name, k8s deployment id>
- Production verification:
  - <production_check.url>: HTTP <code> in <ms>ms (curl excerpt below)
  - <ssh_state_check output, if applicable>

## Result
- Code: SHIPPED (commit <sha>)
- Build: SHIPPED (<size, if known> — <build tool, if known>)
- Deploy: SHIPPED (<target host/URL>)
- Outstanding gaps: <e.g. "asset-string emission waiting on core rope_appendToString RPC">
```

If only the emission phase succeeded (e.g. `MODE=emission` or
`human_setup_required=true`), write only the `## Phase: emission`
section and the `## Result` section. Future `--deploy` runs will read
the marker, see no `## Phase: deploy`, and re-fire the deploy phase
correctly.

Then exit with status 0.

---

## HARD CONSTRAINTS (apply in every mode, every step)

1. **Do not modify deploy infrastructure.** No edits to the audited
   `deploy_command` script, `Dockerfile`, `.env*`, systemd units,
   vercel/netlify/fly configs, `.github/workflows/`. Use what is on
   disk. If something is broken, write a blocker.
2. **Do not push to git unless the deploy mechanism legitimately
   requires it.** Some Vercel/Netlify flows trigger on tag — that is
   fine. Otherwise, push at most once at the end of STEP 3.
3. **Do not escalate privileges.** If sudo is required and the script
   doesn't already handle it, write a blocker.
4. **Do not modify other projects' infrastructure.** Stay in this
   workspace's `cwd`.
5. **Do not silently retry knot writes** (per
   `handover-quipu-canon-v2-migration-2026-05-03.mdc` §"What you must
   NOT do" §4). Use the `?durable=true` flag for cases where you need
   durability-before-ack.
6. **Do not change the EVM-compat alias layer** or any
   `eth_*`-namespaced RPC behaviour. The DCR-20 / Quipu Canon v1.2
   surface is additive, never replacing.
7. **Do not modify `RopeString::compute_id` or any v1
   knot-hash code path** (per
   `quipu-canon-knot-hash-construction.mdc`). Knot-hash construction
   migration is its own multi-phase plan and is out of scope here.
8. **If the workspace requires a node restart** to pick up code
   changes (e.g. systemd-managed service), that restart MUST be done
   by the audited deploy_command — never by you directly.

---

## FAILURE MODES — write a blocker, don't push through

If any of these apply, STOP, write `{{BLOCKED_REPORT}}` with a
1-paragraph diagnosis + the literal command(s) you tried + what
happened, then exit with status 1:

- A precondition for the action isn't met (e.g. the
  `quipu-canon-v1.2-emission-shipped.marker` doesn't exist and the
  action requires it).
- The build or test suite fails after your edits.
- The deploy command prompts for input you can't provide.
- The deploy command runs but verification (STEP 5) fails.
- Sensitivity gate hit (health/securities migration, real-money
  trading-logic change, native binary release).
- `deploy_command` is `null` for this project — emission may still
  ship; deploy is deferred to human (this is `deferred-deploy`,
  written to the same blocker file).
- `human_setup_required` is true — same as above; emission may ship,
  deploy is deferred until the operator completes
  `human_setup_checklist`.

Blockers are honest signals. They are not failures of you the agent;
they are signals to the human operator that this workspace needs
attention beyond what an autonomous run can do safely.

---

## BEGIN

You have everything you need. Start with STEP 0.
