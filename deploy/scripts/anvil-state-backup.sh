#!/bin/bash
# =============================================================================
# anvil-state-backup.sh — Periodic Anvil state snapshots
# =============================================================================
#
# Install as a cron job on the VPS:
#   crontab -e
#   0 */4 * * * /opt/datachain-rope/scripts/anvil-state-backup.sh
#
# This creates timestamped snapshots of the Anvil state file every 4 hours.
# Keeps the last 7 days of snapshots (42 files at 4h intervals).
# =============================================================================

set -euo pipefail

STATE_FILE="/opt/datachain-rope/anvil-state/state.json"
BACKUP_DIR="/opt/datachain-rope/anvil-state/backups"
MAX_AGE_DAYS=7

mkdir -p "$BACKUP_DIR"

if [ ! -f "$STATE_FILE" ]; then
    echo "[anvil-backup] No state file at $STATE_FILE — skipping"
    exit 0
fi

SIZE=$(stat -c%s "$STATE_FILE" 2>/dev/null || stat -f%z "$STATE_FILE" 2>/dev/null)
if [ "$SIZE" -lt 100 ]; then
    echo "[anvil-backup] State file is suspiciously small ($SIZE bytes) — skipping"
    exit 1
fi

TIMESTAMP=$(date +%Y%m%d-%H%M%S)
BACKUP_FILE="$BACKUP_DIR/state-$TIMESTAMP.json"

cp "$STATE_FILE" "$BACKUP_FILE"
gzip "$BACKUP_FILE" 2>/dev/null || true

echo "[anvil-backup] Snapshot saved: $BACKUP_FILE.gz ($SIZE bytes)"

# Prune old backups
find "$BACKUP_DIR" -name "state-*.json*" -mtime +$MAX_AGE_DAYS -delete 2>/dev/null || true

REMAINING=$(ls -1 "$BACKUP_DIR"/state-*.json* 2>/dev/null | wc -l)
echo "[anvil-backup] $REMAINING snapshots retained (max age: ${MAX_AGE_DAYS}d)"
