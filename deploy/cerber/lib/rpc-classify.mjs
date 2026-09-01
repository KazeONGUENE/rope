/**
 * CERBER R14 - RPC activity classifier for erpc.datachain.network + peers.
 *
 * Consumes rope-nginx access-log lines (from `docker logs rope-nginx` or a
 * stdin pipe) and produces a per-IP behaviour profile with a threat level
 * in { normal, heavy, suspicious, malicious }.
 *
 * The classifier is DEFENSIVE:
 *   - malicious IPs are written to
 *     /opt/datachain-rope/code/deploy/nginx/conf.d/malicious-ips.include
 *     (bind-mounted read-only into the rope-nginx container at
 *     /etc/nginx/conf.d/malicious-ips.include; the `.include` extension
 *     keeps it out of nginx's `*.conf` auto-glob so it is only pulled in
 *     via the explicit `include` inside tarpit.map.conf's `map` block).
 *     Requests from those IPs are routed to the local tarpit (a slow,
 *     plausible-but-wrong JSON-RPC responder) so they do NOT reach the
 *     writer.
 *   - we NEVER emit retaliatory traffic. There is no DDoS-back mechanism.
 *
 * Output artifacts:
 *   /var/lib/datachain-rope/cerber/rpc-classify/latest.json      - full rollup
 *   /opt/datachain-rope/code/deploy/nginx/conf.d/malicious-ips.include
 *                                                                 - nginx map input
 *   /var/lib/datachain-rope/cerber/rpc-classify/history/...      - ndjson history
 *
 * Public helpers:
 *   parseAccessLine(line)              - regex parser for the log_format we use
 *   bucketByIp(records)                - Map<ip, aggregate>
 *   classifyBucket(agg, thresholds)    - { level, reasons[] }
 *   summarizeBuckets(buckets)          - overall stats + top-N tables
 *   renderReport(summary, options)     - text/markdown report body for email
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync, appendFileSync } from "node:fs";
import { dirname, join } from "node:path";

/* --------------------------------------------------------------------- */
/* Log parsing                                                            */
/* --------------------------------------------------------------------- */

/**
 * Matches the log_format defined in deploy/nginx/nginx.conf (2026-08-04):
 *
 *   '$remote_addr - $remote_user [$time_local] "$request" '
 *   '$status $body_bytes_sent "$http_referer" '
 *   '"$http_user_agent" ua_len=$request_length '
 *   'rt=$request_time uc=$upstream_connect_time '
 *   'ur=$upstream_response_time us=$upstream_status '
 *   'ua=$upstream_addr uct=$upstream_cache_status'
 */
const LOG_RE = new RegExp(
  "^(?<ip>[0-9a-f\\.:]+) - (?<user>\\S+) " +
    "\\[(?<time>[^\\]]+)\\] " +
    '"(?<method>[A-Z]+) (?<path>[^" ]*) (?<proto>[^"]+)" ' +
    "(?<status>\\d+) (?<bytes>\\d+) " +
    '"(?<ref>[^"]*)" ' +
    '"(?<ua>[^"]*)" ' +
    "ua_len=(?<ua_len>\\d+) " +
    "rt=(?<rt>\\S+) " +
    "uc=(?<uc>\\S+) " +
    "ur=(?<ur>\\S+) " +
    "us=(?<us>\\S+) " +
    "ua=(?<upstream>\\S+) " +
    "uct=(?<uct>\\S+)"
);

const TIME_RE = /^(\d{2})\/([A-Za-z]{3})\/(\d{4}):(\d{2}):(\d{2}):(\d{2}) ([+\-]\d{4})$/;
const MONTHS = { Jan: 0, Feb: 1, Mar: 2, Apr: 3, May: 4, Jun: 5, Jul: 6, Aug: 7, Sep: 8, Oct: 9, Nov: 10, Dec: 11 };

