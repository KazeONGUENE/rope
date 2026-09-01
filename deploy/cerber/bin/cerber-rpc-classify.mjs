#!/usr/bin/env node
/**
 * CERBER R14 - RPC activity classifier CLI.
 *
 * Usage:
 *   cerber-rpc-classify --source docker --container rope-nginx --since 24h
 *   cerber-rpc-classify --source stdin < /path/to/nginx.access.log
 *   cerber-rpc-classify --source file --path /var/log/nginx/access.log --since 24h
 *
 * Flags:
 *   --source        docker | stdin | file           (default: docker)
 *   --container     rope-nginx                      (default: rope-nginx)
 *   --path          /path/to/access.log             (with --source file)
 *   --since         24h | 1h | 15m                  (default: 24h; docker only)
 *   --report        stdout | email | both           (default: both)
 *   --page-if-severe                                page high-sev instantly
 *   --window-label  "24h"                           label for the email
 *   --dry-run                                       do not write tarpit list
 *
 * Environment:
 *   CERBER_R14_DIR                 - rollup directory (default /var/lib/datachain-rope/cerber/rpc-classify)
 *   CERBER_R14_MALICIOUS_IPS_PATH  - tarpit map input file
 *   CERBER_ALERT_EMAIL / EMAIL_*   - see /etc/cerber-alert.env
 */

import { spawn, spawnSync } from "node:child_process";
import { createReadStream, existsSync } from "node:fs";
import { createInterface } from "node:readline";
import { hostname } from "node:os";

import {
  parseAccessLine,
  bucketByIp,
  summarizeBuckets,
  renderReport,
  writeRollup,
  writeMaliciousIps,
  defaultThresholds,
} from "../lib/rpc-classify.mjs";
import { pageEmail, alertEmailConfigured } from "../lib/page-email.mjs";

function parseArgs(argv) {
  const out = {
    source: "docker",
    container: "rope-nginx",
    path: null,
    since: "24h",
    report: "both",
    pageIfSevere: false,
    windowLabel: null,
    dryRun: false,
    reload: true,
  };
  for (let i = 2; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = argv[i + 1];
    switch (arg) {
      case "--source":
        out.source = next;
        i += 1;
        break;
      case "--container":
        out.container = next;
        i += 1;
        break;
      case "--path":
        out.path = next;
        i += 1;
        break;
      case "--since":
        out.since = next;
        i += 1;
        break;
      case "--report":
        out.report = next;
        i += 1;
        break;
      case "--page-if-severe":
        out.pageIfSevere = true;
        break;
      case "--window-label":
        out.windowLabel = next;
        i += 1;
        break;
      case "--dry-run":
        out.dryRun = true;
        break;
      case "--no-reload":
        out.reload = false;
        break;
      case "-h":
      case "--help":
        printHelp();
        process.exit(0);
      // eslint-disable-next-line no-fallthrough
      default:
        process.stderr.write(`unknown flag: ${arg}\n`);
        process.exit(2);
    }
  }
  if (!out.windowLabel) out.windowLabel = out.since;
  return out;
}

function printHelp() {
  process.stdout.write(
    "cerber-rpc-classify - CERBER R14 RPC activity classifier\n" +
      "  --source docker|stdin|file  (default docker)\n" +
      "  --container NAME            (default rope-nginx)\n" +
      "  --path PATH                 (with --source file)\n" +
      "  --since 24h|1h|15m          (docker only, default 24h)\n" +
      "  --report stdout|email|both  (default both)\n" +
      "  --page-if-severe            page contact@onguene.com immediately on malicious IP\n" +
      "  --window-label 'name'       label shown in the report\n" +
      "  --dry-run                   do not write tarpit list\n" +
      "  --no-reload                 do not run nginx -t + reload after map write\n"
  );
}

/**
 * Test the nginx config inside the rope-nginx container and, only if the test
 * passes, reload nginx so the newly-written malicious-ips.include is picked
 * up by the tarpit map. Any failure is logged but never crashes the classifier
 * (the next tick will pick up the same set of malicious IPs and try again).
 *
 * The map file is a bind-mount from the host into the container at
 * /etc/nginx/conf.d/malicious-ips.include, so no file copy is needed - the
 * file is already visible inside the container the moment we've written it
 * on the host.
 */
function reloadNginxMap({ container }) {
  const test = spawnSync("docker", ["exec", container, "nginx", "-t"], {
    encoding: "utf8",
    timeout: 10000,
  });
  if (test.status !== 0) {
    process.stderr.write(
      `[cerber-rpc-classify] nginx -t FAILED (status=${test.status}); skipping reload.\n` +
        (test.stderr || "").slice(0, 1200) + "\n"
    );
    return { reloaded: false, reason: "nginx-t-failed" };
  }
  const reload = spawnSync("docker", ["exec", container, "nginx", "-s", "reload"], {
    encoding: "utf8",
    timeout: 10000,
  });
  if (reload.status !== 0) {
    process.stderr.write(
      `[cerber-rpc-classify] nginx -s reload FAILED (status=${reload.status}).\n` +
        (reload.stderr || "").slice(0, 1200) + "\n"
    );
    return { reloaded: false, reason: "reload-failed" };
  }
  return { reloaded: true, reason: "ok" };
}

