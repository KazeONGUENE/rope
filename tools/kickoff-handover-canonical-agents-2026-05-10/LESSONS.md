# LESSONS — what 2026-05-10 taught us about handover dispatchers

The 2026-05-10 canonical-agents handover rollout was the third generation
of the per-project agent-fan-out pattern (after `kickoff-quipu-canon-v1.2`
and `deploy-kickoff-quipu-canon-v1.2`). It surfaced two failure modes
that the v1.2 generation got away with by luck. This file is the
forward-looking record so the **next** rollout — whatever the next
handover is — doesn't repeat them.

If you are about to fork this directory for a future handover, **read
this first**, then port the schema-v2 contracts (`workspaces.json`,
`primer.template.md`, `kickoff.ts`, `kickoff.sh`) into the new dir.

---

## Lesson 1 — emission and deploy are different phases (the **tanastok ENOSPC**)

### What happened

We launched `npm run full --workspace=tanastok`. The agent's STEP 0
ran `rg "kind: \"asset\""` against the source tree, found a hit in
`src/lib/quipu-canon-emission.ts`, decided "the action is already
done", wrote the implemented marker, and exited cleanly.

In reality the deploy-clean-production.sh build had crashed two days
earlier on **ENOSPC** (laptop disk had filled up to 4.1 GiB free; Next.js
16 needs ~5 GiB just for the build trace files). The committed code was
sitting on `main` but nothing had ever shipped to https://tanastok.io.
The marker fooled the dispatcher into reporting success.

I (the operator) had to (a) free disk, (b) `rm` the marker, (c) bypass
the agent layer entirely, (d) run `./deploy-clean-production.sh`
manually, (e) verify HTTP 200 from `https://tanastok.io/home`, and (f)
overwrite the marker with an honest post-deploy record.

### Why the v1 schema let it happen

The v1 `workspaces.json` had **one** signal list per project
(`already_done_signals`) and STEP 0 short-circuited on **any** signal.
Code presence and live-production-status were treated as
interchangeable evidence. They are not.

### What v2 changed

1. **Signal split.** `emission_signals` (code-presence checks) and
   `deploy_signals` (live-production checks) are independent. STEP 0 in
   `--emission` mode honours emission_signals only; `--deploy` honours
   deploy_signals only; `--full` requires both.

2. **Structured marker.** The marker now has explicit `## Phase: emission`
   and `## Phase: deploy` sections. A re-run in `--deploy` mode against
   an emission-only marker re-fires the deploy phase instead of
   short-circuiting. Old free-form markers are read as
   `## Phase: emission` only — they don't immunise the deploy phase.

3. **Pinned `deploy_command`.** Each project's audited deploy command is
   now stored verbatim in `workspaces.json` (harvested from
   `.cursor/quipu-canon-v1.2-deploy-plan.md`). STEP 4 uses that command
   instead of re-deriving it via `ls -lt deploy*.sh`. The mtime gamble
   is over: tanastok ships `deploy.sh`, `deploy-blue-green.sh`,
   `deploy-direct.sh`, `deploy-existing-build.sh`, `deploy-standalone.sh`,
   `deploy-homepage-v3.sh`, `deploy-direct-vps.sh`, `deploy-clean-production.sh`,
   `monitor-deployment.sh`, `trigger-deployment.sh` — only the last
   May-2026 one is correct, and it is now pinned.

4. **Pre-flight gate.** `min_disk_gib` (5 for tanastok), `ssh_keys_required`
   (`~/.ssh/shametrails_key`), git-remote presence are now checked
   before any agent or deploy script spawns. ENOSPC at the wrong time
   is a `skipped-preflight-blocker` exit, not a failed build mid-stream.

5. **`--direct-deploy` mode.** The recovery path I had to invent on
   2026-05-10 (bypass agent, run the audited script, verify) is now a
   first-class mode. Operators no longer need to re-derive it under
   pressure.

6. **`--force` / `--force-deploy` flags.** When the marker is stale or
   misleading, override it without `rm`-ing.

### How to spot this failure in your own future rollout

- Watch for `STEP 0` short-circuiting on signal 1 of N. If signal 1 is
  a code-grep and the others are live-API checks, you are in
  pre-tanastok territory. Either narrow the signal list to deploy-only
  or migrate to schema v2.
- Watch the agent timeline: if the marker is written **before** any
  deploy-related stdout (no `npm run build`, no `rsync`, no health
  check), the deploy never happened.
- Cross-check the live URL. If `curl -sS -o /dev/null -w "%{http_code}"
  <project_url>` doesn't show recent activity but the marker says DONE,
  the marker is lying.

---

## Lesson 2 — workspace-state mismatch isn't a deploy bug, it's a setup bug (**shametrails / EAS**)

### What happened

shametrails was reported as a deploy block by every dispatcher run with
the diagnosis "no automated deploy script". The operator confirmed and
then provided context: there **is** an EAS-OTA deploy path
(`eas update --branch production`), and the project lives at
`https://expo.dev/accounts/datachainfoundation/projects/shametrails`,
with a known iOS submission profile (`PM964XKLSQ` for the primary
target, `GQ2QI5GE3QV1` for the +Skywatchers variant under bundle ID
`com.shametrails.shametrails.9lkvv3`, Apple Team `Q5UBRU9F2C`).

The local workspace was, however, in a **vibecode template** state
(`app.json` had `slug: "vibecode"`, `owner: null`, no `eas.json`, no
`extra.eas.projectId`, no git remote, EAS auth on the wrong account
`datachainers` instead of `datachainfoundation`). No `eas update`
invocation can succeed against this state — even if you patched the
agent to discover the EAS command, the project link is missing.

