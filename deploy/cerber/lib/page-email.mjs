/**
 * CERBER mesh email paging (Rope / Alteros / Tanastok / DCSwap mesh daemons).
 *
 * Canonical recipient: CERBER_ALERT_EMAIL (must be contact@onguene.com mesh-wide).
 * SMTP: EMAIL_HOST / EMAIL_PORT / EMAIL_USER / EMAIL_PASS / EMAIL_FROM
 *        (same shape as DCSwap bot/.env — install via /etc/cerber-alert.env).
 *
 * Zero hard dependency: uses SendGrid HTTP v3 when EMAIL_HOST contains
 * "sendgrid" or EMAIL_PASS starts with SG.; otherwise STARTTLS SMTP via node:tls.
 * Dedupes per breach key for CERBER_ALERT_COOLDOWN_MS (default 6h).
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { createConnection } from "node:net";
import { connect as tlsConnect } from "node:tls";
import { hostname } from "node:os";

const STATE_PATH =
  process.env.CERBER_ALERT_STATE_PATH ||
  "/var/lib/datachain-rope/cerber/alert-state.json";
const COOLDOWN_MS = Number(process.env.CERBER_ALERT_COOLDOWN_MS ?? 6 * 60 * 60 * 1000);

function envAny(...names) {
  for (const n of names) {
    const v = process.env[n];
    if (v !== undefined && String(v).trim() !== "") return String(v).trim();
  }
  return undefined;
}

export function alertEmailConfigured() {
  const to = envAny("CERBER_ALERT_EMAIL", "FOUNDER_EMAIL_ADDRESS");
  const host = envAny("EMAIL_HOST", "SMTP_HOST");
  const pass = envAny("EMAIL_PASS", "SMTP_PASS", "SENDGRID_API_KEY");
  return Boolean(to && host && pass);
}

function readState() {
  try {
    if (!existsSync(STATE_PATH)) return {};
    return JSON.parse(readFileSync(STATE_PATH, "utf8")) ?? {};
  } catch {
    return {};
  }
}

function writeState(state) {
  try {
    mkdirSync(dirname(STATE_PATH), { recursive: true, mode: 0o750 });
    writeFileSync(STATE_PATH, JSON.stringify(state), { mode: 0o600 });
  } catch (e) {
    process.stderr.write(`[cerber-page] state write failed: ${e.message || e}\n`);
  }
}

function parseFrom(from) {
  const m = String(from).match(/^(.*)<([^>]+)>$/);
  if (m) return { name: m[1].trim().replace(/^"|"$/g, ""), email: m[2].trim() };
  return { email: String(from).trim() };
}

async function sendViaSendGrid({ from, to, subject, text, apiKey }) {
  const res = await fetch("https://api.sendgrid.com/v3/mail/send", {
    method: "POST",
    headers: {
      authorization: `Bearer ${apiKey}`,
      "content-type": "application/json",
    },
    body: JSON.stringify({
      personalizations: [{ to: [{ email: to }] }],
      from: parseFrom(from),
      subject,
      content: [{ type: "text/plain", value: text }],
    }),
    signal: AbortSignal.timeout(20_000),
  });
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new Error(`SendGrid HTTP ${res.status}: ${body.slice(0, 200)}`);
  }
}

function smtpRead(socket) {
  return new Promise((resolve, reject) => {
    let buf = "";
    const onData = (chunk) => {
      buf += chunk.toString("utf8");
      const lines = buf.split(/\r?\n/).filter(Boolean);
      const last = lines[lines.length - 1] || "";
      if (/^[0-9]{3}[ -]/.test(last) && /^[0-9]{3} /.test(last)) {
        cleanup();
        resolve(buf);
      }
    };
    const onErr = (e) => {
      cleanup();
      reject(e);
    };
    const onTimeout = () => {
      cleanup();
      reject(new Error("SMTP read timeout"));
    };
    const cleanup = () => {
      socket.off("data", onData);
      socket.off("error", onErr);
      socket.off("timeout", onTimeout);
    };
    socket.on("data", onData);
    socket.on("error", onErr);
    socket.once("timeout", onTimeout);
  });
}

async function smtpCmd(socket, line, expectPrefix) {
  if (line != null) socket.write(line + "\r\n");
  const resp = await smtpRead(socket);
  if (expectPrefix && !resp.startsWith(expectPrefix)) {
    throw new Error(`SMTP unexpected: ${resp.trim().slice(0, 180)}`);
  }
  return resp;
}

async function sendViaSmtpStartTls({ host, port, user, pass, from, to, subject, text }) {
  const plain = await new Promise((resolve, reject) => {
    const s = createConnection({ host, port }, () => resolve(s));
    s.setTimeout(20_000);
    s.once("error", reject);
  });
  await smtpCmd(plain, null, "220");
  await smtpCmd(plain, `EHLO ${hostname()}`, "250");
  await smtpCmd(plain, "STARTTLS", "220");
  const socket = await new Promise((resolve, reject) => {
    const s = tlsConnect({ socket: plain, servername: host }, () => resolve(s));
    s.setTimeout(20_000);
    s.once("error", reject);
  });
  try {
    await smtpCmd(socket, `EHLO ${hostname()}`, "250");
    await smtpCmd(socket, "AUTH LOGIN", "334");
    await smtpCmd(socket, Buffer.from(user, "utf8").toString("base64"), "334");
    await smtpCmd(socket, Buffer.from(pass, "utf8").toString("base64"), "235");
    const fromEmail = parseFrom(from).email;
    await smtpCmd(socket, `MAIL FROM:<${fromEmail}>`, "250");
    await smtpCmd(socket, `RCPT TO:<${to}>`, "250");
    await smtpCmd(socket, "DATA", "354");
    const payload =
      `From: ${from}\r\n` +
      `To: ${to}\r\n` +
      `Subject: ${subject}\r\n` +
      `MIME-Version: 1.0\r\n` +
      `Content-Type: text/plain; charset=utf-8\r\n` +
      `\r\n` +
      text.replace(/\r?\n/g, "\r\n") +
      "\r\n.";
    await smtpCmd(socket, payload, "250");
    await smtpCmd(socket, "QUIT", "221");
  } finally {
    try {
      socket.end();
    } catch {
      /* ignore */
    }
  }
}