async function collectFromDocker({ container, since }) {
  const args = ["logs", "--since", since, container];
  const child = spawn("docker", args, { stdio: ["ignore", "pipe", "pipe"] });
  const records = [];
  let parseFail = 0;
  const stderrChunks = [];
  child.stderr.on("data", (c) => stderrChunks.push(c));
  const rl = createInterface({ input: child.stdout });
  for await (const line of rl) {
    const rec = parseAccessLine(line);
    if (rec) records.push(rec);
    else parseFail += 1;
  }
  const exit = await new Promise((resolve) => child.on("close", resolve));
  return { records, parseFail, exit, stderr: Buffer.concat(stderrChunks).toString("utf8") };
}

async function collectFromStdin() {
  const records = [];
  let parseFail = 0;
  const rl = createInterface({ input: process.stdin });
  for await (const line of rl) {
    const rec = parseAccessLine(line);
    if (rec) records.push(rec);
    else parseFail += 1;
  }
  return { records, parseFail, exit: 0, stderr: "" };
}

async function collectFromFile({ path }) {
  if (!path || !existsSync(path)) throw new Error(`file not found: ${path}`);
  const records = [];
  let parseFail = 0;
  const rl = createInterface({ input: createReadStream(path) });
  for await (const line of rl) {
    const rec = parseAccessLine(line);
    if (rec) records.push(rec);
    else parseFail += 1;
  }
  return { records, parseFail, exit: 0, stderr: "" };
}

async function main() {
  const args = parseArgs(process.argv);
  let collected;
  if (args.source === "docker") {
    collected = await collectFromDocker({ container: args.container, since: args.since });
  } else if (args.source === "stdin") {
    collected = await collectFromStdin();
  } else if (args.source === "file") {
    collected = await collectFromFile({ path: args.path });
  } else {
    process.stderr.write(`unknown --source: ${args.source}\n`);
    process.exit(2);
  }
  const { records, parseFail, exit, stderr } = collected;
  if (records.length === 0) {
    process.stderr.write(
      `[cerber-rpc-classify] no parseable log lines (fail=${parseFail}, exit=${exit}).\n`
    );
    if (stderr) process.stderr.write(stderr.slice(0, 800) + "\n");
    process.exit(1);
  }
  const thresholds = defaultThresholds();
  const buckets = bucketByIp(records);
  const summary = summarizeBuckets(buckets, thresholds);
  const report = renderReport(summary, { windowLabel: args.windowLabel });

  if (!args.dryRun) {
    writeRollup(summary, { source: args.source, container: args.container, since: args.since, host: hostname() });
    const mIps = writeMaliciousIps(summary);
    process.stderr.write(
      `[cerber-rpc-classify] tarpit map: kept=${mIps.keptCount} newly-added=${mIps.added.length} dropped=${mIps.dropped.length}\n`
    );
    // Only ask nginx to reload when the map actually changed - a no-op tick
    // otherwise reloads nginx every 15 min for no reason.
    if (args.reload && (mIps.added.length > 0 || mIps.dropped.length > 0)) {
      const r = reloadNginxMap({ container: args.container });
      process.stderr.write(
        `[cerber-rpc-classify] nginx reload: ${r.reloaded ? "ok" : `skipped (${r.reason})`}\n`
      );
    }
  }

  if (args.report === "stdout" || args.report === "both") {
    process.stdout.write(report + "\n");
  }
  if (args.report === "email" || args.report === "both") {
    if (!alertEmailConfigured()) {
      process.stderr.write(
        "[cerber-rpc-classify] CERBER_ALERT_EMAIL or SMTP not configured; skipping email.\n"
      );
    } else {
      const dayKey = new Date().toISOString().slice(0, 10);
      const res = await pageEmail({
        subject: `RPC activity report (${args.windowLabel}) - ${summary.totals.malicious ?? 0} malicious / ${summary.uniqueIps} unique IPs`,
        body: report,
        dedupeKey: `r14-daily:${dayKey}`,
        threatLevel: 2,
        rule: "r14-rpc-activity",
      });
      process.stderr.write(`[cerber-rpc-classify] email: ${res.sent ? "sent" : `not-sent (${res.reason})`}\n`);
    }
  }
  if (args.pageIfSevere && (summary.totals.malicious ?? 0) > 0) {
    const top = summary.rows.filter((r) => r.level === "malicious").slice(0, 10);
    const body =
      `${top.length} malicious IPs detected in ${args.windowLabel}. Each has been added\n` +
      `to the nginx tarpit map and will be served slow, plausible-but-wrong JSON-RPC\n` +
      `replies. No retaliatory traffic is generated.\n\n` +
      top
        .map(
          (r) =>
            `  ${r.ip.padEnd(20)} ${r.total} req  ${(r.reqPerHour || 0).toFixed(0)}/h  reasons=${r.reasons.join("; ") || "(rate-only)"}`
        )
        .join("\n");
    const res = await pageEmail({
      subject: `malicious IP burst detected (${top.length})`,
      body,
      dedupeKey: `r14-severe:${Math.floor(Date.now() / 3600000)}h`,
      threatLevel: 4,
      rule: "r14-rpc-activity",
    });
    process.stderr.write(`[cerber-rpc-classify] severe page: ${res.sent ? "sent" : `not-sent (${res.reason})`}\n`);
  }
}

main().catch((e) => {
  process.stderr.write(`[cerber-rpc-classify] fatal: ${e?.stack || e}\n`);
  process.exit(1);
});