### Why the v1 schema couldn't represent this

Two distinct gates were collapsed into one:
- "Is there a scripted deploy path?" (yes — `eas update`)
- "Is the workspace correctly linked to the production target?" (no —
  vibecode template state)

v1 either had to mark shametrails BLOCKED (the safe choice it took) or
silently fail mid-deploy. Neither captures the real story.

### What v2 changed

1. **`human_setup_required: true` flag.** When the workspace is in a
   state that cannot deploy without one-off operator config, this flag
   tells the dispatcher to defer the deploy phase rather than block on
   it. Emission can still ship.

2. **`human_setup_checklist[]`.** Verbatim list of one-off setup steps
   the operator must perform. Surfaced by pre-flight as a warning and
   embedded in the agent's primer.

3. **Audit trail of project metadata.** The shametrails entry now stores
   the EAS project URL, EAS Submit ID, App Store Connect API Key IDs,
   Apple Team ID, and bundle identifier in `deploy_command_notes`.
   When the operator (or a future agent) walks the human_setup_checklist,
   the IDs they need are right there.

### Pattern for any future EAS / mobile / store-bound project

```jsonc
{
  "name": "your-mobile-app",
  "deploy_command": "eas update --branch production --message \"<msg>\"",
  "deploy_command_notes": "EAS-OTA path. Owner: <expo-account>. Project: https://expo.dev/.../<slug>. Native binary path (eas build + eas submit) is HUMAN-ONLY because store review is required. Apple Team: <id>. App Store Connect API key id: <id>. iOS bundle: <bundleId>.",
  "human_setup_required": true,
  "human_setup_checklist": [
    "Update app.json: owner, slug, expo.ios.bundleIdentifier, expo.android.package",
    "Run `eas init --id <projectId>` to link to the existing Expo project",
    "Create eas.json with build/submit profiles referencing the App Store Connect API key",
    "Add a git remote for code persistence",
    "Run `eas whoami` and confirm correct account; `eas login` if mismatched"
  ],
  "production_check": {
    "url": null,
    "eas_check": "eas update:list --branch production --limit 1 --json"
  }
}
```

OTA via `eas update` is safe to automate (no store review). `eas
build` + `eas submit` should remain `human_setup_required: true` and
out of the agent path indefinitely.

---

## Lesson 3 — concurrency is a deploy hazard, not a speed-up

When `--concurrency=2` runs two `--full` agents at once, both can race
for the same SSH socket / rsync target / Docker registry / EAS push
slot. We saw this on 2026-05-10 with paired runs of datawallet-web and
datawallet-rn (both push via SSH to the same VPS).

v2 forces `--direct-deploy` to concurrency 1 unconditionally. Agent
modes still default to 2 because most of the wall-clock time in those
modes is the agent thinking, not the deploy itself. If you are
deploying to a new shared production surface, drop concurrency to 1.

---

## Lesson 4 — verification must be cheap, automatic, and run **after** deploy

The `production_check` block in `workspaces.json` is the contract:
deploy is DONE if and only if its checks pass. v1 had no analogue — STEP
5 was free-form prose, and most agents either skipped it or wrote
"site looks fine" without curling.

v2:

1. `production_check.url` + `expected_status` + `follow_redirects` →
   `curl -sS -o /dev/null -w "%{http_code}" -L? <url>` is a 1-second
   verification any operator or agent can run.
2. `production_check.ssh_state_check` for cases where the public URL
   doesn't reflect deploy state (alteros: systemctl is-active).
3. `production_check.eas_check` for OTA channel timestamp checks.
4. `production_check.rpc_check` for chain-state RPC calls.

`--direct-deploy` runs (1) and (2) automatically post-deploy. Agent
flows are instructed (in primer STEP 5) to run all four kinds.

---

## Lesson 5 — markers must be tamper-evident and self-describing

The v1 marker was a free-text 1-paragraph note. Anyone (agent, operator,
re-run) could overwrite it without losing the prior record's evidence.

The v2 marker is **append-only by convention**: each phase appends a
`## Phase: <emission|deploy>` block with timestamped evidence. Multiple
re-deploys produce multiple `## Phase: deploy` blocks; readers see the
full timeline.

The dispatcher now appends to existing markers rather than overwriting,
so an emission-only run followed by a deploy-only run produces a complete
record.

---

## Migration path for the **next** handover

When you fork this directory for the next refresh:

1. Copy `workspaces.json`, `primer.template.md`, `kickoff.ts`,
   `kickoff.sh`, `package.json`, `tsconfig.json`, `LESSONS.md`,
   `README.md` to the new directory.
2. Update the marker filenames in `workspaces.json` to the new date.
3. Update the `handover_reference` to the new handover .mdc path.
4. Update each project's `actions[]`, `emission_signals[]`,
   `deploy_signals[]` to match the new handover content.
5. **Audit each project's `deploy_command`** — re-run
   `ls -lt deploy*.sh` and check whether the command has been replaced
   since the last rollout. If you find a newer deploy script, the
   operator must approve before you pin the new one. (This audit is
   the per-handover human-in-the-loop step that schema v2 explicitly
   makes visible.)
6. Re-validate `production_check` URLs (the project may have moved
   hosts).
7. **Do not skip pre-flight.** Disk usage, SSH keys, and the EAS-style
   `human_setup_required` gates have caught real problems three runs in
   a row.

The schema is `workspaces.json` → `schema_version: 2`. If you change
the shape, bump the version and update both dispatchers' parsers.
