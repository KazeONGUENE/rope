#!/bin/bash
# =============================================================================
# Datachain Rope - SSL Certificate Installation Script
#
# SECURITY NOTE (2026-07-25 remediation, finding C1 of
# docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md):
#   This script previously embedded the plaintext PEM private keys for
#   datachain.network, rope.network, and dcscan.io directly in this file,
#   which is tracked by git and pushed to a public GitHub repository. That
#   was a critical secret-exposure vulnerability: anyone who ever cloned
#   the repo had full impersonation capability for all three domains.
#
#   This script no longer contains, generates, or transports any private
#   key material. It only VALIDATES and INSTALLS certificate material that
#   already exists on the target host, staged out-of-band (scp/rsync
#   directly to the host, never through git) at $SSL_INBOX_DIR below.
#
#   Renewal workflow: Gandi-issued certs are renewed against the SAME
#   private key (never regenerate the key on renewal) unless you are
#   deliberately rotating compromised key material, in which case generate
#   a fresh keypair + CSR on the target host itself
#   ("openssl req -newkey rsa:2048 -nodes -keyout privkey.pem -out req.csr"),
#   submit the CSR through the Gandi certificate portal/API, and stage only
#   the returned fullchain.pem plus your already-local privkey.pem below.
# =============================================================================

set -euo pipefail

SSL_DIR="/opt/datachain-rope/ssl"
# Secure staging area. Populate out-of-band (scp/rsync from an operator
# workstation, or from a secrets manager) — never via git, never inline in
# this script. Each domain subdirectory must contain privkey.pem (mode 600)
# and fullchain.pem before running this script.
SSL_INBOX_DIR="${SSL_INBOX_DIR:-/opt/datachain-rope/ssl-inbox}"

DOMAINS=(datachain.network rope.network dcscan.io)

echo "╔════════════════════════════════════════════════════════════════╗"
echo "║       INSTALLING SSL CERTIFICATES                              ║"
echo "╚════════════════════════════════════════════════════════════════╝"

for domain in "${DOMAINS[@]}"; do
  privkey="$SSL_INBOX_DIR/$domain/privkey.pem"
  fullchain="$SSL_INBOX_DIR/$domain/fullchain.pem"

  if [[ ! -f "$privkey" || ! -f "$fullchain" ]]; then
    echo "❌ Missing staged material for $domain — expected:"
    echo "     $privkey"
    echo "     $fullchain"
    echo "   Stage both files (out-of-band, never via git) before re-running."
    exit 1
  fi

  echo "📜 Validating $domain certificate/key pair..."

  # Confirm the private key and certificate are a matching pair before
  # installing anything, so a mismatched or corrupted staged pair never
  # silently breaks TLS termination.
  key_modulus=$(openssl rsa -noout -modulus -in "$privkey" 2>/dev/null | openssl sha256)
  cert_modulus=$(openssl x509 -noout -modulus -in "$fullchain" 2>/dev/null | openssl sha256)
  if [[ "$key_modulus" != "$cert_modulus" ]]; then
    echo "❌ Private key does not match certificate for $domain — refusing to install."
    exit 1
  fi

  not_after=$(openssl x509 -noout -enddate -in "$fullchain" | cut -d= -f2)
  echo "   ✓ key/cert pair matches. Expires: $not_after"

  sudo mkdir -p "$SSL_DIR/$domain"
  sudo install -m 600 -o root -g root "$privkey" "$SSL_DIR/$domain/privkey.pem"
  sudo install -m 644 -o root -g root "$fullchain" "$SSL_DIR/$domain/fullchain.pem"

  echo "   ✓ installed to $SSL_DIR/$domain/"
done

sudo chmod 755 "$SSL_DIR"
sudo chmod 755 "$SSL_DIR"/*

echo ""
echo "✅ SSL certificates installed successfully!"
echo ""
echo "Certificates installed:"
ls -la "$SSL_DIR"/*/