function parseNginxTime(t) {
  const m = TIME_RE.exec(t);
  if (!m) return null;
  const [, dd, mon, yyyy, HH, MM, SS, tz] = m;
  const sign = tz.startsWith("-") ? -1 : 1;
  const tzMin = sign * (Number(tz.slice(1, 3)) * 60 + Number(tz.slice(3, 5)));
  const utc = Date.UTC(Number(yyyy), MONTHS[mon] ?? 0, Number(dd), Number(HH), Number(MM), Number(SS));
  return new Date(utc - tzMin * 60_000);
}

export function parseAccessLine(line) {
  const clean = String(line).replace(/^\s+|\s+$/g, "");
  if (!clean) return null;
  const m = LOG_RE.exec(clean);
  if (!m) return null;
  const g = m.groups;
  const ts = parseNginxTime(g.time);
  return {
    ts: ts ? ts.toISOString() : null,
    tsMs: ts ? ts.getTime() : Date.now(),
    ip: g.ip,
    user: g.user === "-" ? null : g.user,
    method: g.method,
    path: g.path,
    proto: g.proto,
    status: Number(g.status),
    bytes: Number(g.bytes),
    ref: g.ref === "-" ? null : g.ref,
    ua: g.ua === "-" ? "" : g.ua,
    uaLen: Number(g.ua_len),
    rt: g.rt === "-" ? null : Number(g.rt),
    uc: g.uc === "-" ? null : Number(g.uc),
    ur: g.ur === "-" ? null : Number(g.ur),
    us: g.us === "-" ? null : Number(g.us),
    upstream: g.upstream === "-" ? null : g.upstream,
    uct: g.uct === "-" ? null : g.uct,
  };
}

/* --------------------------------------------------------------------- */
/* Aggregation                                                            */
/* --------------------------------------------------------------------- */

export function newAgg(ip) {
  return {
    ip,
    total: 0,
    firstSeenMs: Number.POSITIVE_INFINITY,
    lastSeenMs: 0,
    byStatus: new Map(),
    byMethod: new Map(),
    byPath: new Map(),
    byUa: new Map(),
    byUpstream: new Map(),
    rts: [],
    bytesTotal: 0,
    error4xx: 0,
    error5xx: 0,
    ok2xx: 0,
  };
}

function inc(map, key) {
  map.set(key, (map.get(key) ?? 0) + 1);
}

export function ingestRecord(agg, r) {
  agg.total += 1;
  if (r.tsMs < agg.firstSeenMs) agg.firstSeenMs = r.tsMs;
  if (r.tsMs > agg.lastSeenMs) agg.lastSeenMs = r.tsMs;
  inc(agg.byStatus, r.status);
  inc(agg.byMethod, r.method);
  inc(agg.byPath, r.path.length > 80 ? r.path.slice(0, 80) + "..." : r.path);
  inc(agg.byUa, r.ua.length > 120 ? r.ua.slice(0, 120) + "..." : r.ua);
  if (r.upstream) inc(agg.byUpstream, r.upstream);
  if (Number.isFinite(r.rt)) agg.rts.push(r.rt);
  agg.bytesTotal += r.bytes;
  if (r.status >= 200 && r.status < 300) agg.ok2xx += 1;
  else if (r.status >= 400 && r.status < 500) agg.error4xx += 1;
  else if (r.status >= 500) agg.error5xx += 1;
}

export function bucketByIp(records) {
  const out = new Map();
  for (const r of records) {
    if (!r || !r.ip) continue;
    let agg = out.get(r.ip);
    if (!agg) {
      agg = newAgg(r.ip);
      out.set(r.ip, agg);
    }
    ingestRecord(agg, r);
  }
  return out;
}

/* --------------------------------------------------------------------- */
/* Classification                                                         */
/* --------------------------------------------------------------------- */

/**
 * Signatures that flag an IP as scanning/exploit regardless of rate.
 * These are the string patterns known to appear in RPC-facing scans in
 * production journals; extend cautiously - a false positive here tarpits
 * a real user.
 */
export const SCANNER_UA_PATTERNS = [
  /Go-http-client\/1\.\d+ Nmap/i,
  /Nmap/i,
  /masscan/i,
  /zgrab/i,
  /shodan/i,
  /Expanse/i,
  /internet-measurement/i,
  /paloaltonetworks\.com/i,
  /censys/i,
  /Nikto/i,
];

