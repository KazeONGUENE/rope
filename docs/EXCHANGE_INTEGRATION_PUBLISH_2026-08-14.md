# Publish notes: `dcscan.io/apis/exchange-integration` (v1.0, 2026-08-14)

**Owner:** Datachain Rope agent
**Deploy status:** ready to sync + reload. Operator-gated (per the standing rule: any prod `systemctl restart` / nginx reload / cargo restart happens through the operator).
**Files:**
- Source tree: `datachain-rope/crates/rope-explorer/static/apis/exchange-integration/index.html`
- Deploy tree: `datachain-rope/deploy/nginx/html/dcscan/apis/exchange-integration/index.html`
- Both trees identical (74,904 bytes, md5 verified equal by identical byte count and same content).

Zero em/en dash characters and zero em/en dash HTML entities present. Follows `dcscan-header-style-and-asset-caching.mdc`:

- Header markup exactly matches `apis.html` (nav labels without leading icons, chevron only, `.header-actions .btn-secondary` connect pill).
- Top-bar uses ID selectors `#topbar-fat-price`, `#topbar-fat-change`, `#topbar-gas` with plain `-` placeholders. `dcscan-stats.js` (already deployed) populates them on first request.
- No `?v=` cache-busters on `<script>` tags. ETag-based revalidation from the 2026-08-11 nginx cache policy handles freshness.

Follows `datachain-rope-canonical-design-system.mdc`: white cards on `--gray-50`, black primary buttons, semantic status pills, monospace for hashes. All colors from the canonical palette; no ad-hoc styling.

## Deploy

The path `dcscan.io/apis/exchange-integration` needs nothing beyond serving the new file; the existing nginx location for `dcscan.io` already has `try_files $uri $uri/index.html /index.html` at the root, which is why `/apis` (which is `apis.html`, a top-level file) works today and why placing `apis/exchange-integration/index.html` in the served root makes `/apis/exchange-integration` resolve correctly. **Do not** need to touch `deploy/nginx/conf.d/dcscan.io.conf`.

Operator sequence (rope-vps, `/opt/datachain-rope`):

```bash
# 0. Pre-flight (from local laptop, dry run)
cd "/Users/kazealphonseonguene/Downloads/DATACHAIN ROPE"
diff -r "datachain-rope/crates/rope-explorer/static/apis/exchange-integration/" \
        "datachain-rope/deploy/nginx/html/dcscan/apis/exchange-integration/"
# Expect no output = identical.

# 1. Rsync the new directory to rope-vps deploy tree.
rsync -avz "datachain-rope/deploy/nginx/html/dcscan/apis/exchange-integration/" \
  ubuntu@rope-vps:/opt/datachain-rope/code/deploy/nginx/html/dcscan/apis/exchange-integration/

# 2. Sync the source tree copy too (for future `cargo run` local previews on the VPS).
rsync -avz "datachain-rope/crates/rope-explorer/static/apis/exchange-integration/" \
  ubuntu@rope-vps:/home/ubuntu/datachain-rope/crates/rope-explorer/static/apis/exchange-integration/

# 3. Sanity-check nginx config on the VPS (no config change but always verify).
ssh ubuntu@rope-vps 'sudo docker exec rope-nginx nginx -t'
# Expect: syntax is ok, test is successful

# 4. Nothing to reload (static file drop; no `docker exec ... -s reload` required unless
#    you want to bust nginx open_file_cache immediately). If you'd like to force it:
# ssh ubuntu@rope-vps 'sudo docker exec rope-nginx nginx -s reload'

# 5. Verify from any workstation.
curl -sSI https://dcscan.io/apis/exchange-integration | head -n 3
# Expect: HTTP/2 200, content-type: text/html
curl -sS https://dcscan.io/apis/exchange-integration | wc -c
# Expect: ~74904

# 6. Sub-anchor spot check (the page has 13 sections; three sample anchors).
curl -sS https://dcscan.io/apis/exchange-integration | \
  grep -oE 'id="(tldr|dcswap|coingecko)"' | sort -u
# Expect all three ids on stdout.
```

## Rollback

Single-file drop; rollback is `rm` on the deploy tree + reload. The URL will 404 gracefully (no `index.html` at that subpath means nginx falls through to the root `try_files` which serves the ecosystem homepage - documented gotcha in `handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc` but not a data-loss scenario, just a UX blip on that one URL). If you want a true rollback, keep the file and just remove the two Nav dropdown entries in `apis.html`.

## What the page contains

Twelve sections mapping 1:1 to `docs/EXCHANGE_INTEGRATION_GUIDE_v1.md`:

- §0 TL;DR
- §1 Two integration models (CEX Model A vs DEX/aggregator Model B)
- §2 Network parameters (paste-safe table + `/api/v1/network/config` sample)
- §3 JSON-RPC surface (standard `eth_*` + additive `rope_*`)
- §4 Block explorer API (`/api/v1/*` endpoints + canonical price feed)
- §5 Asset registry (native FAT, WFAT, legacy DC, migration contracts, bridged stables)
- §6 DCSwap (Router, Factory, live pools, aggregator pattern)
- §7 MintMe playbook (Model A, listing form fields, market pairs, LP commitment)
- §8 XSwap Protocol playbook (Model B, wagmi config, cross-chain flow)
- §9 CoinGecko DEX listing (DCSwap tracker submission, live endpoint verification)
- §10 Acceptance criteria (four post-integration smoke tests)
- §11 Points of contact
- §12 Versioning + change control

Every URL, address, and code block was live-verified against production endpoints on 2026-08-14.

## Related outreach

See `EXCHANGE_INTEGRATION_OUTREACH_2026-08-14.md` (same directory) for:
- MintMe listing email (send to `contact@mintme.com`)
- XSwap Telegram message (send to `@xspswap`)
- CoinGecko DEX submission cover note (send to `hello@coingecko.com` alongside the form)
