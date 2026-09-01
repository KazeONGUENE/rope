#!/usr/bin/env python3
"""Attester-only /v1/read filter for DO/GREEN edges that lack njs.

BLUE docker nginx uses rpc_router.routeAttesterRead (HTTP 405 on writes).
DO-rpc-1 / DO-rpc-2 run Ubuntu nginx 1.18 without njs, so `location /`
used to forward eth_sendRawTransaction to the local attester mempool
(ghost-tx class, 2026-07-29). This process is the njs-equivalent:

  GET  -> descriptor JSON (writes=false)
  POST write / rope_* / txpool_* / empty body -> HTTP 405
  POST eth_* reads -> proxy to local rope-node :8545

Listen: 127.0.0.1:18547 (nginx location = /v1/read proxies here).
Reth websocket already occupies :8547 (--ws.port), so this filter must not collide.
"""
from __future__ import annotations

import json
import os
import socket
import sys
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LISTEN_HOST = os.environ.get("ATTESTER_READ_LISTEN", "127.0.0.1")
LISTEN_PORT = int(os.environ.get("ATTESTER_READ_PORT", "18547"))
UPSTREAM = os.environ.get("ATTESTER_READ_UPSTREAM", "http://127.0.0.1:8545")
MAX_BODY = int(os.environ.get("ATTESTER_READ_MAX_BODY", str(1 * 1024 * 1024)))
UPSTREAM_TIMEOUT = float(os.environ.get("ATTESTER_READ_TIMEOUT", "10"))

WRITE_METHODS = {
    "eth_sendRawTransaction",
    "eth_sendTransaction",
    "eth_sign",
    "eth_signTransaction",
    "eth_signTypedData",
    "eth_signTypedData_v3",
    "eth_signTypedData_v4",
    "personal_sign",
    "rope_untieKnot",
    "rope_erasePersonalLedger",
    "rope_appendToLedger",
    "rope_createPersonalLedger",
    "rope_anchorDeployerAttestation",
    "rope_submitTestimony",
    "rope_registerValidator",
    "rope_v2_appendKnot",
    "rope_v2_compact",
    "rope_registerDevice",
    "rope_ingestTelemetry",
    "rope_subscribeAgentToWallet",
    "txpool_content",
    "txpool_status",
    "txpool_inspect",
}

DESCRIPTOR = {
    "ok": True,
    "role": "attester-read",
    "writes": False,
    "url": "https://erpc.datachain.network/v1/read",
    "note": (
        "eth_* reads against this attester only. Never send "
        "eth_sendRawTransaction or rope_* here."
    ),
}

DENY_BODY = (
    '{"jsonrpc":"2.0","id":null,"error":{"code":-32601,'
    '"message":"Method denied on attester-read endpoint; writes and rope_* '
    'stay on https://erpc.datachain.network/"}}\n'
)

CORS = {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type, Authorization",
    "Cache-Control": "no-store",
    "X-Rope-Read-Pool": "attesters-only",
}


def methods_from_body(raw: bytes) -> list[str]:
    if not raw:
        return ["<empty>"]
    try:
        parsed = json.loads(raw.decode("utf-8"))
    except (ValueError, UnicodeDecodeError):
        return ["<unparseable>"]
    items = parsed if isinstance(parsed, list) else [parsed]
    out: list[str] = []
    for item in items:
        if isinstance(item, dict) and isinstance(item.get("method"), str):
            out.append(item["method"])
    return out or ["<missing-method>"]


def is_denied(methods: list[str]) -> bool:
    for m in methods:
        if m in WRITE_METHODS:
            return True
        if m.startswith("rope_"):
            return True
        if m in ("<empty>", "<unparseable>", "<missing-method>"):
            return True
    return False


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt: str, *args) -> None:
        sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))

    def _send(self, code: int, body: bytes, content_type: str) -> None:
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        for k, v in CORS.items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def do_OPTIONS(self) -> None:  # noqa: N802
        self._send(204, b"", "text/plain")

    def do_GET(self) -> None:  # noqa: N802
        body = (json.dumps(DESCRIPTOR, separators=(",", ":")) + "\n").encode()
        self._send(200, body, "application/json")

    def do_HEAD(self) -> None:  # noqa: N802
        self.do_GET()

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("Content-Length") or "0")
        if length > MAX_BODY:
            self._send(413, b'{"error":"payload too large"}\n', "application/json")
            return
        raw = self.rfile.read(length) if length else b""
        if is_denied(methods_from_body(raw)):
            self._send(405, DENY_BODY.encode(), "application/json")
            return
        req = urllib.request.Request(
            UPSTREAM,
            data=raw,
            method="POST",
            headers={
                "Content-Type": self.headers.get("Content-Type") or "application/json",
                "Content-Length": str(len(raw)),
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=UPSTREAM_TIMEOUT) as resp:
                payload = resp.read()
                self.send_response(resp.status)
                self.send_header(
                    "Content-Type",
                    resp.headers.get("Content-Type") or "application/json",
                )
                self.send_header("Content-Length", str(len(payload)))
                for k, v in CORS.items():
                    self.send_header(k, v)
                rpc_ver = resp.headers.get("X-Rope-RPC-Version")
                if rpc_ver:
                    self.send_header("X-Rope-RPC-Version", rpc_ver)
                self.end_headers()
                self.wfile.write(payload)
        except urllib.error.HTTPError as e:
            payload = e.read()
            self._send(e.code, payload or b"", e.headers.get("Content-Type") or "application/json")
        except (urllib.error.URLError, TimeoutError, socket.timeout) as e:
            msg = (
                '{"jsonrpc":"2.0","id":null,"error":{"code":-32603,'
                '"message":"attester-read upstream unavailable: %s"}}\n'
                % str(e).replace('"', "'")
            )
            self._send(502, msg.encode(), "application/json")


def main() -> int:
    httpd = ThreadingHTTPServer((LISTEN_HOST, LISTEN_PORT), Handler)
    httpd.allow_reuse_address = True
    sys.stderr.write(
        "attester-read-proxy listening on %s:%s -> %s\n"
        % (LISTEN_HOST, LISTEN_PORT, UPSTREAM)
    )
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
