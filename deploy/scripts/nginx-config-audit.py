#!/usr/bin/env python3
"""nginx-config-audit.py

Regression guard for the failover pools defined in
deploy/nginx/conf.d/*.conf.

Motivation (2026-08-23)
-----------------------
Chainlist marked 3 of 4 published Rope RPC endpoints red because the nginx
`digitalocean_rpc` upstream had silently collapsed from a 3-node DigitalOcean
round-robin (per handover-digitalocean-third-blue-green-slot.mdc, 2026-05-03)
to a single `host.docker.internal:8545` (BLUE only). The name kept its
"failover-looking" identifier while every failover semantic was removed. The
only reason `erpc.datachain.network` stayed green is that it uses a *different*
upstream (`rpc_read_failover`) that still has the 3 attester backups.

Contract
--------
Every `upstream <name> { ... }` block MUST be preceded (immediately, on the
line above the `upstream` keyword) by a single machine-readable annotation:

    # nginx-audit: role=<role> [min-servers=N] [port=P] [must-include=A,B,...] [must-exclude=A,...] [write-safe=<true|false>]

Recognised roles + their default assertions:

* role=write-primary        : write-safe=true, min-servers=1, no `backup` markers
                              (writes MUST NEVER fail over onto an attester
                              mempool - see the 2026-07-25 silent-unmined tx
                              incident documented in datachain.network.conf).
* role=read-failover        : min-servers=2 by default, MUST include exactly
                              one non-backup primary + at least one `backup`.
* role=read-attesters-only  : min-servers=1, MUST exclude BLUE
                              (host.docker.internal:8545 or 127.0.0.1:8545).
* role=ws-writer-only       : min-servers=1, all servers on WS port (8546).
* role=ws-failover          : min-servers=2, all servers on WS port (8546).
* role=deprecated           : the upstream is scheduled for removal; the
                              audit only records that no reachable location
                              still points at it (that check is Tier C.v2).

Failure modes the guard MUST catch
----------------------------------
1. An upstream with role=read-failover collapses to 1 server (the bug that
   triggered this script).
2. A write-primary pool grows a `backup` server (writes would silently land
   in an attester mempool).
3. An attesters-only pool gains BLUE (writes routed to /v1/read would
   silently accept a raw tx that MetaMask thinks is going to a read-only
   endpoint).
4. A ws pool points at :8545 instead of :8546 (the 2026-08-11 ws.rope.network
   symptom - HTTP-JSON-RPC never returns 101 Switching Protocols).

Usage
-----
    python3 deploy/scripts/nginx-config-audit.py deploy/nginx/conf.d/*.conf

Exit codes: 0 all-green, 1 any assertion failed, 2 no configs found /
malformed annotation / syntax error inside an upstream block.

The script is stdlib-only (no jq, no yaml, no regex-based nginx parser).
Runs on any Python 3.8+. Safe to invoke in CI, pre-commit, or ad-hoc.
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Set, Tuple

BLUE_ADDRESSES: Set[str] = {
    "host.docker.internal:8545",
    "127.0.0.1:8545",
    "localhost:8545",
}
WS_PORT = "8546"

ANNOTATION_PREFIX = "# nginx-audit:"
ANNOTATION_KV_RE = re.compile(r"([a-zA-Z_-]+)=([^\s]+)")
UPSTREAM_OPEN_RE = re.compile(r"^\s*upstream\s+([A-Za-z0-9_]+)\s*\{\s*$")
SERVER_RE = re.compile(r"^\s*server\s+([^\s;]+)(.*?);\s*$")


@dataclass
class Annotation:
    """Parsed # nginx-audit: line for one upstream."""

    file: Path
    line: int
    role: str
    min_servers: int = 1
    port: Optional[str] = None
    must_include: Set[str] = field(default_factory=set)
    must_exclude: Set[str] = field(default_factory=set)
    write_safe: Optional[bool] = None

    @classmethod
    def parse(cls, file: Path, line: int, raw: str) -> "Annotation":
        rest = raw.split(ANNOTATION_PREFIX, 1)[1].strip()
        role: Optional[str] = None
        min_servers = 1
        port: Optional[str] = None
        must_include: Set[str] = set()
        must_exclude: Set[str] = set()
        write_safe: Optional[bool] = None

        for key, value in ANNOTATION_KV_RE.findall(rest):
            if key == "role":
                role = value
            elif key == "min-servers":
                if not value.isdigit():
                    _die(file, line, f"min-servers must be a positive integer, got {value!r}")
                min_servers = int(value)
            elif key == "port":
                port = value
            elif key == "must-include":
                must_include = {addr.strip() for addr in value.split(",") if addr.strip()}
            elif key == "must-exclude":
                must_exclude = {addr.strip() for addr in value.split(",") if addr.strip()}
            elif key == "write-safe":
                if value not in ("true", "false"):
                    _die(file, line, f"write-safe must be true|false, got {value!r}")
                write_safe = value == "true"
            else:
                _die(file, line, f"unknown nginx-audit key {key!r}")

        if role is None:
            _die(file, line, "annotation missing required key: role=<role>")

        return cls(
            file=file,
            line=line,
            role=role,
            min_servers=min_servers,
            port=port,
            must_include=must_include,
            must_exclude=must_exclude,
            write_safe=write_safe,
        )


