/**
 * Quipu Canon v1.2 — ecosystem DEPLOY kickoff
 * ===========================================
 *
 * Sister script to ../kickoff-quipu-canon-v1.2/. The emission kickoff
 * shipped v1.2 emission CODE to GitHub for nine of ten projects on
 * 2026-05-03. This script ships that code to PRODUCTION by spawning one
 * Cursor agent per workspace and instructing each agent to:
 *
 *   1. Verify the v1.2 emission marker is present (precondition).
 *   2. DISCOVER its own most recent deploy method by inspecting its own
 *      filesystem and git history — never hardcoded by us, because the
 *      same project may have five competing deploy scripts and only one
 *      reflects current practice (Tanastok being the canonical example:
 *      `deploy.sh` from Nov 2025 vs `deploy-clean-production.sh` from
 *      May 2026 — using the older one would deploy obsolete config).
 *   3. Print the chosen method to stdout (audit trail) BEFORE running.
 *   4. Run the deploy.
 *   5. Verify the production endpoint reflects the new code (per project).
 *   6. Write `.cursor/quipu-canon-v1.2-deployed.marker` on success.
 *
 * Philosophy: the laptop is the source of truth. The agent treats its
 * own workspace as the authoritative record of how this project gets
 * deployed.
 *
 * Usage:
 *   export CURSOR_API_KEY=cursor_...
 *   npm install
 *   npm run dry-run         # preview without launching agents
 *   npm run discover-only   # launch agents, but they only DISCOVER + report
 *                           # the chosen deploy method; do not execute
 *   npm run kickoff         # launch + execute deploys (default concurrency=2)
 *   npm run status          # summarise past runs from logs/
 */

