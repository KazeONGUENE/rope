#!/bin/bash
# =============================================================================
# DC Explorer — static frontend deploy (dcscan.io)
# =============================================================================
#
# TOPOLOGY GOTCHA (read this before editing dcscan.io HTML/CSS/JS):
#
#   dcscan.io is served by the `rope-nginx` DOCKER container directly from
#     deploy/nginx/html/dcscan/          <-- THIS is the live source of truth
#   Nginx only proxies `/api/*` to the dc-explorer Rust binary; every other
#   path (/, /supply, /txs, /address/*, ...) is served as a static file
#   straight off disk, completely bypassing dc-explorer.
#
#   crates/rope-explorer/static/ is a *separate* tree used only by
#   dc-explorer's own bundled/fallback static server (DCSCAN_STATIC env var).
#   It is NOT what nginx serves for dcscan.io's HTML pages in production.
#   Editing it will build/compile fine and feel like it "worked" (dc-explorer
#   itself will happily serve it on its own port), but the public site will
#   not change. This exact mistake shipped a broken fix silently for weeks
#   (2026-07-24 incident: "Legacy DC Remaining" + migration tx categorization
#   fixes landed in crates/rope-explorer/static/ only, never went live).
#
#   ==> If your change should show up on https://dcscan.io, edit files under
#       deploy/nginx/html/dcscan/ and deploy with THIS script.
#   ==> If you're not sure which tree a page lives in, run this script with
#       --dry-run first — the diff output tells you exactly what's about to
#       change on production before anything is touched.
#
# Usage:
#   ./deploy/deploy-static.sh --dry-run     # preview only, no changes made
#   ./deploy/deploy-static.sh --apply       # sync + reload nginx
#
# =============================================================================

set -euo pipefail

SERVER="rope-vps"   # resolved via ~/.ssh/config (92.243.26.189)
REMOTE_PATH="/opt/datachain-rope/code/deploy/nginx/html/dcscan"
LOCAL_PATH="$(cd "$(dirname "$0")/nginx/html/dcscan" && pwd)"

MODE="${1:---dry-run}"

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║  DC EXPLORER STATIC DEPLOY (dcscan.io)                        ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo "Local:  $LOCAL_PATH"
echo "Remote: $SERVER:$REMOTE_PATH"
echo ""

case "$MODE" in
  --dry-run)
    echo "--- DRY RUN: showing what would change on production (no changes made) ---"
    rsync -avzn --delete "$LOCAL_PATH/" "$SERVER:$REMOTE_PATH/"
    echo ""
    echo "Nothing was changed. Re-run with --apply to push these changes live."
    ;;
  --apply)
    echo "--- Step 1/3: syncing $LOCAL_PATH -> $SERVER:$REMOTE_PATH ---"
    rsync -avz --delete "$LOCAL_PATH/" "$SERVER:$REMOTE_PATH/"

    echo ""
    echo "--- Step 2/3: reloading nginx (rope-nginx container) ---"
    ssh "$SERVER" "docker exec rope-nginx nginx -s reload"

    echo ""
    echo "--- Step 3/3: smoke-testing https://dcscan.io ---"
    sleep 1
    curl -sS -o /dev/null -w "https://dcscan.io/  -> HTTP %{http_code}\n" https://dcscan.io/
    curl -sS -o /dev/null -w "https://dcscan.io/supply -> HTTP %{http_code}\n" https://dcscan.io/supply

    echo ""
    echo "Done. Static frontend deployed and nginx reloaded."
    ;;
  *)
    echo "Unknown mode: $MODE"
    echo "Usage: $0 [--dry-run|--apply]"
    exit 1
    ;;
esac