export const SCANNER_PATH_PATTERNS = [
  /\/\.git\//i,
  /\/\.env(\.|$)/i,
  /\/wp-(admin|login|content)/i,
  /\/phpmyadmin/i,
  /\/xmlrpc\.php/i,
  /\/config\.json$/i,
  /\/\.aws\//i,
  /\/actuator\//i,
];

export function defaultThresholds() {
  return {
    heavyPerHour: 1_000,
    suspiciousPerHour: 5_000,
    maliciousPerHour: 10_000,
    error4xxRatioSuspicious: 0.5,
    error4xxRatioMalicious: 0.75,
    error5xxRatioSuspicious: 0.5,
    minSamplesForRatio: 20,
    scannerUaAutoMalicious: true,
    scannerPathAutoMalicious: true,
  };
}

/**
 * @returns {{ level: 'normal'|'heavy'|'suspicious'|'malicious', reasons: string[], reqPerHour: number, error4xxRatio: number, error5xxRatio: number }}
 */
export function classifyBucket(agg, thresholds = defaultThresholds()) {
  const spanMs = Math.max(1, agg.lastSeenMs - agg.firstSeenMs);
  const spanHours = spanMs / 3_600_000;
  const reqPerHour = spanHours > 0 ? agg.total / spanHours : agg.total;
  const error4xxRatio = agg.total > 0 ? agg.error4xx / agg.total : 0;
  const error5xxRatio = agg.total > 0 ? agg.error5xx / agg.total : 0;
  const reasons = [];
  let level = "normal";

  // Scanner signature autoclass ---------------------------------------
  if (thresholds.scannerUaAutoMalicious) {
    for (const [ua] of agg.byUa) {
      if (SCANNER_UA_PATTERNS.some((re) => re.test(ua))) {
        reasons.push(`scanner UA: ${ua.slice(0, 80)}`);
        level = "malicious";
        break;
      }
    }
  }
  if (thresholds.scannerPathAutoMalicious) {
    for (const [p] of agg.byPath) {
      if (SCANNER_PATH_PATTERNS.some((re) => re.test(p))) {
        reasons.push(`scanner path: ${p.slice(0, 80)}`);
        level = "malicious";
        break;
      }
    }
  }

  // Rate-based promotion ---------------------------------------------
  if (level !== "malicious") {
    if (reqPerHour >= thresholds.maliciousPerHour) {
      reasons.push(`rate ${reqPerHour.toFixed(0)} req/h >= malicious ${thresholds.maliciousPerHour}`);
      level = "malicious";
    } else if (reqPerHour >= thresholds.suspiciousPerHour) {
      reasons.push(`rate ${reqPerHour.toFixed(0)} req/h >= suspicious ${thresholds.suspiciousPerHour}`);
      level = "suspicious";
    } else if (reqPerHour >= thresholds.heavyPerHour) {
      reasons.push(`rate ${reqPerHour.toFixed(0)} req/h >= heavy ${thresholds.heavyPerHour}`);
      level = "heavy";
    }
  }

  // Error-ratio promotion --------------------------------------------
  if (agg.total >= thresholds.minSamplesForRatio) {
    if (error4xxRatio >= thresholds.error4xxRatioMalicious && level !== "malicious") {
      reasons.push(`4xx ratio ${(error4xxRatio * 100).toFixed(0)}% >= malicious ${(thresholds.error4xxRatioMalicious * 100).toFixed(0)}%`);
      level = "malicious";
    } else if (error4xxRatio >= thresholds.error4xxRatioSuspicious && level === "normal") {
      reasons.push(`4xx ratio ${(error4xxRatio * 100).toFixed(0)}% >= suspicious ${(thresholds.error4xxRatioSuspicious * 100).toFixed(0)}%`);
      level = "suspicious";
    }
    if (error5xxRatio >= thresholds.error5xxRatioSuspicious && level === "normal") {
      reasons.push(`5xx ratio ${(error5xxRatio * 100).toFixed(0)}%`);
      level = "suspicious";
    }
  }

  return { level, reasons, reqPerHour, error4xxRatio, error5xxRatio };
}

