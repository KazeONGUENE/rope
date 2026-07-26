# Datachain Rope - Production Deployment Guide

## 🖥️ VPS Information

| Property | Value |
|----------|-------|
| **IP Address** | 92.243.26.189 |
| **OS** | Ubuntu 24.04 LTS |
| **Provider** | Gandi |
| **SSH Key** | DCRope_key |

## 🌐 Domains

| Domain | Purpose | SSL |
|--------|---------|-----|
| datachain.network | Main website + Primary RPC | ✅ Gandi |
| erpc.datachain.network | RPC Endpoint | ✅ Gandi |
| ws.datachain.network | WebSocket Endpoint | ✅ Gandi |
| rope.network | Secondary RPC | ✅ Gandi |
| erpc.rope.network | Secondary RPC Endpoint | ✅ Gandi |
| ws.rope.network | Secondary WebSocket | ✅ Gandi |
| dcscan.io | Block Explorer | ✅ Gandi |
| api.dcscan.io | Explorer API | ✅ Gandi |

## 🚀 Step-by-Step Deployment

### Step 1: Connect to VPS

```bash
# From your local machine
ssh -i ~/.ssh/DCRope_key ubuntu@92.243.26.189
```

### Step 2: Initial VPS Setup

```bash
# Upload and run setup script
scp -i ~/.ssh/DCRope_key deploy/setup-vps.sh ubuntu@92.243.26.189:/tmp/
ssh -i ~/.ssh/DCRope_key ubuntu@92.243.26.189 "chmod +x /tmp/setup-vps.sh && /tmp/setup-vps.sh"
```

### Step 3: Upload SSL Certificates

Create certificate files on the VPS:

```bash
# SSH into VPS
ssh -i ~/.ssh/DCRope_key ubuntu@92.243.26.189

# Create directories
sudo mkdir -p /opt/datachain-rope/ssl/datachain.network
sudo mkdir -p /opt/datachain-rope/ssl/rope.network
sudo mkdir -p /opt/datachain-rope/ssl/dcscan.io
```

For each domain, create the certificate files. The certificates you provided need to be split:

**datachain.network:**
```bash
# Private key
sudo nano /opt/datachain-rope/ssl/datachain.network/privkey.pem
# Paste the private key

# Full chain (domain cert + intermediate + root)
sudo nano /opt/datachain-rope/ssl/datachain.network/fullchain.pem
# Paste the certificate chain
```

**rope.network:**
```bash
sudo nano /opt/datachain-rope/ssl/rope.network/privkey.pem
sudo nano /opt/datachain-rope/ssl/rope.network/fullchain.pem
```

**dcscan.io:**
```bash
sudo nano /opt/datachain-rope/ssl/dcscan.io/privkey.pem
sudo nano /opt/datachain-rope/ssl/dcscan.io/fullchain.pem
```

Set permissions:
```bash
sudo chmod 600 /opt/datachain-rope/ssl/*/privkey.pem
sudo chmod 644 /opt/datachain-rope/ssl/*/fullchain.pem
```

### Step 4: Configure Environment

```bash
cd /opt/datachain-rope/code/deploy
cp env.production.example .env
nano .env
```

Fill in:
```env
POSTGRES_PASSWORD=<generate with: openssl rand -base64 32>
REDIS_PASSWORD=<generate with: openssl rand -base64 32>
# Use a least-privilege application role, never the *_owner role.
NEON_DATABASE_URL=postgresql://<app_role>:<password>@<your-neon-host>/<db>?sslmode=require&channel_binding=require
```

> **2026-07-25 security note:** an earlier revision of this document
> committed a live Neon `*_owner` connection string in plaintext. That
> credential must be treated as compromised — rotate the Neon password and
> switch to a scoped application role. See
> `docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md` finding C3.

### Step 5: Deploy

```bash
chmod +x deploy.sh
./deploy.sh
```

### Step 6: Verify Deployment

