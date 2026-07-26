/**
 * Canonical-Agents Handover Rollout — kickoff dispatcher (2026-05-10, schema v2)
 * ============================================================================
 *
 * Sister to:
 *   - ../kickoff-quipu-canon-v1.2/                 (v1.2 emission kickoff)
 *   - ../deploy-kickoff-quipu-canon-v1.2/          (v1.2 deploy kickoff)
 *
 * Those scripts shipped Quipu Canon v1.2 emission code and then deployed
 * it on 2026-05-03. THIS script rolls out the 2026-05-10 refresh of the
 * canonical-agents handover (`.cursor/rules/handover-canonical-agents-live-from-rope-2026-05-05.mdc`)
 * across the same 10 ecosystem workspaces.
 *
 * SCHEMA v2 (refactored 2026-05-10 after the tanastok deploy lesson):
 *   - workspaces.json now ships per-project audited deploy_command,
 *     production_check, ssh_keys_required, min_disk_gib, and signal
 *     splits (emission_signals / deploy_signals).
 *   - The marker is now phase-structured (## Phase: emission /
 *     ## Phase: deploy). Mode-aware idempotency: --deploy mode reads
 *     deploy_signals only; --emission mode reads emission_signals only.
 *   - Pre-flight gate: disk free / SSH keys present / human_setup_required
 *     surfaced as warnings before any agent spawns.
 *   - --direct-deploy mode bypasses the agent layer and runs the audited
 *     deploy_command directly (the manual recovery path we used for
 *     tanastok on 2026-05-10 — now first-class).
 *   - --force / --force-deploy flags bypass the marker check.
 *
 * Modes (mutually exclusive):
 *   --dry-run            Print pre-flight + plan-per-workspace, don't launch agents.
 *   --plan-only          Launch agents; STEP 0 + STEP 1 only.
 *   --emission           Launch agents; STEP 0 + STEP 1-3 (commit + push, NO deploy).
 *   --deploy             Launch agents; STEP 0 + STEP 4-6 (deploy + verify + marker).
 *   --full               Launch agents; all 7 steps end-to-end.
 *   --direct-deploy      Bypass agents. Run each project's audited deploy_command
 *                        directly with built-in pre-flight + post-deploy verify.
 *                        Useful when the agent's STEP 0 falsely short-circuits or
 *                        when re-deploying after a build crash (the tanastok recovery
 *                        path — formalised so we don't have to invent it again).
 *   --status             Walk logs/, summarise past runs. Exits.
 *
 * Force flags (override marker-based skipping):
 *   --force              Bypass implemented-marker check entirely (any mode).
 *   --force-deploy       Bypass marker check in deploy / direct-deploy / full modes
 *                        only. Useful when you want to re-run deploy because
 *                        you know the marker is stale but you don't want to
 *                        nuke the whole record.
 *
 * Other knobs:
 *   --workspace=<name>   Restrict to one workspace.
 *   --concurrency=<N>    Default 2. Set to 1 for strict serial.
 *   --model=<id>         Cursor model id. Default composer-2.
 *   --skip-preflight     Skip disk / ssh-key / human-setup checks. Use only
 *                        if pre-flight has a known false positive on this run.
 *
 * Idempotency markers (per workspace):
 *   .cursor/handover-canonical-agents-2026-05-10-implemented.marker
 *   .cursor/handover-canonical-agents-2026-05-10-blocked.md
 *   .cursor/handover-canonical-agents-2026-05-10-plan.md
 *
 * Marker is now structured:
 *   ## Phase: emission     (code shipped to git)
 *   ## Phase: deploy       (production verifiably running new code)
 *   ## Result              (one-line summary)
 *
 * Old free-form markers from previous rollouts are read as
 * "## Phase: emission" only; --deploy mode against an emission-only
 * marker re-fires the deploy phase.
 *
 * Usage:
 *   export CURSOR_API_KEY=cursor_...
 *   npm install
 *   npm run dry-run                    # safe preview, no API key needed
 *   npm run plan-only                  # launches agents, plan files only
 *   npm run full                       # full end-to-end across all 10 projects
 *   npm run direct-deploy -- --workspace=tanastok    # bypass agent, run audited script
 *   npm run deploy -- --workspace=tanastok --force-deploy  # agent re-deploy, ignore stale marker
 */

