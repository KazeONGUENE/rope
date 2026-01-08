#!/bin/bash
# =============================================================================
# Datachain Rope - SSL Certificate Setup
# This script sets up the SSL certificates from the provided Gandi certs
# Run this AFTER uploading certificates to the VPS
# =============================================================================

SSL_DIR="/opt/datachain-rope/ssl"

echo "╔════════════════════════════════════════════════════════════════╗"
echo "║       DATACHAIN ROPE - SSL CERTIFICATE SETUP                   ║"
echo "╚════════════════════════════════════════════════════════════════╝"

# Create directories
sudo mkdir -p $SSL_DIR/datachain.network
sudo mkdir -p $SSL_DIR/rope.network
sudo mkdir -p $SSL_DIR/dcscan.io

echo ""
echo "📋 Instructions:"
echo ""
echo "1. Create certificate files for each domain:"
echo ""
echo "   For datachain.network:"
echo "   - $SSL_DIR/datachain.network/fullchain.pem (cert + CA chain)"
echo "   - $SSL_DIR/datachain.network/privkey.pem (private key)"
echo ""
echo "   For rope.network:"
echo "   - $SSL_DIR/rope.network/fullchain.pem"
echo "   - $SSL_DIR/rope.network/privkey.pem"
echo ""
echo "   For dcscan.io:"
echo "   - $SSL_DIR/dcscan.io/fullchain.pem"
echo "   - $SSL_DIR/dcscan.io/privkey.pem"
echo ""
echo "2. Set proper permissions:"
echo "   sudo chmod 600 $SSL_DIR/*/privkey.pem"
echo "   sudo chmod 644 $SSL_DIR/*/fullchain.pem"
echo ""
echo "3. To create fullchain.pem, concatenate:"
echo "   cat domain_cert.pem intermediate_ca.pem > fullchain.pem"
echo ""

# Set permissions
sudo chown -R root:root $SSL_DIR
sudo chmod 755 $SSL_DIR
sudo chmod 755 $SSL_DIR/*

echo "✅ SSL directories created. Please add your certificates."