/* --------------------------------------------------------------------- */
/* Summary + report                                                       */
/* --------------------------------------------------------------------- */

function topN(map, n) {
  return [...map.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, n)
    .map(([k, v]) => ({ key: k, count: v }));
}

function pct(rts, p) {
  if (rts.length === 0) return null;
  const sorted = [...rts].sort((a, b) => a - b);
  const idx = Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length));
  return sorted[idx];
}

export function summarizeBuckets(buckets, thresholds = defaultThresholds()) {
  const rows = [];
  for (const agg of buckets.values()) {
    const c = classifyBucket(agg, thresholds);
    rows.push({
      ip: agg.ip,
      total: agg.total,
      firstSeenMs: agg.firstSeenMs,
      lastSeenMs: agg.lastSeenMs,
      spanMinutes: (agg.lastSeenMs - agg.firstSeenMs) / 60_000,
      level: c.level,
      reasons: c.reasons,
      reqPerHour: c.reqPerHour,
      error4xxRatio: c.error4xxRatio,
      error5xxRatio: c.error5xxRatio,
      p50rt: pct(agg.rts, 50),
      p95rt: pct(agg.rts, 95),
      topStatus: topN(agg.byStatus, 3),
      topMethod: topN(agg.byMethod, 3),
      topPath: topN(agg.byPath, 3),
      topUa: topN(agg.byUa, 2),
      topUpstream: topN(agg.byUpstream, 3),
    });
  }
  rows.sort((a, b) => {
    const rank = { malicious: 3, suspicious: 2, heavy: 1, normal: 0 };
    if (rank[b.level] !== rank[a.level]) return rank[b.level] - rank[a.level];
    return b.total - a.total;
  });
  const totals = rows.reduce(
    (acc, r) => {
      acc.total += r.total;
      acc[r.level] = (acc[r.level] ?? 0) + 1;
      return acc;
    },
    { total: 0, malicious: 0, suspicious: 0, heavy: 0, normal: 0 }
  );
  return { rows, totals, uniqueIps: rows.length };
}

export function renderReport(summary, { windowLabel = "24h", topIps = 20 } = {}) {
  const { rows, totals, uniqueIps } = summary;
  const lines = [];
  lines.push(`CERBER R14 - RPC activity classification (${windowLabel})`);
  lines.push(`Unique client IPs           : ${uniqueIps}`);
  lines.push(`Total requests              : ${totals.total}`);
  lines.push(`  normal                    : ${totals.normal ?? 0}`);
  lines.push(`  heavy    (integrator)     : ${totals.heavy ?? 0}`);
  lines.push(`  suspicious                : ${totals.suspicious ?? 0}`);
  lines.push(`  malicious (tarpited)      : ${totals.malicious ?? 0}`);
  lines.push("");
  const topRows = rows.slice(0, topIps);
  lines.push(`Top ${topRows.length} clients by activity + threat level:`);
  lines.push("-".repeat(78));
  for (const r of topRows) {
    lines.push(
      `${r.ip.padEnd(20)}  ${r.level.padEnd(10)}  ` +
        `${String(r.total).padStart(7)} req  ` +
        `${(r.reqPerHour || 0).toFixed(0).padStart(6)}/h  ` +
        `4xx=${(r.error4xxRatio * 100).toFixed(0).padStart(3)}%  ` +
        `5xx=${(r.error5xxRatio * 100).toFixed(0).padStart(3)}%  ` +
        `p95=${r.p95rt != null ? r.p95rt.toFixed(2) + "s" : "n/a"}`
    );
    for (const reason of r.reasons.slice(0, 2)) lines.push(`  reason: ${reason}`);
    const ua = r.topUa[0]?.key || "";
    if (ua) lines.push(`  ua    : ${ua.slice(0, 70)}`);
  }
  lines.push("");
  lines.push("Malicious IPs are automatically added to the nginx tarpit map");
  lines.push("(deploy/nginx/conf.d/tarpit.map.conf) and served fake JSON-RPC replies.");
  lines.push("There is NO retaliatory traffic. See handover for policy.");
  return lines.join("\n");
}