import {
  Agent,
  CursorAgentError,
  type RunResult,
} from "@cursor/sdk";
import { execSync, spawnSync } from "node:child_process";
import {
  appendFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  statfsSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const LOG_DIR = join(__dirname, "logs");
const RUN_LOG = join(
  LOG_DIR,
  `run-${new Date().toISOString().replace(/[:.]/g, "-")}.jsonl`,
);

const PRIMER_TEMPLATE_PATH = join(__dirname, "primer.template.md");
const WORKSPACES_JSON_PATH = join(__dirname, "workspaces.json");

// -----------------------------------------------------------------------------
// Types
// -----------------------------------------------------------------------------

interface ProductionCheck {
  url?: string | null;
  expected_status?: number;
  follow_redirects?: boolean;
  ssh_state_check?: string;
  eas_check?: string;
  rpc_check?: string;
}

interface WorkspaceConfig {
  name: string;
  cwd: string;
  sensitivity: "normal" | "health" | "securities" | "real-money";
  actions: string[];
  /** v2 signal split. */
  emission_signals?: string[];
  deploy_signals?: string[];
  /** v1 fallback (treated as union of emission+deploy). */
  already_done_signals?: string[];
  /** v2: audited deploy command. null = no scripted deploy. */
  deploy_command: string | null;
  deploy_command_notes?: string;
  /** v2: live production verification. */
  production_check?: ProductionCheck;
  min_disk_gib?: number;
  ssh_keys_required?: string[];
  /** v2: workspace needs one-off operator setup before deploy can run. */
  human_setup_required?: boolean;
  human_setup_checklist?: string[];
  primer_extra: string;
}

interface WorkspacesFile {
  schema_version?: number;
  handover_reference: string;
  implemented_marker: string;
  blocked_report: string;
  plan_file: string;
  default_min_disk_gib?: number;
  workspaces: WorkspaceConfig[];
}

type Mode =
  | "dry-run"
  | "plan-only"
  | "emission"
  | "deploy"
  | "full"
  | "direct-deploy"
  | "status";

interface RunArgs {
  mode: Mode;
  workspaceFilter?: string;
  concurrency: number;
  model: string;
  force: boolean;
  forceDeploy: boolean;
  skipPreflight: boolean;
}

// -----------------------------------------------------------------------------
// Args
// -----------------------------------------------------------------------------

function parseArgs(argv: string[]): RunArgs {
  const args: Partial<RunArgs> = {
    concurrency: 2,
    model: "composer-2",
    force: false,
    forceDeploy: false,
    skipPreflight: false,
  };
  let modeSet = false;
  const setMode = (m: Mode): void => {
    if (modeSet)
      throw new Error(
        `Multiple modes specified; pick one: --dry-run, --plan-only, --emission, --deploy, --full, --direct-deploy, --status`,
      );
    args.mode = m;
    modeSet = true;
  };
  for (const a of argv.slice(2)) {
    if (a === "--dry-run") setMode("dry-run");
    else if (a === "--plan-only") setMode("plan-only");
    else if (a === "--emission") setMode("emission");
    else if (a === "--deploy") setMode("deploy");
    else if (a === "--full") setMode("full");
    else if (a === "--direct-deploy") setMode("direct-deploy");
    else if (a === "--status") setMode("status");
    else if (a === "--force") args.force = true;
    else if (a === "--force-deploy") args.forceDeploy = true;
    else if (a === "--skip-preflight") args.skipPreflight = true;
    else if (a.startsWith("--workspace="))
      args.workspaceFilter = a.slice("--workspace=".length);
    else if (a.startsWith("--concurrency="))
      args.concurrency = parseInt(a.slice("--concurrency=".length), 10);
    else if (a.startsWith("--model=")) args.model = a.slice("--model=".length);
    else throw new Error(`Unknown flag: ${a}`);
  }
  if (!modeSet) {
    throw new Error(
      `No mode specified. Pick one of: --dry-run, --plan-only, --emission, --deploy, --full, --direct-deploy, --status`,
    );
  }
  return args as RunArgs;
}

function loadWorkspaces(): WorkspacesFile {
  if (!existsSync(WORKSPACES_JSON_PATH))
    throw new Error(`workspaces.json not found at ${WORKSPACES_JSON_PATH}`);
  const raw = readFileSync(WORKSPACES_JSON_PATH, "utf8");
  const parsed = JSON.parse(raw) as WorkspacesFile;
  if (!Array.isArray(parsed.workspaces) || parsed.workspaces.length === 0)
    throw new Error(`workspaces.json has no workspaces`);
  return parsed;
}

function loadPrimerTemplate(): string {
  if (!existsSync(PRIMER_TEMPLATE_PATH))
    throw new Error(`primer.template.md not found at ${PRIMER_TEMPLATE_PATH}`);
  return readFileSync(PRIMER_TEMPLATE_PATH, "utf8");
}

// -----------------------------------------------------------------------------
// Pre-flight (disk, SSH keys, human_setup, git remote)
// -----------------------------------------------------------------------------

function expandHome(p: string): string {
  if (p.startsWith("~/")) return join(homedir(), p.slice(2));
  if (p === "~") return homedir();
  return p;
}

function freeDiskGiB(path: string): number {
  try {
    const s = statfsSync(path);
    // bavail is reserved-aware; bsize × bavail = bytes available to non-root.
    return Number((s.bavail * BigInt(s.bsize)) / BigInt(1024 ** 3));
  } catch {
    return Number.POSITIVE_INFINITY;
  }
}

function gitRemoteCount(cwd: string): number {
  try {
    const out = execSync("git remote", { cwd, encoding: "utf8" });
    return out.trim().split("\n").filter(Boolean).length;
  } catch {
    return 0;
  }
}

interface PreflightResult {
  ok: boolean;
  warnings: string[];
  blockers: string[];
}

function runPreflight(
  ws: WorkspaceConfig,
  cfg: WorkspacesFile,
  mode: Mode,
): PreflightResult {
  const warnings: string[] = [];
  const blockers: string[] = [];

  // Disk: relevant for any mode that may build (emission, deploy, full,
  // direct-deploy). Skip for plan-only / dry-run / status.
  const buildModes: Mode[] = ["emission", "deploy", "full", "direct-deploy"];
  if (buildModes.includes(mode)) {
    const minDiskGiB =
      ws.min_disk_gib ?? cfg.default_min_disk_gib ?? 5;
    const free = freeDiskGiB(ws.cwd);
    if (free < minDiskGiB) {
      blockers.push(
        `disk free is ${free} GiB at ${ws.cwd}; project requires ≥ ${minDiskGiB} GiB`,
      );
    } else if (free < minDiskGiB * 2) {
      warnings.push(
        `disk free is ${free} GiB (project min ${minDiskGiB} GiB) — low margin`,
      );
    }

    // SSH keys for projects that need them at deploy time
    if (mode === "deploy" || mode === "full" || mode === "direct-deploy") {
      for (const key of ws.ssh_keys_required ?? []) {
        const expanded = expandHome(key);
        if (!existsSync(expanded)) {
          blockers.push(`SSH key required by deploy_command not found: ${key}`);
        }
      }
    }
  }

  // Human-setup gate
  if (
    ws.human_setup_required &&
    (mode === "deploy" || mode === "direct-deploy" || mode === "full")
  ) {
    warnings.push(
      `human_setup_required=true — deploy will be deferred. Operator must complete human_setup_checklist before next run.`,
    );
  }

  // Git-remote check: not a blocker, just a warning when emission mode pushes
  if (
    (mode === "emission" || mode === "full") &&
    existsSync(join(ws.cwd, ".git"))
  ) {
    if (gitRemoteCount(ws.cwd) === 0) {
      warnings.push(
        "no git remote configured — STEP 3 will commit locally only, no push",
      );
    }
  }

  // deploy_command sanity check
  if (
    (mode === "deploy" || mode === "full" || mode === "direct-deploy") &&
    ws.deploy_command == null
  ) {
    warnings.push(
      `deploy_command is null — deploy phase will write a deferred-deploy blocker (this is by design for ${ws.name})`,
    );
  }

  return {
    ok: blockers.length === 0,
    warnings,
    blockers,
  };
}

// -----------------------------------------------------------------------------
// Marker reader (structured v2 + free-form v1 fallback)
// -----------------------------------------------------------------------------

interface MarkerSummary {
  exists: boolean;
  hasEmissionPhase: boolean;
  hasDeployPhase: boolean;
  raw: string;
}

function readMarker(markerPath: string): MarkerSummary {
  if (!existsSync(markerPath))
    return { exists: false, hasEmissionPhase: false, hasDeployPhase: false, raw: "" };
  const raw = readFileSync(markerPath, "utf8");
  // Accept both schema-v2 canonical (`## Phase: deploy`) and the variant
  // (`## Deploy phase`) that existed before the v2 refactor was codified.
  const hasEmissionPhase =
    /^##\s*(Phase:\s*emission|Emission\s+phase)\b/im.test(raw);
  const hasDeployPhase =
    /^##\s*(Phase:\s*deploy|Deploy\s+phase)\b/im.test(raw);
  if (!hasEmissionPhase && !hasDeployPhase) {
    // v1 free-form marker: treat as emission-only.
    return { exists: true, hasEmissionPhase: true, hasDeployPhase: false, raw };
  }
  return { exists: true, hasEmissionPhase, hasDeployPhase, raw };
}

function shouldSkip(
  mode: Mode,
  marker: MarkerSummary,
  args: RunArgs,
): { skip: boolean; reason: string } {
  if (!marker.exists) return { skip: false, reason: "" };
  if (args.force) return { skip: false, reason: "" };
  if (
    args.forceDeploy &&
    (mode === "deploy" || mode === "direct-deploy" || mode === "full")
  )
    return { skip: false, reason: "" };

  if (mode === "emission") {
    if (marker.hasEmissionPhase)
      return {
        skip: true,
        reason: "emission phase already recorded in marker",
      };
  } else if (mode === "deploy" || mode === "direct-deploy") {
    if (marker.hasDeployPhase)
      return {
        skip: true,
        reason: "deploy phase already recorded in marker",
      };
  } else if (mode === "full") {
    if (marker.hasEmissionPhase && marker.hasDeployPhase)
      return {
        skip: true,
        reason: "both phases recorded in marker",
      };
  }
  return { skip: false, reason: "" };
}

// -----------------------------------------------------------------------------
// Primer builder
// -----------------------------------------------------------------------------

function buildPrimer(
  ws: WorkspaceConfig,
  cfg: WorkspacesFile,
  template: string,
  mode: Mode,
): string {
  const actionsBlock = ws.actions
    .map((a, i) => `   ${i + 1}. ${a}`)
    .join("\n");

  // v2 signal split with v1 fallback.
  const emissionSignals =
    ws.emission_signals ?? ws.already_done_signals ?? [];
  const deploySignals = ws.deploy_signals ?? ws.already_done_signals ?? [];
  const emissionBlock = emissionSignals.map((s) => `   - ${s}`).join("\n");
  const deployBlock = deploySignals.map((s) => `   - ${s}`).join("\n");

  // The agent doesn't see direct-deploy (we never spawn an agent in that
  // mode); collapse to deploy for display.
  let modeForPrimer: Mode = mode;
  if (mode === "dry-run") modeForPrimer = "plan-only";
  if (mode === "direct-deploy") modeForPrimer = "deploy";

  const setupChecklist =
    ws.human_setup_checklist?.length
      ? ws.human_setup_checklist.map((s, i) => `   ${i + 1}. ${s}`).join("\n")
      : "   (none)";

  const prodCheckJson = JSON.stringify(ws.production_check ?? null, null, 2);
  const deployCmd = ws.deploy_command ?? "null";
  const deployNotes =
    ws.deploy_command_notes ?? "(no notes recorded — see workspaces.json)";

  return template
    .replaceAll("{{NAME}}", ws.name)
    .replaceAll("{{CWD}}", ws.cwd)
    .replaceAll("{{HANDOVER_PATH}}", cfg.handover_reference)
    .replaceAll("{{IMPLEMENTED_MARKER}}", cfg.implemented_marker)
    .replaceAll("{{BLOCKED_REPORT}}", cfg.blocked_report)
    .replaceAll("{{PLAN_FILE}}", cfg.plan_file)
    .replaceAll("{{MODE}}", modeForPrimer)
    .replaceAll("{{SENSITIVITY}}", ws.sensitivity)
    .replaceAll("{{ACTIONS}}", actionsBlock)
    .replaceAll("{{EMISSION_SIGNALS}}", emissionBlock || "   (none recorded)")
    .replaceAll("{{DEPLOY_SIGNALS}}", deployBlock || "   (none recorded)")
    .replaceAll("{{DEPLOY_COMMAND}}", deployCmd)
    .replaceAll("{{DEPLOY_COMMAND_NOTES}}", deployNotes)
    .replaceAll("{{PRODUCTION_CHECK}}", prodCheckJson)
    .replaceAll(
      "{{HUMAN_SETUP_REQUIRED}}",
      String(Boolean(ws.human_setup_required)),
    )
    .replaceAll("{{HUMAN_SETUP_CHECKLIST}}", setupChecklist)
    .replaceAll("{{PRIMER_EXTRA}}", ws.primer_extra);
}

// -----------------------------------------------------------------------------
// Run records
// -----------------------------------------------------------------------------

interface RunRecord {
  workspace: string;
  mode: Mode;
  status:
    | "skipped-missing-cwd"
    | "skipped-already-implemented"
    | "skipped-preflight-blocker"
    | "skipped-dry-run"
    | "skipped-no-deploy-command"
    | "finished"
    | "deploy-success"
    | "deploy-failed"
    | "verify-failed"
    | "error"
    | "cancelled"
    | "startup-failed";
  runId?: string;
  durationMs?: number;
  error?: string;
  warnings?: string[];
  startedAt: string;
  endedAt?: string;
}

function logRun(record: RunRecord): void {
  if (!existsSync(LOG_DIR)) mkdirSync(LOG_DIR, { recursive: true });
  appendFileSync(RUN_LOG, JSON.stringify(record) + "\n");
}

// -----------------------------------------------------------------------------
// Direct deploy (no agent — runs the audited script with built-in verify)
// -----------------------------------------------------------------------------

function curlStatus(
  url: string,
  follow = true,
): { code: number; ms: number; ok: boolean } {
  const args = [
    "-sS",
    "-o",
    "/dev/null",
    "-w",
    "%{http_code}|%{time_total}",
    follow ? "-L" : "",
    "--max-time",
    "20",
    url,
  ].filter(Boolean);
  const t0 = Date.now();
  const r = spawnSync("curl", args, { encoding: "utf8" });
  const ms = Date.now() - t0;
  if (r.status !== 0) return { code: 0, ms, ok: false };
  const [code, t] = r.stdout.trim().split("|");
  return { code: Number(code), ms: Math.round(Number(t) * 1000) || ms, ok: true };
}

function verifyProduction(ws: WorkspaceConfig): {
  ok: boolean;
  evidence: string[];
} {
  const evidence: string[] = [];
  const pc = ws.production_check;
  if (!pc) {
    evidence.push("no production_check defined — skipping URL verify");
    return { ok: true, evidence };
  }
  if (pc.url) {
    const expected = pc.expected_status ?? 200;
    const r = curlStatus(pc.url, pc.follow_redirects ?? true);
    const ok = r.code === expected;
    evidence.push(
      `${pc.url} → HTTP ${r.code} in ${r.ms}ms (expected ${expected})`,
    );
    if (!ok) return { ok: false, evidence };
  }
  if (pc.ssh_state_check) {
    try {
      const out = execSync(pc.ssh_state_check, {
        encoding: "utf8",
        timeout: 30_000,
      });
      evidence.push(
        `ssh_state_check: ${pc.ssh_state_check.slice(0, 80)} → ${out.trim().slice(0, 200)}`,
      );
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      evidence.push(`ssh_state_check FAILED: ${msg.slice(0, 200)}`);
      return { ok: false, evidence };
    }
  }
  // eas_check / rpc_check left to STEP 5 of the agent flow; direct-deploy
  // performs only the cheap http+ssh checks.
  return { ok: true, evidence };
}

function writeStructuredMarker(
  markerPath: string,
  ws: WorkspaceConfig,
  evidence: string[],
  durationMs: number,
): void {
  const ts = new Date().toISOString();
  const lines = [
    ts,
    "",
    "## Phase: deploy",
    `- Deploy command: ${ws.deploy_command}`,
    `- Mode: --direct-deploy (no agent in the loop)`,
    `- Started: ${new Date(Date.now() - durationMs).toISOString()}`,
    `- Finished: ${ts}`,
    `- Production verification:`,
    ...evidence.map((e) => `  - ${e}`),
    "",
    "## Result",
    `- Deploy: SHIPPED via ${ws.deploy_command}`,
    `- Verification: passed (see Phase: deploy → Production verification above)`,
    "",
  ];
  appendFileSync(markerPath, lines.join("\n"));
}

async function runDirectDeploy(
  ws: WorkspaceConfig,
  cfg: WorkspacesFile,
  args: RunArgs,
): Promise<RunRecord> {
  const startedAt = new Date().toISOString();
  const t0 = Date.now();
  const markerPath = join(ws.cwd, cfg.implemented_marker);

  if (ws.deploy_command == null) {
    console.log(
      `[${ws.name}] DEPLOY-SKIP deploy_command is null — see deploy_command_notes`,
    );
    const r: RunRecord = {
      workspace: ws.name,
      mode: args.mode,
      status: "skipped-no-deploy-command",
      startedAt,
      endedAt: new Date().toISOString(),
      error: ws.deploy_command_notes ?? "no notes",
    };
    logRun(r);
    return r;
  }
  if (ws.human_setup_required) {
    console.log(
      `[${ws.name}] DEPLOY-DEFERRED human_setup_required — see human_setup_checklist`,
    );
    const r: RunRecord = {
      workspace: ws.name,
      mode: args.mode,
      status: "skipped-no-deploy-command",
      startedAt,
      endedAt: new Date().toISOString(),
      error: "human_setup_required=true",
    };
    logRun(r);
    return r;
  }

  console.log(
    `[${ws.name}] DIRECT-DEPLOY cwd=${ws.cwd} cmd=${ws.deploy_command}`,
  );
  const t = Date.now();
  const r = spawnSync("bash", ["-c", ws.deploy_command], {
    cwd: ws.cwd,
    stdio: "inherit",
    env: process.env,
  });
  const deployMs = Date.now() - t;

  if (r.status !== 0) {
    console.error(
      `[${ws.name}] DEPLOY-FAIL exit=${r.status} duration=${(deployMs / 1000).toFixed(1)}s`,
    );
    const rec: RunRecord = {
      workspace: ws.name,
      mode: args.mode,
      status: "deploy-failed",
      durationMs: Date.now() - t0,
      startedAt,
      endedAt: new Date().toISOString(),
      error: `deploy_command exited ${r.status}`,
    };
    logRun(rec);
    return rec;
  }

  const verify = verifyProduction(ws);
  if (!verify.ok) {
    console.error(`[${ws.name}] VERIFY-FAIL`);
    for (const e of verify.evidence) console.error(`  ${e}`);
    const rec: RunRecord = {
      workspace: ws.name,
      mode: args.mode,
      status: "verify-failed",
      durationMs: Date.now() - t0,
      startedAt,
      endedAt: new Date().toISOString(),
      error: `production verification failed: ${verify.evidence.join("; ")}`,
    };
    logRun(rec);
    return rec;
  }

  writeStructuredMarker(markerPath, ws, verify.evidence, deployMs);
  console.log(
    `[${ws.name}] DEPLOY-SUCCESS duration=${(deployMs / 1000).toFixed(1)}s`,
  );
  for (const e of verify.evidence) console.log(`  ${e}`);
  const rec: RunRecord = {
    workspace: ws.name,
    mode: args.mode,
    status: "deploy-success",
    durationMs: Date.now() - t0,
    startedAt,
    endedAt: new Date().toISOString(),
  };
  logRun(rec);
  return rec;
}

// -----------------------------------------------------------------------------
// Agent-driven run
// -----------------------------------------------------------------------------

async function runOne(
  ws: WorkspaceConfig,
  cfg: WorkspacesFile,
  template: string,
  args: RunArgs,
): Promise<RunRecord> {
  const startedAt = new Date().toISOString();

  if (!existsSync(ws.cwd)) {
    const record: RunRecord = {
      workspace: ws.name,
      mode: args.mode,
      status: "skipped-missing-cwd",
      startedAt,
      endedAt: new Date().toISOString(),
      error: `workspace not found at ${ws.cwd}`,
    };
    logRun(record);
    console.error(`[${ws.name}] STARTUP-FAIL workspace missing at ${ws.cwd}`);
    return record;
  }

  // Pre-flight gate (disk, ssh keys, human_setup, git remote)
  const pf = args.skipPreflight
    ? { ok: true, warnings: [], blockers: [] }
    : runPreflight(ws, cfg, args.mode);
  for (const w of pf.warnings)
    console.warn(`[${ws.name}] PREFLIGHT-WARN ${w}`);
  if (!pf.ok) {
    for (const b of pf.blockers)
      console.error(`[${ws.name}] PREFLIGHT-BLOCK ${b}`);
    const record: RunRecord = {
      workspace: ws.name,
      mode: args.mode,
      status: "skipped-preflight-blocker",
      startedAt,
      endedAt: new Date().toISOString(),
      warnings: pf.warnings,
      error: pf.blockers.join("; "),
    };
    logRun(record);
    return record;
  }

  // Direct-deploy mode bypasses the agent layer entirely.
  if (args.mode === "direct-deploy") {
    // Check structured marker for prior deploy phase
    const marker = readMarker(join(ws.cwd, cfg.implemented_marker));
    const skip = shouldSkip(args.mode, marker, args);
    if (skip.skip) {
      console.log(`[${ws.name}] SKIP (${skip.reason}) — pass --force-deploy to override`);
      const record: RunRecord = {
        workspace: ws.name,
        mode: args.mode,
        status: "skipped-already-implemented",
        startedAt,
        endedAt: new Date().toISOString(),
        error: skip.reason,
      };
      logRun(record);
      return record;
    }
    return runDirectDeploy(ws, cfg, args);
  }

  // Idempotency at dispatcher level for agent modes
  const marker = readMarker(join(ws.cwd, cfg.implemented_marker));
  const skip = shouldSkip(args.mode, marker, args);
  if (skip.skip) {
    const record: RunRecord = {
      workspace: ws.name,
      mode: args.mode,
      status: "skipped-already-implemented",
      startedAt,
      endedAt: new Date().toISOString(),
      error: `${skip.reason}; pass --force or --force-deploy to override`,
    };
    logRun(record);
    console.log(
      `[${ws.name}] SKIP (${skip.reason}) — --force or --force-deploy to override`,
    );
    return record;
  }

  const primer = buildPrimer(ws, cfg, template, args.mode);

  if (args.mode === "dry-run") {
    console.log(`[${ws.name}] DRY-RUN cwd=${ws.cwd}`);
    console.log(`  sensitivity:    ${ws.sensitivity}`);
    console.log(`  actions:        ${ws.actions.length}`);
    console.log(`  deploy_command: ${ws.deploy_command ?? "null (deferred)"}`);
    console.log(
      `  human_setup:    ${Boolean(ws.human_setup_required)}${ws.human_setup_required ? " (deploy will defer)" : ""}`,
    );
    console.log(`  primer length:  ${primer.length} chars`);
    console.log(
      `  marker:         ${
        marker.exists
          ? `present (emission=${marker.hasEmissionPhase}, deploy=${marker.hasDeployPhase})`
          : "no"
      }`,
    );
    return {
      workspace: ws.name,
      mode: args.mode,
      status: "skipped-dry-run",
      startedAt,
      endedAt: new Date().toISOString(),
    };
  }

  console.log(
    `[${ws.name}] LAUNCH cwd=${ws.cwd} mode=${args.mode} sensitivity=${ws.sensitivity}`,
  );
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
      mode: args.mode,
      status,
      runId: result.id,
      durationMs,
      startedAt,
      endedAt,
      warnings: pf.warnings,
      error:
        status === "finished"
          ? undefined
          : `run terminated with status=${result.status}`,
    };
    logRun(record);
    console.log(
      `[${ws.name}] ${status.toUpperCase()} runId=${result.id} duration=${(durationMs / 1000).toFixed(1)}s`,
    );
    return record;
  } catch (err) {
    const endedAt = new Date().toISOString();
    const durationMs = Date.now() - t0;
    const isCursorErr = err instanceof CursorAgentError;
    const errMsg = err instanceof Error ? err.message : String(err);
    const record: RunRecord = {
      workspace: ws.name,
      mode: args.mode,
      status: "startup-failed",
      durationMs,
      startedAt,
      endedAt,
      warnings: pf.warnings,
      error: `${isCursorErr ? "CursorAgentError" : "Error"}: ${errMsg}`,
    };
    logRun(record);
    console.error(`[${ws.name}] STARTUP-FAIL ${errMsg}`);
    return record;
  }
}