```bash
# Check all containers
docker-compose ps

# Check logs
docker-compose logs -f

# Test RPC
curl -X POST https://erpc.datachain.network \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rope_chainId","params":[],"id":1}'

# Test Explorer
curl https://dcscan.io
```

## 📊 Architecture

```
                            ┌─────────────────────────────────────┐
                            │            INTERNET                 │
                            └──────────────┬──────────────────────┘
                                           │
                    ┌──────────────────────┼──────────────────────┐
                    │                      │                      │
                    ▼                      ▼                      ▼
        datachain.network          rope.network             dcscan.io
        erpc.datachain.network     erpc.rope.network        api.dcscan.io
        ws.datachain.network       ws.rope.network
                    │                      │                      │
                    └──────────────────────┼──────────────────────┘
                                           │
                                           ▼
                    ┌──────────────────────────────────────────────┐
                    │                  NGINX                        │
                    │           (SSL Termination)                   │
                    │              Port 80/443                      │
                    └──────────────────────┬───────────────────────┘
                                           │
              ┌────────────────────────────┼────────────────────────┐
              │                            │                        │
              ▼                            ▼                        ▼
    ┌─────────────────┐         ┌─────────────────┐      ┌─────────────────┐
    │   ROPE NODE     │         │  DC EXPLORER    │      │    INDEXER      │
    │                 │         │                 │      │                 │
    │  RPC: 8545      │◄────────┤   Port: 3000    │      │   (Background)  │
    │  WS:  8546      │         │                 │      │                 │
    │  P2P: 9000      │         └────────┬────────┘      └────────┬────────┘
    │                 │                  │                        │
    │  ┌───────────┐  │                  │                        │
    │  │  RocksDB  │  │                  │                        │
    │  │ (embedded)│  │                  │                        │
    │  └───────────┘  │                  │                        │
    └─────────────────┘                  │                        │
                                         ▼                        ▼
                    ┌──────────────────────────────────────────────┐
                    │               POSTGRESQL                      │
                    │          (Explorer Database)                  │
                    │              Port: 5432                       │
                    └──────────────────────────────────────────────┘
                                         │
                                         ▼
                    ┌──────────────────────────────────────────────┐
                    │                 REDIS                         │
                    │              (Cache Layer)                    │
                    │              Port: 6379                       │
                    └──────────────────────────────────────────────┘
                                         │
                                         ▼
                    ┌──────────────────────────────────────────────┐
                    │           NEON POSTGRESQL                     │
                    │         (Cloud Backup/Sync)                   │
                    └──────────────────────────────────────────────┘
```

## 🔧 Maintenance Commands

```bash
# View logs
docker-compose logs -f rope-node
docker-compose logs -f dc-explorer

# Restart a service
docker-compose restart rope-node

# Update deployment
cd /opt/datachain-rope/code
git pull
cd deploy
./deploy.sh

# Database backup
docker exec rope-postgres pg_dump -U dcscan dcscan > backup.sql

# Check disk usage
df -h
docker system df

# Clean up
docker system prune -a
```

## 🔐 Security Notes

1. **Firewall**: Only ports 80, 443, and 9000 are exposed
2. **SSH**: Key-based auth only (password disabled)
3. **Fail2ban**: Enabled for SSH protection
4. **SSL**: TLS 1.2/1.3 only, strong ciphers
5. **Rate limiting**: Applied to RPC endpoints

## 📈 Monitoring

- **Prometheus**: http://localhost:9090/metrics (internal only)
- **Logs**: `/opt/datachain-rope/logs/`
- **Docker stats**: `docker stats`

## 🆘 Troubleshooting

### Container won't start
```bash
docker-compose logs <container_name>
docker-compose down
docker-compose up -d
```

### SSL issues
```bash
# Test certificate
openssl s_client -connect datachain.network:443 -servername datachain.network
```

### Database issues
```bash
docker exec -it rope-postgres psql -U dcscan -d dcscan
```

### Network issues
```bash
# Check if ports are open
sudo netstat -tlnp
```

## 📞 Support

- **GitHub**: https://github.com/KazeONGUENE/rope
- **Email**: contact@datachain.network

