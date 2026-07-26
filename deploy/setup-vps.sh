#!/bin/bash
# =============================================================================
# Datachain Rope VPS Setup Script — Gandi-hosted node baseline
# (rope-vps / anvil-vps / dcswap-prod pattern — see SECURITY_POLICY.md §1, §5)
# Run this ONCE after first SSH connection, on a FRESH Ubuntu box, as a human
# operator sitting at an interactive terminal. Do NOT pipe this into an
# unattended CI runner — step 6 below deliberately pauses for a manual
# out-of-band verification before the real SSH port is cut over, per
# SECURITY_POLICY.md §6 "Port Change Procedure (CRITICAL)".
#
# M7 (2026-07-25 security audit): this script previously ran `ufw allow ssh`
# (= port 22) and never touched sshd_config, fail2ban jails, endlessh, or
# CrowdSec. That is NOT what production actually runs — every Gandi-hosted
# node's real sshd listens on port 41722, while port 22 (and 2222) run an
# endlessh tarpit decoy (SECURITY_POLICY.md §2 "Layer 5", §7 "Port ownership
# — rope-vps"). A fresh VPS provisioned from the OLD version of this script
# would boot with the real sshd still on port 22 (world-exposed, no tarpit,
# no jail tuned for the tarpit-vs-real-SSH split) — a materially different,
# weaker posture than every node this script is supposed to match. This
# revision implements the actual topology end-to-end and idempotently.
# =============================================================================

set -euo pipefail

# ---- Configuration (override via environment before running if needed) ----
REAL_SSH_PORT="${REAL_SSH_PORT:-41722}"
TARPIT_PORT_PRIMARY="${TARPIT_PORT_PRIMARY:-22}"
TARPIT_PORT_SECONDARY="${TARPIT_PORT_SECONDARY:-2222}"
CROWDSEC_LAPI_PORT="${CROWDSEC_LAPI_PORT:-8180}"
VPS_LABEL="${VPS_LABEL:-$(hostname)}"

echo "╔════════════════════════════════════════════════════════════════╗"
echo "║       DATACHAIN ROPE - VPS PRODUCTION SETUP                    ║"
echo "║       Host: ${VPS_LABEL}"
echo "╚════════════════════════════════════════════════════════════════╝"

# -----------------------------------------------------------------------------
# 1. System update + base packages (added: endlessh, jq, nftables/iptables
#    prerequisites for CrowdSec's bouncer)
# -----------------------------------------------------------------------------
echo "📦 Updating system packages..."
sudo apt update && sudo apt upgrade -y

echo "📦 Installing dependencies..."
sudo apt install -y \
    curl \
    wget \
    git \
    build-essential \
    pkg-config \
    libssl-dev \
    clang \
    htop \
    tmux \
    fail2ban \
    ufw \
    jq \
    endlessh

# -----------------------------------------------------------------------------
# 2. Docker + Docker Compose (unchanged)
# -----------------------------------------------------------------------------
if ! command -v docker >/dev/null 2>&1; then
    echo "🐳 Installing Docker..."
    curl -fsSL https://get.docker.com | sudo sh
    sudo usermod -aG docker "$USER"
else
    echo "🐳 Docker already installed, skipping."
fi

if ! command -v docker-compose >/dev/null 2>&1; then
    echo "🐳 Installing Docker Compose..."
    sudo curl -L "https://github.com/docker/compose/releases/latest/download/docker-compose-$(uname -s)-$(uname -m)" -o /usr/local/bin/docker-compose
    sudo chmod +x /usr/local/bin/docker-compose
else
    echo "🐳 Docker Compose already installed, skipping."
fi

# -----------------------------------------------------------------------------
# 3. Rust (unchanged)
# -----------------------------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
    echo "🦀 Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi
# shellcheck disable=SC1090
source "$HOME/.cargo/env" 2>/dev/null || true