// -----------------------------------------------------------------------------
// Concurrency
// -----------------------------------------------------------------------------

async function pmap<T, R>(
  items: readonly T[],
  concurrency: number,
  worker: (item: T) => Promise<R>,
): Promise<R[]> {
  const out = new Array<R>(items.length);
  let next = 0;
  const lanes = Math.max(1, Math.min(concurrency, items.length));
  const runners = Array.from({ length: lanes }, async () => {
    while (true) {
      const i = next++;
      if (i >= items.length) return;
      out[i] = await worker(items[i]!);
    }
  });
  await Promise.all(runners);
  return out;
}

// -----------------------------------------------------------------------------
// Status / past-runs summary
// -----------------------------------------------------------------------------

function summarizePastRuns(): void {
  if (!existsSync(LOG_DIR)) {
    console.log("No previous runs (logs/ does not exist).");
    return;
  }
  const fs = require("node:fs") as typeof import("node:fs");
  const files = fs
    .readdirSync(LOG_DIR)
    .filter((f) => f.endsWith(".jsonl"))
    .sort();
  if (files.length === 0) {
    console.log("No previous runs (logs/ is empty).");
    return;
  }
  for (const f of files) {
    const lines = readFileSync(join(LOG_DIR, f), "utf8")
      .trim()
      .split("\n")
      .filter(Boolean);
    console.log(`\n=== ${f} (${lines.length} runs) ===`);
    for (const line of lines) {
      const r = JSON.parse(line) as RunRecord;
      console.log(
        `  ${r.workspace.padEnd(16)} ${r.mode.padEnd(13)} ${r.status.padEnd(28)} ` +
          (r.durationMs != null
            ? `${(r.durationMs / 1000).toFixed(1)}s `
            : "       ") +
          (r.runId ? `run=${r.runId}` : (r.error ?? "")),
      );
    }
  }
}