/**
 * Page contact@onguene.com (or CERBER_ALERT_EMAIL) at most once per dedupeKey / cooldown.
 */
export async function pageEmail({ subject, body, dedupeKey, threatLevel = 4, rule = "mesh" }) {
  const to = envAny("CERBER_ALERT_EMAIL", "FOUNDER_EMAIL_ADDRESS");
  const host = envAny("EMAIL_HOST", "SMTP_HOST");
  const port = envAny("EMAIL_PORT", "SMTP_PORT") ?? "587";
  const user = envAny("EMAIL_USER", "SMTP_USER") ?? "apikey";
  const pass = envAny("EMAIL_PASS", "SMTP_PASS", "SENDGRID_API_KEY");
  const from =
    envAny("EMAIL_FROM", "SMTP_FROM", "DEFAULT_FROM_EMAIL") ||
    "Datachain CERBER <contact@datachain.one>";

  if (!to || !host || !pass) {
    return { sent: false, reason: "CERBER_ALERT_EMAIL or SMTP not configured in /etc/cerber-alert.env" };
  }

  const key = dedupeKey ?? `${rule}:${subject}`;
  const state = readState();
  const last = state[key]?.lastSentMs ?? 0;
  const now = Date.now();
  if (now - last < COOLDOWN_MS) {
    return {
      sent: false,
      reason: `suppressed, already paged ${Math.round((COOLDOWN_MS - (now - last)) / 60000)}m ago`,
      to,
    };
  }

  const text = [
    `CERBER invariant breach on ${hostname()} (${envAny("CERBER_PEER_ID") || "cerber"})`,
    ``,
    `Rule        : ${rule}`,
    `Threat level: ${threatLevel}`,
    `Detected    : ${new Date().toISOString()}`,
    ``,
    body,
    ``,
    `─────────────────────────────────────────────`,
    `Mesh-wide page address: ${to}`,
    `Further pages for this exact breach are suppressed for ${Math.round(COOLDOWN_MS / 3600000)}h.`,
  ].join("\n");

  const fullSubject = `[CERBER ${rule}/L${threatLevel}] ${subject}`;

  try {
    const useSendGrid =
      /sendgrid/i.test(host) || Boolean(envAny("SENDGRID_API_KEY")) || pass.startsWith("SG.");
    if (useSendGrid) {
      await sendViaSendGrid({ from, to, subject: fullSubject, text, apiKey: pass });
    } else {
      await sendViaSmtpStartTls({
        host,
        port: Number(port),
        user,
        pass,
        from,
        to,
        subject: fullSubject,
        text,
      });
    }
    state[key] = { lastSentMs: now, count: (state[key]?.count ?? 0) + 1, to };
    writeState(state);
    return { sent: true, to };
  } catch (e) {
    return { sent: false, reason: String(e?.message || e).slice(0, 240), to };
  }
}