/* --------------------------------------------------------------------- */
/* Persistence                                                            */
/* --------------------------------------------------------------------- */

export function baseDir() {
  return process.env.CERBER_R14_DIR || "/var/lib/datachain-rope/cerber/rpc-classify";
}

export function maliciousIpsPath() {
  // Default target is inside the rope-nginx conf.d bind-mount so the file
  // is visible to nginx workers. The `.include` extension prevents nginx's
  // `include /etc/nginx/conf.d/*.conf;` auto-glob from picking it up as a
  // top-level directive block; it is only consumed via the explicit
  // `include` inside tarpit.map.conf's `map { ... }` block. Override with
  // CERBER_R14_MALICIOUS_IPS_PATH on hosts where the bind-mount differs.
  return (
    process.env.CERBER_R14_MALICIOUS_IPS_PATH ||
    "/opt/datachain-rope/code/deploy/nginx/conf.d/malicious-ips.include"
  );
}

function ensureDir(p) {
  mkdirSync(p, { recursive: true, mode: 0o750 });
}

export function writeRollup(summary, extra = {}) {
  ensureDir(baseDir());
  const body = { generatedAt: new Date().toISOString(), ...extra, summary };
  writeFileSync(join(baseDir(), "latest.json"), JSON.stringify(body, null, 2), { mode: 0o640 });
  const day = new Date().toISOString().slice(0, 10);
  ensureDir(join(baseDir(), "history"));
  appendFileSync(
    join(baseDir(), "history", `classify-${day}.ndjson`),
    JSON.stringify(body) + "\n",
    { mode: 0o640 }
  );
}

/**
 * Write nginx tarpit map input: one line per malicious IP,
 *   "1.2.3.4 malicious"
 * plus a trailing "default normal" so the nginx `map` has a fallback.
 *
 * A separate signed "malicious-ips.signed.json" file is written alongside
 * for peers that want to verify the list is authentic.
 */
export function writeMaliciousIps(summary, { retainOldMs = 24 * 60 * 60 * 1000 } = {}) {
  const path = maliciousIpsPath();
  ensureDir(dirname(path));
  const now = Date.now();
  const previous = readMaliciousIps(path);
  const seen = new Map(previous);
  for (const r of summary.rows) {
    if (r.level === "malicious") {
      seen.set(r.ip, { addedMs: seen.get(r.ip)?.addedMs ?? now, refreshedMs: now, reasons: r.reasons });
    }
  }
  const kept = [];
  const dropped = [];
  for (const [ip, meta] of seen) {
    if (now - meta.refreshedMs > retainOldMs) dropped.push(ip);
    else kept.push([ip, meta]);
  }
  const lines = [
    "# CERBER R14 - malicious IPs (auto-generated, do not hand-edit).",
    "# Each row -> nginx map $remote_addr $rope_tarpit_flag.",
    "# The `default` arm is declared in tarpit.map.conf; do NOT duplicate it here.",
    "# Regenerated every scan window; rows retained for retainOldMs after last hit.",
    `# Generated: ${new Date(now).toISOString()}`,
    "",
    ...kept.map(([ip, meta]) => `${ip} malicious;  # since ${new Date(meta.addedMs).toISOString()}`),
    "",
  ];
  writeFileSync(path, lines.join("\n"), { mode: 0o640 });
  return { added: summary.rows.filter((r) => r.level === "malicious").map((r) => r.ip), dropped, keptCount: kept.length };
}

export function readMaliciousIps(path = maliciousIpsPath()) {
  if (!existsSync(path)) return new Map();
  const text = readFileSync(path, "utf8");
  const out = new Map();
  for (const rawLine of text.split("\n")) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#") || line.startsWith("default")) continue;
    const m = /^([0-9a-f\.:]+)\s+malicious;.*?since\s+(\S+)/i.exec(line);
    if (m) {
      const [, ip, iso] = m;
      const addedMs = Date.parse(iso) || Date.now();
      out.set(ip, { addedMs, refreshedMs: addedMs, reasons: [] });
    }
  }
  return out;
}