# -----------------------------------------------------------------------------
# 4. SSH hardening — write the drop-in config for the REAL sshd port, but do
#    NOT restart sshd yet. We stage the config, syntax-check it, and only
#    apply it after step 6's explicit human confirmation, per
#    SECURITY_POLICY.md §6 "Port Change Procedure (CRITICAL)":
#      1. UFW allow NEW_PORT/tcp        <- done in step 5 below
#      2. Test: verify NEW_PORT reachable from OUTSIDE (operator does this
#         manually in a second terminal — a script on the box cannot prove
#         external reachability of itself)
#      3. Change service config to NEW_PORT   <- staged here, applied in step 6
#      4. Restart service                     <- step 6
#      5. Test: verify service works on NEW_PORT   <- operator, step 6
#      6. UFW delete allow OLD_PORT/tcp   <- N/A here: old port 22 is
#         repurposed to the endlessh tarpit, never fully closed
#      7. Update ~/.ssh/config on admin machines   <- operator follow-up
#      8. Update fail2ban jail port setting    <- step 8 below (pre-staged)
# -----------------------------------------------------------------------------
echo "🔐 Staging SSH hardening (port ${REAL_SSH_PORT}, no restart yet)..."
SSHD_DROPIN="/etc/ssh/sshd_config.d/99-datachain-rope-hardened.conf"
sudo mkdir -p /etc/ssh/sshd_config.d
sudo tee "$SSHD_DROPIN" >/dev/null <<EOF
# Managed by datachain-rope/deploy/setup-vps.sh — do not hand-edit without
# updating that script too. See SECURITY_POLICY.md §2 "Layer 2" + §6.
Port ${REAL_SSH_PORT}
PermitRootLogin no
PasswordAuthentication no
MaxAuthTries 3
EOF
sudo chmod 644 "$SSHD_DROPIN"

echo "🔎 Syntax-checking staged sshd config..."
sudo sshd -t
echo "✅ sshd config syntax OK. Not yet active — applied in step 6."

# -----------------------------------------------------------------------------
# 5. Firewall — open the REAL ssh port, the two tarpit ports, and the actual
#    public service ports. Nothing else. Internal-only ports (8595 Reth,
#    9090 metrics, 8180 CrowdSec LAPI, 5432/6379 datastores) are deliberately
#    NOT opened here — they stay 127.0.0.1-only per SECURITY_POLICY.md §7.
# -----------------------------------------------------------------------------
echo "🔥 Configuring firewall (real topology: ${REAL_SSH_PORT}=ssh, ${TARPIT_PORT_PRIMARY}/${TARPIT_PORT_SECONDARY}=tarpit)..."
sudo ufw default deny incoming
sudo ufw default allow outgoing

# Real SSH — rate-limited natively by ufw (denies an IP after 6 connection
# attempts within 30s), which is the concrete mechanism behind
# SECURITY_POLICY.md §3's "DDoS jail: ban after 6 attempts in 60 seconds".
sudo ufw limit "${REAL_SSH_PORT}"/tcp comment 'real sshd (rate-limited)'

# Tarpit ports stay open to the internet on purpose — that is the point of a
# decoy. They are NOT the real sshd.
sudo ufw allow "${TARPIT_PORT_PRIMARY}"/tcp comment 'endlessh tarpit (decoy, not real ssh)'
sudo ufw allow "${TARPIT_PORT_SECONDARY}"/tcp comment 'endlessh tarpit (decoy, not real ssh)'

sudo ufw allow 80/tcp    comment 'HTTP (nginx, redirects to 443)'
sudo ufw allow 443/tcp   comment 'HTTPS (nginx public edge)'
sudo ufw allow 9000/tcp  comment 'libp2p P2P'

sudo ufw --force enable
sudo ufw status verbose || true

# -----------------------------------------------------------------------------
# 6. CUTOVER — the one genuinely irreversible-if-botched step. Refuses to
#    proceed without an explicit operator confirmation that they have a
#    SECOND, independent path to the box (e.g. cloud provider web console,
#    or an already-open session) before the real sshd moves off port 22.
# -----------------------------------------------------------------------------
echo ""
echo "╔════════════════════════════════════════════════════════════════╗"
echo "║  ⚠️  SSH PORT CUTOVER — READ CAREFULLY                          ║"
echo "╚════════════════════════════════════════════════════════════════╝"
echo "About to restart sshd with:"
echo "  - real SSH moving to port ${REAL_SSH_PORT}"
echo "  - PermitRootLogin no, PasswordAuthentication no, MaxAuthTries 3"
echo ""
echo "BEFORE continuing, in a SEPARATE window/tab, confirm you can reach:"
echo "    ssh -p ${REAL_SSH_PORT} -o ConnectTimeout=5 \$(whoami)@\$(curl -s ifconfig.me) exit"
echo "and that it prompts for your key (not 'connection refused')."
echo "This works because ufw already opened ${REAL_SSH_PORT} in step 5, even"
echo "though sshd hasn't switched to it yet — 'connection refused' vs"
echo "'no route'/timeout tells you whether the firewall path is open."
echo ""
echo "DO NOT continue if you only have this one session open."
echo ""
read -r -p "Type EXACTLY 'I HAVE A SECOND SESSION OPEN' to proceed: " CONFIRM
if [ "$CONFIRM" != "I HAVE A SECOND SESSION OPEN" ]; then
    echo "❌ Confirmation not given. Aborting before touching sshd."
    echo "   Firewall + staged config are in place; re-run this script to retry."
    exit 1
