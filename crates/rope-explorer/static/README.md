# DCScan static frontend

This directory contains the **same HTML/CSS frontend** that was previously served by dcscan-api (`public/`). dc-explorer serves it directly so the block explorer UI (dcscan.io) is delivered from a single service.

## How it is used

- **By default:** When you run dc-explorer from the repo (e.g. `cargo run -p rope-explorer` from the workspace root, or from `crates/rope-explorer` with `static/` present), the binary looks for a directory named `static` with `index.html` (next to the current working directory or under common workspace paths) and serves it for all non-API routes.
- **Override:** Set **`DCSCAN_STATIC`** to the absolute (or relative) path of a directory that contains `index.html` to serve a different frontend or a different location (e.g. in production: `/opt/datachain-rope/dc-explorer/static`).

## Contents

- `index.html` - landing and SPA entry
- Pages: `strings.html`, `txs.html`, `accounts.html`, `tokens.html`, `verify.html`, `defi.html`, `stats.html`, `testimonies.html`, `agents.html`, etc.
- Detail routes: `tx/`, `address/`, `string/`, `token/`, `blockchain/`, `agents/`, `tokens/`
- Assets: `css/`, `assets/`

All `/api/v1/*` requests are handled by dc-explorer; everything else falls back to these static files (same behavior as former dcscan-api).