@dataclass
class UpstreamServer:
    addr: str  # "host:port"
    options: str
    line: int

    @property
    def is_backup(self) -> bool:
        return "backup" in self.options.split()


@dataclass
class Upstream:
    file: Path
    name: str
    open_line: int
    close_line: int
    servers: List[UpstreamServer]
    annotation: Optional[Annotation]


def _die(file: Path, line: int, msg: str) -> None:
    print(f"{file}:{line}: FATAL: {msg}", file=sys.stderr)
    sys.exit(2)


def parse_conf_file(path: Path) -> List[Upstream]:
    """Very small hand-written nginx parser that ONLY understands upstream blocks.

    Everything else in the file is intentionally ignored: we do not simulate
    nginx, we just find blocks that match `upstream <name> { ... }` at any
    top-level position and parse `server` directives inside them.
    """
    text = path.read_text(encoding="utf-8", errors="replace").splitlines()

    upstreams: List[Upstream] = []
    pending_annotation: Optional[Annotation] = None

    i = 0
    while i < len(text):
        raw = text[i]
        stripped = raw.strip()

        if stripped.startswith(ANNOTATION_PREFIX):
            # `line` is 1-indexed and refers to the annotation line itself.
            pending_annotation = Annotation.parse(path, i + 1, stripped)
            i += 1
            continue

        # Blank / other comment lines between annotation and upstream are
        # tolerated (documentation blocks are useful).
        m = UPSTREAM_OPEN_RE.match(raw)
        if not m:
            # An annotation MUST be followed by an upstream block; if we see
            # any *non-blank, non-comment* directive first, that's a config
            # bug (someone added an annotation over the wrong thing).
            if pending_annotation and stripped and not stripped.startswith("#"):
                _die(
                    path,
                    pending_annotation.line,
                    "nginx-audit annotation is not immediately above an `upstream` block",
                )
            i += 1
            continue

        name = m.group(1)
        open_line = i + 1
        servers: List[UpstreamServer] = []
        j = i + 1
        while j < len(text):
            line = text[j]
            if line.strip() == "}":
                break
            sm = SERVER_RE.match(line)
            if sm:
                addr = sm.group(1)
                opts = sm.group(2).strip()
                servers.append(UpstreamServer(addr=addr, options=opts, line=j + 1))
            j += 1
        else:
            _die(path, open_line, f"upstream {name!r} is missing a closing brace")

        upstreams.append(
            Upstream(
                file=path,
                name=name,
                open_line=open_line,
                close_line=j + 1,
                servers=servers,
                annotation=pending_annotation,
            )
        )
        pending_annotation = None
        i = j + 1

    if pending_annotation is not None:
        _die(
            path,
            pending_annotation.line,
            "trailing nginx-audit annotation with no upstream after it",
        )

    return upstreams