fi

echo "🔐 Restarting sshd on port ${REAL_SSH_PORT}..."
sudo systemctl restart ssh || sudo systemctl restart sshd
sleep 2
if sudo ss -tlnp | grep -q ":${REAL_SSH_PORT} "; then
    echo "✅ sshd is listening on ${REAL_SSH_PORT}."
else
    echo "❌ sshd does NOT appear to be listening on ${REAL_SSH_PORT}. Check"
    echo "   'sudo systemctl status ssh' and 'sudo journalctl -u ssh -n 50' NOW,"
    echo "   from your second session, before disconnecting this one."
    exit 1
fi

# -----------------------------------------------------------------------------
# 7. endlessh tarpit — two independent instances (templated unit), one per
#    decoy port, per SECURITY_POLICY.md §2 "Layer 5" ("Runs on port 22 and
#    2222"). The stock Debian/Ubuntu `endlessh` package ships a single
#    Port directive in /etc/endlessh/config; we template it so each decoy
#    port gets its own config + systemd instance instead of fighting over
#    one listener.
# -----------------------------------------------------------------------------
echo "🐌 Configuring endlessh tarpit on ports ${TARPIT_PORT_PRIMARY} and ${TARPIT_PORT_SECONDARY}..."
sudo systemctl stop endlessh 2>/dev/null || true
sudo systemctl disable endlessh 2>/dev/null || true

sudo mkdir -p /etc/endlessh
for PORT in "${TARPIT_PORT_PRIMARY}" "${TARPIT_PORT_SECONDARY}"; do
    sudo tee "/etc/endlessh/config-${PORT}" >/dev/null <<EOF
# Managed by datachain-rope/deploy/setup-vps.sh
Port ${PORT}
Bind 0.0.0.0
MaxLineLength 32
Delay 10000
MaxClients 4096
LogLevel 1
EOF
done

sudo tee /etc/systemd/system/endlessh@.service >/dev/null <<'EOF'
[Unit]
Description=endlessh SSH tarpit decoy (instance: port %i)
Documentation=https://github.com/skeeto/endlessh
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/endlessh -f /etc/endlessh/config-%i
Restart=always
RestartSec=5
DynamicUser=yes
AmbientCapabilities=CAP_NET_BIND_SERVICE
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictAddressFamilies=AF_INET AF_INET6
RestrictNamespaces=yes
RestrictSUIDSGID=yes
LockPersonality=yes
RestrictRealtime=yes
RemoveIPC=yes

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now "endlessh@${TARPIT_PORT_PRIMARY}.service"
sudo systemctl enable --now "endlessh@${TARPIT_PORT_SECONDARY}.service"
echo "✅ endlessh tarpit active on ${TARPIT_PORT_PRIMARY} and ${TARPIT_PORT_SECONDARY}."

# -----------------------------------------------------------------------------
# 8. Fail2ban — SSH jail tuned to the REAL port (per SECURITY_POLICY.md §3:
#    "SSH jail: ban after 3 failed attempts for 24 hours") plus the stock
#    `recidive` jail as the persistent-repeat-offender backstop (the closest
#    real, functional fail2ban primitive to "DDoS jail... for 7 days" — the
#    6-attempts/60-seconds tier is handled by `ufw limit` in step 5, which is
#    a genuine kernel-level rate limiter rather than a fabricated log-regex
#    jail we can't verify against real auth.log formats).
# -----------------------------------------------------------------------------
echo "🔒 Configuring fail2ban jails..."
sudo tee /etc/fail2ban/jail.d/99-datachain-rope.conf >/dev/null <<EOF
# Managed by datachain-rope/deploy/setup-vps.sh — see SECURITY_POLICY.md §3.

[sshd]
enabled = true
port    = ${REAL_SSH_PORT}
backend = systemd
maxretry = 3
bantime  = 24h
findtime = 10m

# Persistent-repeat-offender backstop across all jails (closest real
# fail2ban primitive to a "7 day DDoS ban" — see comment above).
[recidive]
enabled  = true
logpath  = %(fail2ban_log)s
banaction = %(banaction_allports)s
bantime  = 7d
findtime = 1d
maxretry = 3

