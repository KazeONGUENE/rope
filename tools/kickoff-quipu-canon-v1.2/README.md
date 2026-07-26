# Quipu Canon v1.2 — ecosystem kickoff

One-command kickoff for the ten ecosystem workspaces that received the
v1.2 emission handover on 2026-05-03. Spawns one local Cursor agent per
workspace, in parallel, and walks each through reading its handover and
shipping the per-entity knot emission described.

This is the closest thing to "autonomous on restart" — autonomous on
**one** invocation: `npm run kickoff`.

## Why this exists

Cursor desktop agents are interactive: they wake up when you open a
chat in a workspace, not when Cursor restarts. Without this script
you'd open ten chats and paste the same primer ten times.

`@cursor/sdk` lets us drive agents programmatically. This script
loads each workspace's handover path + project-specific context, builds
a focused primer, and launches all ten agents through `Agent.prompt()`
in parallel with bounded concurrency.

## Setup

```bash
cd tools/kickoff-quipu-canon-v1.2
npm install
export CURSOR_API_KEY=cursor_...   # https://cursor.com/dashboard/cloud-agents
```

## Usage

```bash
# Preview what will run, where, with which primer, without launching:
npm run dry-run

# Launch all 10 in parallel (concurrency 4):
npm run kickoff

# Run only one workspace, no parallelism, with a thinking model:
npm run kickoff -- --workspace=moneymaker --concurrency=1 --model=claude-4.5-sonnet-thinking

# Summarise past runs from logs/:
npm run status
```

### Flags

| Flag | Default | Meaning |
|---|---|---|
| `--dry-run` | off | Print plan, do not launch any agents. Doesn't need an API key. |
| `--status` | off | Walk `logs/*.jsonl` and print a per-workspace history summary. Exits without launching. |
| `--workspace=<name>` | all 10 | Restrict to one workspace. Names: `dcswap`, `moneymaker`, `tanastok`, `naturaproof`, `careaway`, `alteros`, `shametrails`, `datawallet-web`, `datawallet-rn`, `syndicated`. |
| `--concurrency=<N>` | 4 | Max agents in flight at once. 10 = full parallel; 1 = sequential. |
| `--model=<id>` | `composer-2` | Cursor model id used for every agent. |

## What each agent is told to do

Per-workspace primer is built in `kickoff.ts` (`buildPrimer`). The
shared backbone is:

1. **Read the handover** — absolute path to the `.mdc` (or, for
   `datawallet-rn`, the JSDoc inline at the top of
   `src/services/DCStakingAPIService.ts`).
2. **Implement the per-entity strings + knot payload schemas** named in
   the handover. Project-specific context (which strings, which event
   types, which constraints) is injected via `primerExtra`.
3. **Wire to existing event/lifecycle hooks** — don't invent new
   pipelines unless the handover explicitly asks for one.
4. **Honour the v1.2 naming policy** — never reuse "string" to mean
   cord-anchor count; expose `kind` + `knot_count` on every public
   surface.
5. **Use the live RPC** at `https://erpc.datachain.network` —
   `rope_appendToLedger` today, switch to `rope_appendToString` once
   `rope-node` v1.2.1 ships.
6. **Run the project's tests, commit, push** with a conventional
   commit message.
7. **Write the marker file** `.cursor/quipu-canon-v1.2-emission-shipped.marker`
   as the very last step. The script uses this on subsequent runs to
   skip workspaces that are already shipped.
8. **If genuinely blocked**, write a short blocker report to
   `.cursor/quipu-canon-v1.2-blocked.md` instead of guessing.

## Idempotency

Re-runs are safe. For each workspace, the script:

- Skips the workspace if `.cursor/quipu-canon-v1.2-emission-shipped.marker`
  exists in its working tree.
- Skips and reports `startup-failed` if either the workspace path or
  the handover rule path is missing on disk.
- Otherwise launches a fresh `Agent.prompt(...)` run and records the
  agent id, run id, status, and duration in
  `logs/run-<utc-timestamp>.jsonl`.

To re-run a single workspace after fixing a problem there:

```bash
rm "<workspace>/.cursor/quipu-canon-v1.2-emission-shipped.marker"
npm run kickoff -- --workspace=<name>
```

## Logs and exit codes

- `logs/run-*.jsonl` — one JSON line per workspace per invocation, with
  `agentId`, `runId`, `status`, `durationMs`, `startedAt`, `endedAt`,
  optional `error`. Inspect with `jq`, replay with `npm run status`.
- Process exit codes follow the SDK's two-failure-modes guidance:
  - **1** — at least one workspace failed at startup (auth, config,
    network, missing files). Fix the environment and re-run.
  - **2** — at least one workspace ran but finished with
    `result.status === "error"`. Inspect the run in
    https://cursor.com/dashboard/cloud-agents/<runId> or via
    `Agent.getRun(runId, { runtime: "local", cwd })` and fix what the
    agent reported.
  - **0** — all workspaces finished cleanly (or were skipped).

## When to run this

- After a fresh clone of the ecosystem.
- After Datachain Rope ships a relevant API change (e.g.
  `rope-node` v1.2.1 with `rope_appendToString`) and you want every
  ecosystem agent to migrate.
- After **you** restart Cursor and want every ecosystem agent to pick
  up where it left off without opening ten chats.

## Adding a new workspace

Append an entry to the `WORKSPACES` array in `kickoff.ts`. Required:
`name`, absolute `cwd`, and `rulePath` relative to `cwd`. Optional but
strongly recommended: `primerExtra` with the project-specific
responsibilities (which strings, which event types, which
constraints).

## Limitations

- **Local runtime only.** Each workspace is checked out on this Mac;
  the script uses `local: { cwd }`. To run cloud agents instead (for
  cleaner sandboxing or to open PRs against private GitHub repos) the
  workspaces would need to be pushable to GitHub first — three are
  currently uncommitted (`moneymaker`, `Datawallet+`,
  `DATAWALLET+ReactNative`) and four have no `.git` directory at all
  (`dcswap`, `alteros`, `shametrails`, `syndicated.ltd`,
  `NaturaProof-platform`). A future version of this script could opt
  per-workspace into cloud when the workspace has a clean tracked
  remote.
- **No live streaming UI.** `Agent.prompt(...)` is fire-and-forget; you
  see one summary line per workspace as it completes. For live token
  streams, port the launcher to `Agent.create() + agent.send() +
  run.stream()` and feed the stream into a TUI.
- **No automatic credential rotation.** `CURSOR_API_KEY` is read once
  at start. Long-running sessions should use `Agent.create` with a
  fresh credential cycle.