class AuditResult:
    def __init__(self) -> None:
        self.errors: List[str] = []
        self.warnings: List[str] = []

    def err(self, up: Upstream, msg: str) -> None:
        self.errors.append(f"{up.file}:{up.open_line}: ERROR upstream {up.name!r}: {msg}")

    def warn(self, up: Upstream, msg: str) -> None:
        self.warnings.append(f"{up.file}:{up.open_line}: WARN  upstream {up.name!r}: {msg}")


def audit_upstream(up: Upstream, result: AuditResult) -> None:
    ann = up.annotation
    if ann is None:
        result.err(
            up,
            "no `# nginx-audit:` annotation directly above upstream. "
            "Add one, e.g. `# nginx-audit: role=read-failover min-servers=4`.",
        )
        return

    server_addrs = [s.addr for s in up.servers]
    n_servers = len(up.servers)
    n_backups = sum(1 for s in up.servers if s.is_backup)
    n_primaries = n_servers - n_backups

    # Universal: min-servers.
    if n_servers < ann.min_servers:
        result.err(
            up,
            f"has {n_servers} server(s), annotation requires min-servers={ann.min_servers}. "
            f"Actual: {server_addrs or '[]'}.",
        )

    # Universal: port.
    if ann.port:
        for s in up.servers:
            if ":" not in s.addr or s.addr.rsplit(":", 1)[1] != ann.port:
                result.err(
                    up,
                    f"server {s.addr!r} at line {s.line} is not on port {ann.port} "
                    f"(annotation port={ann.port}).",
                )

    # Universal: must-include.
    for req in ann.must_include:
        if req not in server_addrs:
            result.err(
                up,
                f"missing required server {req!r} (must-include). Actual: {server_addrs or '[]'}.",
            )

    # Universal: must-exclude.
    for forbidden in ann.must_exclude:
        if forbidden in server_addrs:
            result.err(
                up,
                f"server {forbidden!r} is forbidden by must-exclude. Actual: {server_addrs}.",
            )

    # Role-specific rules.
    role = ann.role

    if role == "write-primary":
        # 2026-07-25 mempool-hazard fix: writes MUST NEVER fail over.
        if ann.write_safe is False:
            result.err(up, "role=write-primary is inherently write-safe=true; drop write-safe=false.")
        if n_servers != 1:
            result.err(
                up,
                f"role=write-primary MUST have exactly 1 server (writes never fail over). "
                f"Got {n_servers}: {server_addrs}.",
            )
        if n_backups > 0:
            result.err(
                up,
                "role=write-primary MUST NOT declare any `backup` server "
                "(silent mempool divergence hazard - see 2026-07-25 incident, "
                "handover-to-dcswap-erpc-dropped-value-transfer-recovered-2026-07-29).",
            )
        primary_addr = server_addrs[0] if server_addrs else None
        if primary_addr and primary_addr not in BLUE_ADDRESSES:
            result.err(
                up,
                f"role=write-primary must target BLUE (one of {sorted(BLUE_ADDRESSES)}), "
                f"got {primary_addr!r}. BLUE is the only proposer on the Rope committee.",
            )

    elif role == "read-failover":
        # Real failover requires >= 2 servers (BLUE + at least one backup).
        eff_min = max(ann.min_servers, 2)
        if n_servers < eff_min:
            result.err(
                up,
                f"role=read-failover requires >=2 servers (BLUE primary + backups). "
                f"Got {n_servers}: {server_addrs}. "
                f"REGRESSION HAZARD: silent single-node collapse of a failover pool is "
                f"exactly the 2026-08-23 `digitalocean_rpc` bug this guard exists to catch.",
            )
        if n_primaries != 1:
            result.err(
                up,
                f"role=read-failover MUST have exactly 1 non-backup primary + N backups. "
                f"Got primaries={n_primaries}, backups={n_backups}.",
            )
        # Primary should be BLUE.
        primaries = [s for s in up.servers if not s.is_backup]
        if primaries and primaries[0].addr not in BLUE_ADDRESSES:
            result.err(
                up,
                f"role=read-failover primary must be BLUE (one of {sorted(BLUE_ADDRESSES)}), "
                f"got {primaries[0].addr!r}.",
            )

    elif role == "read-attesters-only":
        # 2026-08-14 spec: /v1/read must never accept BLUE.
        for s in up.servers:
            if s.addr in BLUE_ADDRESSES:
                result.err(
                    up,
                    f"role=read-attesters-only forbids BLUE ({s.addr!r} at line {s.line}). "
                    f"Ghost-tx hazard: a raw tx sent to /v1/read that landed on BLUE would "
                    f"be silently accepted into the mempool.",
                )

    elif role in ("ws-writer-only", "ws-failover"):
        for s in up.servers:
            port = s.addr.rsplit(":", 1)[1] if ":" in s.addr else None
            if port != WS_PORT:
                result.err(
                    up,
                    f"role={role} server {s.addr!r} is not on WS port {WS_PORT}. "
                    f"HTTP JSON-RPC (:8545) never returns 101 Switching Protocols - "
                    f"the ws.rope.network :8545 misroute is exactly what this checks for.",
                )
        if role == "ws-failover" and n_servers < 2:
            result.err(
                up,
                f"role=ws-failover requires >=2 servers, got {n_servers}.",
            )

    elif role == "deprecated":
        # No structural checks; caller is responsible for retiring callers.
        result.warn(
            up,
            "role=deprecated - a follow-up (Tier C.v2) will scan for any "
            "`proxy_pass http://" + up.name + "` still routing here.",
        )

    else:
        result.err(up, f"unknown role={role!r}. Recognised: write-primary, read-failover, "
                       f"read-attesters-only, ws-writer-only, ws-failover, deprecated.")