// -----------------------------------------------------------------------------
// main()
// -----------------------------------------------------------------------------

async function main(): Promise<void> {
  const args = parseArgs(process.argv);

  // Eagerly create LOG_DIR before anything else can write to it.
  if (!existsSync(LOG_DIR)) mkdirSync(LOG_DIR, { recursive: true });

  if (args.mode === "status") {
    summarizePastRuns();
    return;
  }

  // direct-deploy doesn't need CURSOR_API_KEY (no agent in the loop)
  const needsApiKey =
    args.mode !== "dry-run" && args.mode !== "direct-deploy";
  if (needsApiKey && !process.env.CURSOR_API_KEY) {
    console.error(
      "CURSOR_API_KEY is not set. Get one from https://cursor.com/dashboard/cloud-agents",
    );
    process.exit(1);
  }

  const cfg = loadWorkspaces();
  const template = loadPrimerTemplate();

  const targets = args.workspaceFilter
    ? cfg.workspaces.filter((w) => w.name === args.workspaceFilter)
    : cfg.workspaces;

  if (targets.length === 0) {
    console.error(
      `No workspaces match --workspace=${args.workspaceFilter ?? ""}.`,
    );
    console.error("Known: " + cfg.workspaces.map((w) => w.name).join(", "));
    process.exit(1);
  }

  console.log(
    `Canonical-Agents Handover Rollout (2026-05-10, schema v${cfg.schema_version ?? 2}) — ${targets.length} workspace(s), mode=${args.mode}, concurrency=${args.concurrency}, model=${args.model}${args.force ? ", FORCE" : ""}${args.forceDeploy ? ", FORCE-DEPLOY" : ""}${args.skipPreflight ? ", NO-PREFLIGHT" : ""}`,
  );
  console.log(`Run log: ${RUN_LOG}\n`);

  // Direct-deploy is sequential by default — touching shared production
  // infra at concurrency 2+ from the same laptop is just asking for SSH /
  // rsync contention.
  const concurrency =
    args.mode === "direct-deploy" ? 1 : args.concurrency;

  const results = await pmap(targets, concurrency, (ws) =>
    runOne(ws, cfg, template, args),
  );

  const counts = {
    finished: results.filter((r) => r.status === "finished").length,
    deploySuccess: results.filter((r) => r.status === "deploy-success").length,
    deployFailed: results.filter((r) => r.status === "deploy-failed").length,
    verifyFailed: results.filter((r) => r.status === "verify-failed").length,
    skippedAlreadyImplemented: results.filter(
      (r) => r.status === "skipped-already-implemented",
    ).length,
    skippedMissingCwd: results.filter(
      (r) => r.status === "skipped-missing-cwd",
    ).length,
    skippedPreflightBlocker: results.filter(
      (r) => r.status === "skipped-preflight-blocker",
    ).length,
    skippedDryRun: results.filter((r) => r.status === "skipped-dry-run")
      .length,
    skippedNoDeployCommand: results.filter(
      (r) => r.status === "skipped-no-deploy-command",
    ).length,
    error: results.filter((r) => r.status === "error").length,
    cancelled: results.filter((r) => r.status === "cancelled").length,
    startupFailed: results.filter((r) => r.status === "startup-failed").length,
  };

  console.log("\n=== summary ===");
  console.log(`  finished:                       ${counts.finished}`);
  console.log(`  deploy-success:                 ${counts.deploySuccess}`);
  console.log(`  deploy-failed:                  ${counts.deployFailed}`);
  console.log(`  verify-failed:                  ${counts.verifyFailed}`);
  console.log(
    `  skipped-already-implemented:    ${counts.skippedAlreadyImplemented}`,
  );
  console.log(`  skipped-missing-cwd:            ${counts.skippedMissingCwd}`);
  console.log(
    `  skipped-preflight-blocker:      ${counts.skippedPreflightBlocker}`,
  );
  console.log(`  skipped-dry-run:                ${counts.skippedDryRun}`);
  console.log(
    `  skipped-no-deploy-command:      ${counts.skippedNoDeployCommand}`,
  );
  console.log(`  cancelled:                      ${counts.cancelled}`);
  console.log(`  error:                          ${counts.error}`);
  console.log(`  startup-failed:                 ${counts.startupFailed}`);

  if (counts.startupFailed > 0) process.exit(1);
  if (counts.deployFailed > 0 || counts.verifyFailed > 0) process.exit(2);
  if (counts.error > 0) process.exit(2);
  process.exit(0);
}

main().catch((err) => {
  console.error("Unhandled error in handover rollout kickoff:", err);
  process.exit(1);
});