# Nginx jails (HTTP auth / rate-limit / bot-search) are intentionally NOT
# defined here: nginx runs inside the `rope-nginx` Docker container per
# deploy/docker-compose.yml, and a host-level fail2ban filter needs the
# container's access/error logs bind-mounted to a host path first. Wire
# those mounts in the nginx compose/deploy step, confirm the log path is
# populated, THEN add [nginx-http-auth]/[nginx-limit-req]/[nginx-botsearch]
# jails pointed at it — do not enable a jail against a log path that may
# not exist, that silently does nothing and gives a false sense of coverage.
EOF

sudo systemctl enable fail2ban
sudo systemctl restart fail2ban
echo "✅ fail2ban jails applied (sshd:${REAL_SSH_PORT}, recidive)."

# -----------------------------------------------------------------------------
# 9. CrowdSec — community threat-intel + local firewall bouncer, LAPI bound
#    to 127.0.0.1:8180 (SECURITY_POLICY.md §2 "Layer 4": "avoids IPFS
#    conflict on 8080"). Installed idempotently; skipped if already present.
# -----------------------------------------------------------------------------
if ! command -v cscli >/dev/null 2>&1; then
    echo "🛡️  Installing CrowdSec..."
    curl -fsSL https://packagecloud.io/install/repositories/crowdsec/crowdsec/script.deb.sh | sudo bash
    sudo apt install -y crowdsec crowdsec-firewall-bouncer-iptables
else
    echo "🛡️  CrowdSec already installed, skipping install."
fi

CROWDSEC_CFG="/etc/crowdsec/config.yaml"
if [ -f "$CROWDSEC_CFG" ] && ! grep -q "127.0.0.1:${CROWDSEC_LAPI_PORT}" "$CROWDSEC_CFG"; then
    echo "🛡️  Rebinding CrowdSec LAPI to 127.0.0.1:${CROWDSEC_LAPI_PORT}..."
    sudo sed -i "s#listen_uri:.*#listen_uri: 127.0.0.1:${CROWDSEC_LAPI_PORT}#" "$CROWDSEC_CFG"
    sudo systemctl restart crowdsec 2>/dev/null || true
fi
sudo systemctl enable crowdsec 2>/dev/null || true
sudo systemctl enable crowdsec-firewall-bouncer 2>/dev/null || true
echo "✅ CrowdSec configured (LAPI on 127.0.0.1:${CROWDSEC_LAPI_PORT})."

# -----------------------------------------------------------------------------
# 10. Directories + repo clone + build (unchanged from prior script)
# -----------------------------------------------------------------------------
echo "📁 Creating directories..."
sudo mkdir -p /opt/datachain-rope
sudo mkdir -p /opt/datachain-rope/ssl
sudo mkdir -p /opt/datachain-rope/data
sudo mkdir -p /opt/datachain-rope/logs
sudo chown -R "$USER:$USER" /opt/datachain-rope

echo "📥 Cloning Datachain Rope..."
cd /opt/datachain-rope
if [ ! -d code ]; then
    git clone https://github.com/KazeONGUENE/rope.git code
fi
cd code

echo "🔨 Building Datachain Rope..."
cargo build --release

echo ""
echo "╔════════════════════════════════════════════════════════════════╗"
echo "║  ✅ SETUP COMPLETE!                                            ║"
echo "║                                                                ║"
echo "║  Real SSH is now on port ${REAL_SSH_PORT} — update ~/.ssh/config       ║"
echo "║  on every admin machine (SECURITY_POLICY.md §6 step 7) BEFORE  ║"
echo "║  closing this session:                                         ║"
echo "║    Host <alias>                                                ║"
echo "║      HostName <this-vps-ip>                                    ║"
echo "║      Port ${REAL_SSH_PORT}                                              ║"
echo "║      User $USER                                             ║"
echo "║                                                                 ║"
echo "║  Next steps:                                                   ║"
echo "║  1. Log out and log back in (for Docker group)                 ║"
echo "║  2. Copy SSL certificates to /opt/datachain-rope/ssl/           ║"
echo "║  3. Copy .env file to /opt/datachain-rope/code/deploy/          ║"
echo "║  4. Run: cd /opt/datachain-rope/code/deploy                    ║"
echo "║  5. Run: docker-compose up -d                                  ║"
echo "║  6. Verify: cscli metrics ; sudo fail2ban-client status         ║"
echo "╚════════════════════════════════════════════════════════════════╝"