def audit(paths: List[Path], require_annotations: bool) -> AuditResult:
    result = AuditResult()
    if not paths:
        print("nginx-config-audit: no config files provided", file=sys.stderr)
        sys.exit(2)
    for path in paths:
        upstreams = parse_conf_file(path)
        for up in upstreams:
            if not require_annotations and up.annotation is None:
                continue
            audit_upstream(up, result)
    return result


def main(argv: List[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("configs", nargs="+", help="nginx *.conf files to audit")
    ap.add_argument(
        "--allow-unannotated",
        action="store_true",
        help="skip (do not fail on) upstreams without an nginx-audit annotation. "
             "Default is strict: every upstream must declare its role.",
    )
    ap.add_argument(
        "--quiet",
        action="store_true",
        help="only print errors/warnings, suppress the OK summary line",
    )
    args = ap.parse_args(argv)

    paths = [Path(p) for p in args.configs]
    missing = [p for p in paths if not p.is_file()]
    if missing:
        for p in missing:
            print(f"nginx-config-audit: not a file: {p}", file=sys.stderr)
        return 2

    result = audit(paths, require_annotations=not args.allow_unannotated)

    for w in result.warnings:
        print(w)
    for e in result.errors:
        print(e, file=sys.stderr)

    if result.errors:
        print(
            f"nginx-config-audit: FAIL - {len(result.errors)} error(s), "
            f"{len(result.warnings)} warning(s) across {len(paths)} file(s)",
            file=sys.stderr,
        )
        return 1

    if not args.quiet:
        print(
            f"nginx-config-audit: OK - {len(paths)} file(s), "
            f"{len(result.warnings)} warning(s), 0 errors"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
