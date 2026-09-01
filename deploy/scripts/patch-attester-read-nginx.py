#!/usr/bin/env python3
"""Idempotently insert location = /v1/read into DO/GREEN erpc vhosts.

Usage: python3 patch-attester-read-nginx.py /etc/nginx/conf.d/datachain.network.conf [...]
"""
from __future__ import annotations

import datetime
import pathlib
import re
import shutil
import sys

SNIPPET_PATHS = [
    pathlib.Path("/opt/datachain-rope/nginx-snippets/attester-read-proxy.inc"),
    pathlib.Path(__file__).resolve().parent.parent / "nginx" / "snippets" / "attester-read-proxy.inc",
]

SERVER_NAME_RE = re.compile(
    r"^\s*server_name\s+erpc\.(?:datachain|rope)\.network\s*;\s*$"
)
LOCATION_ROOT_RE = re.compile(r"^\s*location\s+/\s*\{\s*$")


def load_snippet() -> str:
    for p in SNIPPET_PATHS:
        if p.is_file():
            text = p.read_text()
            if not text.endswith("\n"):
                text += "\n"
            return text
    raise SystemExit("attester-read-proxy.inc not found")


def already_patched(text: str) -> bool:
    return "location = /v1/read" in text and "18547" in text


def patch_text(text: str, snippet: str) -> tuple[str, int]:
    if already_patched(text):
        return text, 0
    lines = text.splitlines(True)
    out: list[str] = []
    pending = False
    inserts = 0
    for line in lines:
        if SERVER_NAME_RE.match(line):
            pending = True
        if pending and LOCATION_ROOT_RE.match(line):
            if not snippet.startswith("\n"):
                out.append("\n")
            out.append(snippet)
            if not snippet.endswith("\n"):
                out.append("\n")
            pending = False
            inserts += 1
        out.append(line)
    return "".join(out), inserts


def main(argv: list[str]) -> int:
    snippet = load_snippet()
    paths = [pathlib.Path(a) for a in argv[1:]]
    if not paths:
        raise SystemExit("usage: patch-attester-read-nginx.py <nginx-conf> [...]")
    changed = 0
    for path in paths:
        original = path.read_text()
        updated, inserts = patch_text(original, snippet)
        if inserts == 0:
            print("%s: already patched or no erpc server_name match" % path)
            continue
        bak = path.with_name(
            path.name + ".bak-pre-v1-read-" + datetime.datetime.utcnow().strftime("%Y%m%dT%H%M%SZ")
        )
        shutil.copy2(path, bak)
        path.write_text(updated)
        print("%s: inserted %d /v1/read location(s); backup %s" % (path, inserts, bak))
        changed += 1
    return 0 if changed or all(already_patched(p.read_text()) for p in paths) else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