import {
  Agent,
  CursorAgentError,
  type RunResult,
} from "@cursor/sdk";
import { existsSync, mkdirSync, readFileSync, appendFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const LOG_DIR = join(__dirname, "logs");
const RUN_LOG = join(LOG_DIR, `run-${new Date().toISOString().replace(/[:.]/g, "-")}.jsonl`);

/** Hard precondition: emission must already be shipped before deploy. */
const EMISSION_SHIPPED_MARKER = ".cursor/quipu-canon-v1.2-emission-shipped.marker";
/** Sister marker, written on successful deploy. Idempotency anchor. */
const DEPLOYED_MARKER = ".cursor/quipu-canon-v1.2-deployed.marker";
/** Agent writes here when it cannot proceed and wants you to look. */
const BLOCKED_REPORT = ".cursor/quipu-canon-v1.2-deploy-blocked.md";

interface Workspace {
  name: string;
  cwd: string;
  /**
   * Optional, project-specific knowledge that genuinely helps the agent
   * make the right choice (e.g. "this project has BOTH a Vercel config
   * and a deploy.sh; Vercel reflects current practice"). Keep as short
   * as possible — the discovery itself is the agent's job, not ours.
   */
  primerExtra?: string;
}

const WORKSPACES: Workspace[] = [
  {
    name: "dcswap",
    cwd: "/Users/kazealphonseonguene/Downloads/dcswap",
    primerExtra:
      "Production VPS: dcswap-vps (92.243.26.114). The indexer + frontend live at /opt/dcswap on " +
      "that VPS. Look for the most recently modified script that actually does an `ssh dcswap-vps` " +
      "or `rsync … dcswap-vps:`; that is current practice. The .github/workflows/dcswap-ci.yml " +
      "runs tests only — it does NOT deploy. After deploy, restart the indexer service and " +
      "verify https://dcswap.net/v1/prices returns the latest priceMechanism.version.",
  },
  {
    name: "moneymaker",
    cwd: "/Users/kazealphonseonguene/Downloads/moneymaker",
    primerExtra:
      "Trading bot — has deploy.sh + deploy/systemd/*.service files. Deploy means: rsync to the " +
      "trading host, install/refresh systemd units, restart. Be EXTRA careful: this bot moves " +
      "real money. If you cannot identify a recent successful deploy in the git log or " +
      "deployment.log files, write a blocker report instead of guessing.",
  },
  {
    name: "tanastok",
    cwd: "/Users/kazealphonseonguene/Downloads/tanastok-app",
    primerExtra:
      "MULTIPLE competing deploy scripts: deploy.sh (Nov 2025), deploy-blue-green.sh, " +
      "deploy-existing-build.sh, deploy-direct.sh, deploy-standalone.sh, deploy-clean-production.sh " +
      "(May 2026 — most recent). Use mtime + git log + the deployment-*.log files to choose the " +
      "ONE that reflects current practice. Do not assume `deploy.sh` is correct just because " +
      "it has the canonical name. The deploy/ subdir contains pm2.config.cjs and .env.production " +
      "which point at the runtime topology.",
  },
  {
    name: "naturaproof",
    cwd: "/Users/kazealphonseonguene/Downloads/NaturaProof-platform",
    primerExtra:
      "Workspace has NO .git directory — emission code lives only on this laptop, not on a " +
      "remote. Step 0 for you: check whether deploy/ contains a script that pulls from a " +
      "different repo, or whether it's a pure laptop-to-VPS push. If the latter, that script " +
      "IS the source of truth. If neither, write a blocker report.",
  },
  {
    name: "careaway",
    cwd: "/Users/kazealphonseonguene/Downloads/Careways_health_Connect",
    primerExtra:
      "BOTH vercel.json AND netlify.toml present — usually means one is dead and one is live. " +
      "Check git log for the most recent deploy-related commit and check production-host headers " +
      "(curl -sI on the live URL) to determine which is currently serving traffic. HEALTH DATA: " +
      "if the deploy involves migrations on patient data, STOP and write a blocker — that needs " +
      "human approval, not an agent.",
  },
  {
    name: "alteros",
    cwd: "/Users/kazealphonseonguene/alteros",
    primerExtra:
      "Has deploy.sh + Dockerfile + .github/workflows/. Possibilities: bash deploy script, " +
      "docker push, or GH Actions on push to main. Check which actually moved code to prod most " +
      "recently — likely the Dockerfile route given the rest of the stack.",
  },
  {
    name: "shametrails",
    cwd: "/Users/kazealphonseonguene/Downloads/shametrails",
    primerExtra:
      "No deploy automation found at workspace root by my outer audit. That doesn't mean none " +
      "exists — search subdirs (server/, web/, mobile/, infra/, ops/, scripts/), check " +
      "package.json scripts, check for fastlane, eas.json, expo.json. If there's genuinely no " +
      "deploy mechanism shipped in this repo, write a blocker report saying so.",
  },
  {
    name: "datawallet-web",
    cwd: "/Users/kazealphonseonguene/Downloads/Datawallet+",
    primerExtra:
      "Vite/React web app. No deploy automation found at root by my outer audit — but check " +
      "package.json scripts (look for `deploy`, `release`, `publish`, `build:prod`), netlify or " +
      "vercel CLI invocations in any .sh, and the dist/ output convention. If the web app is " +
      "served via the Datachain Rope nginx static dir, deploy = rsync dist/ to that nginx root.",
  },
  {
    name: "datawallet-rn",
    cwd: "/Users/kazealphonseonguene/Downloads/DATAWALLET+ReactNative",
    primerExtra:
      "React Native + has vercel.json (probably for the web preview build) AND deploy.sh. Mobile " +
      "apps may use eas.json (Expo) — check for it. Distinguish the OTA / web-preview deploy " +
      "from the store-bound mobile build. ONLY deploy the surface where the v1.2 emission code " +
      "actually runs (likely the JS staking layer, not the native shell).",
  },
  {
    name: "syndicated",
    cwd: "/Users/kazealphonseonguene/Downloads/LUZRAN GROUP/syndicated.ltd",
    primerExtra:
      "Has deploy/ subdir but no script at root in my outer audit. Inspect deploy/ contents — " +
      "infrastructure-as-code (terraform, ansible, pulumi), Dockerfile, k8s manifests, or a " +
      "shell script. Securities platform — like careaway, if there's any DB migration touching " +
      "investor records, STOP and write a blocker report.",
  },
];

interface RunArgs {
  dryRun: boolean;
  discoverOnly: boolean;
  status: boolean;
  workspaceFilter?: string;
  concurrency: number;
  model: string;
}

function parseArgs(argv: string[]): RunArgs {
  const args: RunArgs = {
    dryRun: false,
    discoverOnly: false,
    status: false,
    concurrency: 2,
    model: "composer-2",
  };
  for (const a of argv.slice(2)) {
    if (a === "--dry-run") args.dryRun = true;
    else if (a === "--discover-only") args.discoverOnly = true;
    else if (a === "--status") args.status = true;
    else if (a.startsWith("--workspace=")) args.workspaceFilter = a.slice("--workspace=".length);
    else if (a.startsWith("--concurrency=")) args.concurrency = parseInt(a.slice("--concurrency=".length), 10);
    else if (a.startsWith("--model=")) args.model = a.slice("--model=".length);
    else throw new Error(`Unknown flag: ${a}`);
  }
  return args;
}

function buildPrimer(ws: Workspace, args: RunArgs): string {
  const executePolicy = args.discoverOnly
    ? `DISCOVERY-ONLY MODE: complete steps 1–4 below, then STOP. Do NOT run the deploy command. Write the chosen method to .cursor/quipu-canon-v1.2-deploy-plan.md and exit.`
    : `EXECUTE MODE: complete all steps. Run the deploy. Verify. Write the marker.`;

  return [
    `You are the project AI agent for ${ws.name}.`,
    ``,
    `On 2026-05-03 the Datachain Rope agent shipped Quipu Canon v1.2 emission CODE into your`,
    `repo and pushed it to GitHub. That code is NOT yet running in production. Your job is`,
    `to ship it to production using THIS project's own deploy method.`,
    ``,
    `Operating mode: ${executePolicy}`,
    ``,
    `STEP 1 — VERIFY PRECONDITION`,
    ``,
    `  Check that ${EMISSION_SHIPPED_MARKER} exists in this workspace.`,
    `  If it doesn't exist, STOP. Write a one-line blocker to ${BLOCKED_REPORT} and exit.`,
    `  We will not deploy code that hasn't passed the emission kickoff.`,
    ``,
    `STEP 2 — DISCOVER THE DEPLOY METHOD`,
    ``,
    `  This project's correct deploy method LIVES IN THIS WORKSPACE. Do not assume anything.`,
    `  Inspect, in this order, the following sources of truth on this LAPTOP:`,
    ``,
    `    a. Most recently MODIFIED deploy*.sh / release*.sh / publish*.sh / push*.sh scripts.`,
    `       Use \`ls -lt\` (or stat) — choose by mtime, NOT alphabetic order. Filename`,
    `       canonicality (e.g. plain "deploy.sh") is a TRAP if a newer differently-named`,
    `       script reflects current practice (Tanastok ships deploy.sh AND`,
    `       deploy-clean-production.sh; the latter, dated May 2026, is the live one).`,
    ``,
    `    b. package.json — scripts named deploy*, release*, publish*, ship*, prod*, build:prod.`,
    ``,
    `    c. Makefile — targets named deploy, release, prod, ship.`,
    ``,
    `    d. Vercel / Netlify / Fly / Render configs at the root + project ID files in .vercel/.`,
    ``,
    `    e. Dockerfile + docker-compose*.yml + any \`docker push\` / \`docker stack deploy\``,
    `       calls in shell scripts.`,
    ``,
    `    f. .github/workflows/*.yml — workflow_dispatch entries that are explicitly deploys`,
    `       (search for "deploy", "release", "production"). DO NOT mistake a CI test workflow`,
    `       (the dcswap-ci.yml is one) for a deploy.`,
    ``,
    `    g. deploy/ subdirectory contents — pm2 / systemd / k8s / terraform / ansible.`,
    ``,
    `    h. Recent git log entries with messages mentioning "deploy", "release", "ship", "prod".`,
    ``,
    `    i. Local deployment*.log / *.deploy.log files — they record exactly which command`,
    `       was run last time and when it succeeded.`,
    ``,
    `  PROJECT-SPECIFIC HINT: ${ws.primerExtra ?? "(none — discovery is entirely on you)"}`,
    ``,
    `STEP 3 — CHOOSE`,
    ``,
    `  Pick the ONE method most consistent with current practice. Apply these tie-breakers:`,
    ``,
    `    • Most recently modified script wins over older one with a more "canonical" name.`,
    `    • A script whose recent execution shows up in deployment*.log / git log wins over`,
    `      a script with no execution evidence (it may be dead code).`,
    `    • A vercel/netlify/CI deploy wins over a shell script if the live production URL's`,
    `      headers (curl -sI) name that platform AND the platform's last build is recent.`,
    `    • If two paths look equally valid, prefer the one with a recent successful log entry.`,
    `    • If you cannot decide, STOP and write a blocker — do not flip a coin on production.`,
    ``,
    `STEP 4 — ANNOUNCE THE PLAN (audit trail, MANDATORY before running)`,
    ``,
    `  Write the chosen method to .cursor/quipu-canon-v1.2-deploy-plan.md as:`,
    `    - Chosen command(s)`,
    `    - Why this one (which signals: mtime, log evidence, recent commit, etc.)`,
    `    - What signals you DID NOT pick and why (other deploy*.sh scripts that exist but`,
    `      are stale, etc.) — this catches mistakes before they ship.`,
    `    - Production target (host/URL/service)`,
    `    - Expected verification step (what URL or service to hit to confirm success)`,
    ``,
    `STEP 5 — EXECUTE (skip in DISCOVERY-ONLY MODE)`,
    ``,
    `  Run the chosen deploy command from the workspace cwd. Stream output. If the script`,
    `  prompts for input (sudo password, yes/no), STOP and write a blocker — agents must`,
    `  not blindly answer interactive prompts on production.`,
    ``,
    `STEP 6 — VERIFY`,
    ``,
    `  Confirm the deploy worked. The verification depends on the project — examples:`,
    `    • Web app:  curl -sI <production URL> and check for an updated build hash, asset`,
    `      revision, or new HTTP header.`,
    `    • API:      curl <health endpoint> and check that the response includes a v1.2`,
    `      marker (version field, new field name, new event_type, etc.).`,
    `    • systemd:  ssh into the host (only if the deploy script already has the SSH key`,
    `      and target wired in — do not invent SSH targets) and check service is "active".`,
    `    • Mobile:   confirm the new bundle/OTA is published; native binary deploys are`,
    `      out of scope here (require app store review).`,
    ``,
    `  If verification fails, write a blocker. Do NOT write the deployed marker on a`,
    `  partial / unverified deploy.`,
    ``,
    `STEP 7 — WRITE MARKER`,
    ``,
    `  On verified success, write ${DEPLOYED_MARKER} containing:`,
    `    - ISO 8601 timestamp`,
    `    - The exact command executed`,
    `    - The verification signal observed (URL + status / version field / etc.)`,
    `    - Production target(s) updated`,
    ``,
    `  Then exit. The kickoff script uses this marker to skip already-deployed workspaces`,
    `  on re-run.`,
    ``,
    `HARD CONSTRAINTS`,
    ``,
    `  • DO NOT modify deploy scripts, .env files, Dockerfiles, or systemd units. Use what`,
    `    is on disk. If something is broken, write a blocker — do not silently patch and`,
    `    deploy a different version of the deploy mechanism than the maintainer intended.`,
    `  • DO NOT run any deploy that touches a different project's infrastructure. Stay in`,
    `    this workspace's lane.`,
    `  • DO NOT push to git unless the deploy mechanism legitimately requires a tagged`,
    `    commit (some Vercel/Netlify flows do; if so, tag with quipu-canon-v1.2-prod-<date>).`,
    `  • DO NOT escalate privileges. If sudo is required and the script doesn't already`,
    `    handle it, write a blocker.`,
    ``,
    `Begin.`,
  ].join("\n");
}

interface RunRecord {
  workspace: string;
  status: "skipped" | "finished" | "error" | "cancelled" | "startup-failed";
  runId?: string;
  durationMs?: number;
  error?: string;
  startedAt: string;
  endedAt?: string;
}

function logRun(record: RunRecord): void {
  if (!existsSync(LOG_DIR)) mkdirSync(LOG_DIR, { recursive: true });
  appendFileSync(RUN_LOG, JSON.stringify(record) + "\n");
}

async function runOne(ws: Workspace, args: RunArgs): Promise<RunRecord> {
  const startedAt = new Date().toISOString();

  if (!existsSync(ws.cwd)) {
    const record: RunRecord = {
      workspace: ws.name,
      status: "startup-failed",
      startedAt,
      endedAt: new Date().toISOString(),
      error: `workspace not found at ${ws.cwd}`,
    };
    logRun(record);
    console.error(`[${ws.name}] STARTUP-FAIL workspace missing`);
    return record;
  }

  // Hard precondition: skip if the emission marker isn't present.
  const emissionMarker = join(ws.cwd, EMISSION_SHIPPED_MARKER);
  if (!existsSync(emissionMarker)) {
    const record: RunRecord = {
      workspace: ws.name,
      status: "skipped",
      startedAt,
      endedAt: new Date().toISOString(),
      error: `precondition failed: ${EMISSION_SHIPPED_MARKER} not present — run the emission kickoff first`,
    };
    logRun(record);
    console.log(`[${ws.name}] SKIP (no emission marker — run ../kickoff-quipu-canon-v1.2/ first)`);
    return record;
  }

  // Idempotency: skip if already deployed (unless the operator removed
  // the marker to force a re-deploy).
  const deployedMarker = join(ws.cwd, DEPLOYED_MARKER);
  if (existsSync(deployedMarker) && !args.discoverOnly) {
    const record: RunRecord = {
      workspace: ws.name,
      status: "skipped",
      startedAt,
      endedAt: new Date().toISOString(),
      error: `already deployed (marker present); rm ${DEPLOYED_MARKER} to force re-deploy`,
    };
    logRun(record);
    console.log(`[${ws.name}] SKIP (already deployed — marker present)`);
    return record;
  }

  const primer = buildPrimer(ws, args);

  if (args.dryRun) {
    console.log(`[${ws.name}] DRY-RUN cwd=${ws.cwd}`);
    console.log(`  precondition: ${existsSync(emissionMarker) ? "OK (emission marker present)" : "FAIL"}`);
    console.log(`  primer length: ${primer.length} chars, mode=${args.discoverOnly ? "discover-only" : "execute"}`);
    return {
      workspace: ws.name,
      status: "skipped",
      startedAt,
      endedAt: new Date().toISOString(),
      error: "dry-run",
    };
  }

  console.log(`[${ws.name}] LAUNCH cwd=${ws.cwd} mode=${args.discoverOnly ? "discover-only" : "execute"}`);
  const t0 = Date.now();
  try {
    const result: RunResult = await Agent.prompt(primer, {
      apiKey: process.env.CURSOR_API_KEY!,
      model: { id: args.model },
      local: { cwd: ws.cwd },
    });
    const endedAt = new Date().toISOString();
    const durationMs = result.durationMs ?? Date.now() - t0;

    const status: RunRecord["status"] =
      result.status === "finished"
        ? "finished"
        : result.status === "cancelled"
          ? "cancelled"
          : "error";

    const record: RunRecord = {
      workspace: ws.name,
      status,
      runId: result.id,
      durationMs,
      startedAt,
      endedAt,
      error: status === "finished" ? undefined : `run terminated with status=${result.status}`,
    };
    logRun(record);
    console.log(`[${ws.name}] ${status.toUpperCase()} runId=${result.id} duration=${(durationMs / 1000).toFixed(1)}s`);
    return record;
  } catch (err) {
    const endedAt = new Date().toISOString();
    const durationMs = Date.now() - t0;
    const isCursorErr = err instanceof CursorAgentError;
    const errMsg = err instanceof Error ? err.message : String(err);
    const record: RunRecord = {
      workspace: ws.name,
      status: "startup-failed",
      durationMs,
      startedAt,
      endedAt,
      error: `${isCursorErr ? "CursorAgentError" : "Error"}: ${errMsg}`,
    };
    logRun(record);
    console.error(`[${ws.name}] STARTUP-FAIL ${errMsg}`);
    return record;
  }
}

async function pmap<T, R>(
  items: readonly T[],
  concurrency: number,
  worker: (item: T) => Promise<R>,
): Promise<R[]> {
  const out = new Array<R>(items.length);
  let next = 0;
  const runners = Array.from({ length: Math.max(1, Math.min(concurrency, items.length)) }, async () => {
    while (true) {
      const i = next++;
      if (i >= items.length) return;
      out[i] = await worker(items[i]!);
    }
  });
  await Promise.all(runners);
  return out;
}

function summarizePastRuns(): void {
  if (!existsSync(LOG_DIR)) {
    console.log("No previous runs (logs/ does not exist).");
    return;
  }
  const fs = require("node:fs") as typeof import("node:fs");
  const files = fs.readdirSync(LOG_DIR).filter((f) => f.endsWith(".jsonl")).sort();
  if (files.length === 0) {
    console.log("No previous runs (logs/ is empty).");
    return;
  }
  for (const f of files) {
    const lines = readFileSync(join(LOG_DIR, f), "utf8").trim().split("\n").filter(Boolean);
    console.log(`\n=== ${f} (${lines.length} runs) ===`);
    for (const line of lines) {
      const r = JSON.parse(line) as RunRecord;
      console.log(
        `  ${r.workspace.padEnd(16)} ${r.status.padEnd(16)} ` +
          (r.durationMs != null ? `${(r.durationMs / 1000).toFixed(1)}s ` : "       ") +
          (r.runId ? `run=${r.runId}` : r.error ?? ""),
      );
    }
  }
}

async function main(): Promise<void> {
  const args = parseArgs(process.argv);

  // Eagerly create LOG_DIR before anything else can write to it. Lazy
  // creation breaks `npm run kickoff | tee logs/foo.log` because tee
  // opens the file before the script's first log call. Same fix as
  // ../kickoff-quipu-canon-v1.2/ after we hit it on 2026-05-03.
  if (!existsSync(LOG_DIR)) mkdirSync(LOG_DIR, { recursive: true });

  if (args.status) {
    summarizePastRuns();
    return;
  }

  if (!args.dryRun && !process.env.CURSOR_API_KEY) {
    console.error("CURSOR_API_KEY is not set. Get one from https://cursor.com/dashboard/cloud-agents");
    process.exit(1);
  }

  const targets = args.workspaceFilter
    ? WORKSPACES.filter((w) => w.name === args.workspaceFilter)
    : WORKSPACES;

  if (targets.length === 0) {
    console.error(`No workspaces match --workspace=${args.workspaceFilter ?? ""}.`);
    console.error("Known: " + WORKSPACES.map((w) => w.name).join(", "));
    process.exit(1);
  }

  console.log(
    `Quipu Canon v1.2 DEPLOY kickoff — ${targets.length} workspace(s), concurrency=${args.concurrency}, model=${args.model}, dry-run=${args.dryRun}, discover-only=${args.discoverOnly}`,
  );
  console.log(`Run log: ${RUN_LOG}\n`);

  const results = await pmap(targets, args.concurrency, (ws) => runOne(ws, args));

  const counts = {
    finished: results.filter((r) => r.status === "finished").length,
    error: results.filter((r) => r.status === "error").length,
    startupFailed: results.filter((r) => r.status === "startup-failed").length,
    skipped: results.filter((r) => r.status === "skipped").length,
  };

  console.log("\n=== summary ===");
  console.log(`  finished:       ${counts.finished}`);
  console.log(`  error:          ${counts.error}`);
  console.log(`  startup-failed: ${counts.startupFailed}`);
  console.log(`  skipped:        ${counts.skipped}`);

  if (counts.startupFailed > 0) process.exit(1);
  if (counts.error > 0) process.exit(2);
  process.exit(0);
}

main().catch((err) => {
  console.error("Unhandled error in deploy kickoff:", err);
  process.exit(1);
});
