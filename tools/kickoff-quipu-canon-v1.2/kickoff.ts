/**
 * Quipu Canon v1.2 — ecosystem kickoff
 * ====================================
 *
 * Runs one Cursor agent per ecosystem workspace, in parallel, with a primer
 * that points each agent at its already-committed v1.2 emission handover.
 * The agents read the handover, implement the per-entity string emission
 * described, run their tests, commit, and push.
 *
 * Why a one-shot script: Cursor doesn't auto-start agents on restart. This
 * is the closest thing to "autonomous on restart" — autonomous on one
 * `npm run kickoff` invocation. Re-runs are safe; workspaces with a
 * shipped marker are skipped.
 *
 * Usage:
 *   export CURSOR_API_KEY=cursor_...
 *   npm install
 *   npm run dry-run            # preview without launching
 *   npm run kickoff            # launch all
 *   npm run kickoff -- --workspace=moneymaker --concurrency=1
 *   npm run status             # summarize past runs from logs/
 */

import {
  Agent,
  CursorAgentError,
  type RunResult,
} from "@cursor/sdk";
import { existsSync, mkdirSync, readFileSync, writeFileSync, appendFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const LOG_DIR = join(__dirname, "logs");
const RUN_LOG = join(LOG_DIR, `run-${new Date().toISOString().replace(/[:.]/g, "-")}.jsonl`);

// Marker file that, when present in a workspace, means the v1.2 emission
// has already been shipped and we should skip kickoff. Agents we launch
// are instructed (in the primer) to write this file as the last step
// after their commit lands. Re-running this script is therefore safe.
const SHIPPED_MARKER = ".cursor/quipu-canon-v1.2-emission-shipped.marker";

interface Workspace {
  /** Short stable id, used in CLI flags and log filenames. */
  name: string;
  /** Absolute path to the workspace root. */
  cwd: string;
  /** Workspace-relative path of the v1.2 handover file. */
  rulePath: string;
  /** Optional override for the primer (rare; the default works for most). */
  primerExtra?: string;
}

const WORKSPACES: Workspace[] = [
  {
    name: "dcswap",
    cwd: "/Users/kazealphonseonguene/Downloads/dcswap",
    rulePath: ".cursor/rules/handover-quipu-canon-v1.2-from-rope-2026-05-03.mdc",
    primerExtra:
      "Strings DCSwap owns: contract(Router), contract(Factory), one asset string per LP pool, " +
      "one wallet string per bot wallet (62 HD-derived wallets per the Feb-26 handover). " +
      "DCScan now exposes /api/v1/dcswap/bots which renders v12String:null until you ship — " +
      "that is the visible motivation.",
  },
  {
    name: "moneymaker",
    cwd: "/Users/kazealphonseonguene/Downloads/moneymaker",
    rulePath: ".cursor/rules/handover-quipu-canon-v1.2-from-rope-2026-05-03.mdc",
    primerExtra:
      "Strings Moneymaker owns: wallet(per bot), did(per operator), contract(DatachainFlashArb.sol). " +
      "Critical: subscribe to DCSwap Router/Factory contract strings via rope_getString and " +
      "abort trading when an upgrade is detected before your strategy is reviewed.",
  },
  {
    name: "tanastok",
    cwd: "/Users/kazealphonseonguene/Downloads/tanastok-app",
    rulePath: ".cursor/rules/handover-quipu-canon-v1.2-from-rope-2026-05-03.mdc",
    primerExtra:
      "Strings Tanastok owns: contract(DCNFT per asset), contract(ERC-3643 per asset), " +
      "asset(per tokenized RWA). Each MintingComplete / SubscriptionFilled event becomes one knot " +
      "on the corresponding asset string.",
  },
  {
    name: "naturaproof",
    cwd: "/Users/kazealphonseonguene/Downloads/NaturaProof-platform",
    rulePath: ".cursor/rules/handover-quipu-canon-v1.2-from-rope-2026-05-03.mdc",
    primerExtra:
      "Strings NaturaProof owns: contract(per verification module), asset(per biodiversity proof), " +
      "did(per certifier). Verification events / proof revocations / claim issuance must become knots " +
      "with the canonical naturaproof.* event_type values.",
  },
  {
    name: "careaway",
    cwd: "/Users/kazealphonseonguene/Downloads/Careways_health_Connect",
    rulePath: ".cursor/rules/handover-quipu-canon-v1.2-from-rope-2026-05-03.mdc",
    primerExtra:
      "Strings Careaway owns: did(per patient with explicit consent), asset(per insurance policy), " +
      "contract(per claim/payout module). HEALTH DATA: encrypt knot payloads under OES key shred; " +
      "use rope_untieKnot for GDPR Art. 17 / HIPAA right-to-delete.",
  },
  {
    name: "alteros",
    cwd: "/Users/kazealphonseonguene/alteros",
    rulePath: ".cursor/rules/handover-quipu-canon-v1.2-from-rope-2026-05-03.mdc",
    primerExtra:
      "Strings Alteros owns: did(per Alteros instance), contract(Cerber security policy bundle), " +
      "asset(per cognitive skill). Daily digests, policy updates, and security events become knots; " +
      "subscribe to ecosystem state changes via the registry.",
  },
  {
    name: "shametrails",
    cwd: "/Users/kazealphonseonguene/Downloads/shametrails",
    rulePath: ".cursor/rules/handover-quipu-canon-v1.2-from-rope-2026-05-03.mdc",
    primerExtra:
      "Strings shametrails owns: did(per user), asset(per post), contract(moderation engine). " +
      "Posts/edits/moderation actions/engagement digests become knots; PII goes through OES shred " +
      "with hashes on-chain and content on IPFS; rope_untieKnot enforces takedowns.",
  },
  {
    name: "datawallet-web",
    cwd: "/Users/kazealphonseonguene/Downloads/Datawallet+",
    rulePath: ".cursor/rules/handover-quipu-canon-v1.2-from-rope-2026-05-03.mdc",
    primerExtra:
      "Vite/React web app — divergences from the React Native handover are listed in the rule. " +
      "Mirror the canonical DID URI scheme; render `kind` and `knot_count` consistently; add " +
      "/strings and /strings/:kind/:string_id routes.",
  },
  {
    name: "datawallet-rn",
    cwd: "/Users/kazealphonseonguene/Downloads/DATAWALLET+ReactNative",
    // No standalone .mdc — directive is inline at top of DCStakingAPIService.ts
    rulePath: "src/services/DCStakingAPIService.ts",
    primerExtra:
      "The Quipu Canon v1.2 directive is the JSDoc block at the very top of " +
      "src/services/DCStakingAPIService.ts. Implement it for the staking domain: emit one asset " +
      "string per pool, one did string per staker; favour Option B (event-rate-limited emission) " +
      "for financial events; coordinate the DID URI scheme with the web app.",
  },
  {
    name: "syndicated",
    cwd: "/Users/kazealphonseonguene/Downloads/LUZRAN GROUP/syndicated.ltd",
    rulePath: ".cursor/rules/handover-quipu-canon-v1.2-from-rope-2026-05-03.mdc",
    primerExtra:
      "Strings Syndicated.ltd owns: did(per accredited investor), contract(per SPV), " +
      "asset(per syndicated deal cap table). Subscription commitments / distributions / " +
      "accreditation changes become knots; sensitive PII on IPFS; coordinate with master-node " +
      "erasure for MiFID II / GDPR Art. 17 compliance.",
  },
];

interface RunArgs {
  dryRun: boolean;
  status: boolean;
  workspaceFilter?: string;
  concurrency: number;
  /**
   * Cursor model id used for every agent in this kickoff. Default `composer-2`
   * is fast and cheap; switch to a thinking model for deeper changes.
   */
  model: string;
}

function parseArgs(argv: string[]): RunArgs {
  const args: RunArgs = {
    dryRun: false,
    status: false,
    concurrency: 4,
    model: "composer-2",
  };
  for (const a of argv.slice(2)) {
    if (a === "--dry-run") args.dryRun = true;
    else if (a === "--status") args.status = true;
    else if (a.startsWith("--workspace=")) args.workspaceFilter = a.slice("--workspace=".length);
    else if (a.startsWith("--concurrency=")) args.concurrency = parseInt(a.slice("--concurrency=".length), 10);
    else if (a.startsWith("--model=")) args.model = a.slice("--model=".length);
    else throw new Error(`Unknown flag: ${a}`);
  }
  return args;
}

function buildPrimer(ws: Workspace): string {
  const ruleAbs = resolve(ws.cwd, ws.rulePath);
  return [
    `You are the project AI agent for ${ws.name}. The Datachain Rope agent (working in /Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/datachain-rope) shipped Quipu Canon v1.2 on 2026-05-03 and dropped a handover into your repo.`,
    ``,
    `READ FIRST (mandatory): ${ruleAbs}`,
    ``,
    `Then implement the v1.2 emission layer it specifies for THIS project. Focus on:`,
    ``,
    `  1. The per-entity strings this project owns and is responsible for.`,
    `  2. The knot payload schemas with the canonical event_type values.`,
    `  3. Wiring those emissions into the existing event/lifecycle hooks already in this codebase (do not invent new pipelines unless the handover demands it).`,
    `  4. Honour the v1.2 naming policy: never reuse "string" to mean "cord-anchor count"; expose kind + knot_count in any public-facing surface.`,
    `  5. Use the live RPC at https://erpc.datachain.network — method rope_appendToLedger today, switch to rope_appendToString once rope-node v1.2.1 ships.`,
    ``,
    `Project-specific guidance from the Datachain Rope agent: ${ws.primerExtra ?? "(none — follow the handover verbatim)"}`,
    ``,
    `Operating constraints:`,
    ``,
    `  - WORKING DIRECTORY is THIS workspace. Do not modify files outside it.`,
    `  - Run the project's existing test/lint commands before committing.`,
    `  - Commit with a clear conventional-commit message: "feat(quipu-canon-v1.2): emit per-entity knots per Datachain Rope handover".`,
    `  - If the workspace has a tracked git repo and a configured remote, push the commit. If not, just commit and surface the situation in your final report.`,
    `  - As the LAST step (after the commit lands), create the marker file at .cursor/quipu-canon-v1.2-emission-shipped.marker containing today's ISO date plus the commit SHA. The kickoff script uses this marker to skip already-shipped workspaces on re-run.`,
    `  - If you genuinely cannot proceed (rule unclear, breaking-change risk, missing credentials), STOP and write a short blocker report to ./.cursor/quipu-canon-v1.2-blocked.md instead of guessing.`,
    ``,
    `Do not ask for confirmation. Begin.`,
  ].join("\n");
}

interface RunRecord {
  workspace: string;
  /**
   * `cancelled` is included for completeness — it's a `RunResultStatus`
   * the SDK exposes when the user (or another process) cancels a run
   * mid-flight via `run.cancel()`. The kickoff script itself never
   * cancels, but reflecting the value lets `npm run status` report
   * accurately.
   */
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

  // Skip on shipped marker so re-runs are safe.
  const markerPath = join(ws.cwd, SHIPPED_MARKER);
  if (existsSync(markerPath)) {
    const record: RunRecord = {
      workspace: ws.name,
      status: "skipped",
      startedAt,
      endedAt: new Date().toISOString(),
      error: `marker present at ${SHIPPED_MARKER}; remove it to re-run`,
    };
    logRun(record);
    console.log(`[${ws.name}] SKIP (already shipped — marker present)`);
    return record;
  }

  // Validate workspace exists and rule is readable so we fail early
  // instead of inside the agent's first tool call.
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
  const rulePath = join(ws.cwd, ws.rulePath);
  if (!existsSync(rulePath)) {
    const record: RunRecord = {
      workspace: ws.name,
      status: "startup-failed",
      startedAt,
      endedAt: new Date().toISOString(),
      error: `handover rule not found at ${ws.rulePath}`,
    };
    logRun(record);
    console.error(`[${ws.name}] STARTUP-FAIL rule missing at ${rulePath}`);
    return record;
  }

  const primer = buildPrimer(ws);

  if (args.dryRun) {
    console.log(`[${ws.name}] DRY-RUN cwd=${ws.cwd}`);
    console.log(`  rule: ${rulePath}`);
    console.log(`  primer: ${primer.split("\n")[0]}…`);
    return {
      workspace: ws.name,
      status: "skipped",
      startedAt,
      endedAt: new Date().toISOString(),
      error: "dry-run",
    };
  }

  console.log(`[${ws.name}] LAUNCH cwd=${ws.cwd}`);
  const t0 = Date.now();
  try {
    const result: RunResult = await Agent.prompt(primer, {
      apiKey: process.env.CURSOR_API_KEY!,
      model: { id: args.model },
      local: { cwd: ws.cwd },
    });
    const endedAt = new Date().toISOString();
    // Prefer the SDK-reported duration when available, otherwise fall
    // back to wall-clock — useful when the SDK omits durationMs (it's
    // optional in `RunResult`).
    const durationMs = result.durationMs ?? Date.now() - t0;

    // Map RunResultStatus → our internal status. "finished" is the only
    // success terminal; "error" / "cancelled" are surfaced as-is so the
    // operator can tell the two failure modes apart in the log file.
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

/**
 * Bounded-concurrency map: runs `worker(item)` for each item in `items`,
 * with at most `concurrency` runs in flight at once. Returns the results
 * in the original order. Pure, no external deps.
 */
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

  // Create the log directory eagerly, BEFORE anything else writes to it.
  // Lazy creation inside `logRun()` is correct for the script itself, but
  // it breaks shells that pipe `npm run kickoff | tee logs/foo.log`,
  // because `tee` opens the file before the script gets a chance to
  // mkdir. The user saw this on 2026-05-03 — the inner agents all
  // succeeded, but `tee` errored on missing dir and the pipeline exit
  // code was 1, masking a clean run as a failure.
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
    `Quipu Canon v1.2 kickoff — ${targets.length} workspace(s), concurrency=${args.concurrency}, model=${args.model}, dry-run=${args.dryRun}`,
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

  // Exit code per the SDK skill's guidance:
  //   0 only when at least one finished and none of the in-flight runs failed,
  //   1 if any startup failure (auth, config, network — fix env and retry),
  //   2 if any in-flight run reported error status (inspect logs/transcripts).
  if (counts.startupFailed > 0) process.exit(1);
  if (counts.error > 0) process.exit(2);
  process.exit(0);
}

main().catch((err) => {
  console.error("Unhandled error in kickoff:", err);
  process.exit(1);
});
