#!/bin/bash
# issue-testnet-cert.sh - run this ON rope-testnet-1, ONLY AFTER
# testnet.erpc.datachain.network AND faucet.datachain.network A records
# have been repointed to this hosts public IPv4 (159.65.82.207) and
# propagated.
#
# Idempotent. If a cert already exists for these hostnames, certbot
# updates it in place.
#
# What it does:
#   1. Sanity-check that both hostnames actually resolve to us.
#   2. Sanity-check that the ACME HTTP-01 webroot is reachable from
#      the outside (writes a probe, hits it, removes it).
#   3. Run certbot in --nginx mode: it will rewrite the ssl_certificate
#      lines in the two sites-available files to point at
#      /etc/letsencrypt/live/testnet.erpc.datachain.network/{fullchain,privkey}.pem
#      (the primary cert covers both SANs).
#   4. `nginx -t && systemctl reload nginx` (certbot does this).
#   5. Print the new cert fingerprint + validity window.

set -euo pipefail

MYIP=$(curl -s https://ifconfig.co)
echo "This host public IPv4: $MYIP"

for host in testnet.erpc.datachain.network faucet.datachain.network; do
    RESOLVED=$(getent hosts "$host" | awk "{print \$1}" | head -1)
    echo "  $host resolves to: ${RESOLVED:-none}"
    if [[ "$RESOLVED" != "$MYIP" ]]; then
        echo "  ERROR: DNS for $host does not point at $MYIP yet."
        echo "  Update the A record at Gandi (TTL 300) and re-run this script."
        exit 1
    fi
done

echo
echo "=== ACME webroot self-check ==="
TESTFILE=$(mktemp -u XXXXXXXXXX)
sudo mkdir -p /var/www/acme-challenge/.well-known/acme-challenge
echo "acme-probe-$$" | sudo tee "/var/www/acme-challenge/.well-known/acme-challenge/$TESTFILE" >/dev/null
if curl -sf "http://testnet.erpc.datachain.network/.well-known/acme-challenge/$TESTFILE" >/dev/null; then
    echo "  ACME challenge webroot reachable from testnet.erpc.datachain.network"
else
    echo "  ERROR: cannot reach /.well-known/acme-challenge/ from testnet.erpc.datachain.network"
    sudo rm -f "/var/www/acme-challenge/.well-known/acme-challenge/$TESTFILE"
    exit 1
fi
if curl -sf "http://faucet.datachain.network/.well-known/acme-challenge/$TESTFILE" >/dev/null; then
    echo "  ACME challenge webroot reachable from faucet.datachain.network"
else
    echo "  ERROR: cannot reach /.well-known/acme-challenge/ from faucet.datachain.network"
    sudo rm -f "/var/www/acme-challenge/.well-known/acme-challenge/$TESTFILE"
    exit 1
fi
sudo rm -f "/var/www/acme-challenge/.well-known/acme-challenge/$TESTFILE"

echo
echo "=== issue certificate ==="
sudo certbot --nginx \
    --non-interactive \
    --agree-tos \
    -m contact@onguene.com \
    -d testnet.erpc.datachain.network \
    -d faucet.datachain.network \
    --redirect \
    --keep-until-expiring

echo
echo "=== resulting cert ==="
sudo certbot certificates 2>&1 | grep -A6 "testnet.erpc.datachain.network"

echo
echo "=== nginx reload verification ==="
sudo nginx -t
sudo systemctl reload nginx
sudo systemctl status nginx.service --no-pager | head -6
