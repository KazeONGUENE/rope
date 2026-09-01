#!/usr/bin/env node
/**
 * CERBER R15 - docs-vs-production drift monitor CLI.
 *
 * Runs the check in docs-drift.mjs against the public docs, faucet and RPC.
 * If any FAIL finding is present, pages contact@onguene.com with the full
 * report body. WARN findings are journalled but do not page (they still
 * appear in the daily summary).
 *
 * Usage:
 *   cerber-docs-drift [--docs URL] [--faucet URL] [--rpc URL] [--report stdout|email|both]
 *
 * Exit code:
 *   0  no FAIL findings
 *   1  at least one FAIL finding
 *   2  bad arguments
 */

import { hostname } from "node:os";
import { runDriftCheck, renderDriftReport } from "../lib/docs-drift.mjs";
import { pageEmail, alertEmailConfigured } from "../lib/page-email.mjs";
import { appendFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";

function parseArgs(argv) {
  const out = {
    docs: "https://datachain.network/docs",
    // 2026-08-30: point at the canonical faucet host directly. The old
    // default `https://datachain.network/faucet` only worked because
    // nginx 301'd to this subdomain, and any regression that broke that
    // redirect would silently mask a real faucet failure.
    faucet: "https://faucet.datachain.network/",
    rpc: "https://erpc.datachain.network",
    // Testnet chainId check is optional; kept implicit via the default
    // in docs-drift.mjs (https://testnet.erpc.datachain.network).
    report: "both",
  };
  for (let i = 2; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = argv[i + 1];
    switch (arg) {
      case "--docs":
        out.docs = next;
        i += 1;
        break;
      case "--faucet":
        out.faucet = next;
        i += 1;
        break;
      case "--rpc":
        out.rpc = next;
        i += 1;
        break;
      case "--report":
        out.report = next;
        i += 1;
        break;
      case "-h":
      case "--help":
        process.stdout.write(
          "cerber-docs-drift - CERBER R15 docs-vs-production drift check\n" +
            "  --docs URL      (default https://datachain.network/docs)\n" +
            "  --faucet URL    (default https://faucet.datachain.network/)\n" +
            "  --rpc URL       (default https://erpc.datachain.network)\n" +
            "  --report stdout|email|both  (default both)\n"
        );
        process.exit(0);
      // eslint-disable-next-line no-fallthrough
      default:
        process.stderr.write(`unknown flag: ${arg}\n`);
        process.exit(2);
    }
  }
  return out;
}

function persist(check) {
  const dir = process.env.CERBER_R15_DIR || "/var/lib/datachain-rope/cerber/docs-drift";
  mkdirSync(dir, { recursive: true, mode: 0o750 });
  const day = new Date().toISOString().slice(0, 10);
  const body = JSON.stringify({ ts: new Date().toISOString(), host: hostname(), ...check }) + "\n";
  appendFileSync(join(dir, `drift-${day}.ndjson`), body, { mode: 0o640 });
}

async function main() {
  const args = parseArgs(process.argv);
  const check = await runDriftCheck({ docsUrl: args.docs, faucetUrl: args.faucet, rpcUrl: args.rpc });
  const report = renderDriftReport(check);
  persist(check);

  if (args.report === "stdout" || args.report === "both") {
    process.stdout.write(report + "\n");
  }
  if ((args.report === "email" || args.report === "both") && (check.summary.fail ?? 0) > 0) {
    if (!alertEmailConfigured()) {
      process.stderr.write("[cerber-docs-drift] email not configured; skipping page\n");
    } else {
      const dayKey = new Date().toISOString().slice(0, 10);
      const failIds = check.findings.filter((f) => f.status === "fail").map((f) => f.id).join(", ");
      const res = await pageEmail({
        subject: `docs/faucet/rpc drift - ${check.summary.fail} fail (${failIds})`,
        body: report,
        dedupeKey: `r15-fail:${dayKey}:${failIds}`,
        threatLevel: 4,
        rule: "r15-docs-drift",
      });
      process.stderr.write(`[cerber-docs-drift] page: ${res.sent ? "sent" : `not-sent (${res.reason})`}\n`);
    }
  }
  process.exit((check.summary.fail ?? 0) > 0 ? 1 : 0);
}

main().catch((e) => {
  process.stderr.write(`[cerber-docs-drift] fatal: ${e?.stack || e}\n`);
  process.exit(1);
});
