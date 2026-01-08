# Datachain Rope Network Architecture

## Domain Structure

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        DATACHAIN ROPE ECOSYSTEM                              │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │              datachain.network (ROPE NETWORK LAYER)                  │    │
│  │              ════════════════════════════════════════                │    │
│  │                                                                      │    │
│  │   ┌──────────────────────────────────────────────────────────────┐  │    │
│  │   │                    CORE PROTOCOL                              │  │    │
│  │   │                                                               │  │    │
│  │   │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐           │  │    │
│  │   │  │ Post-Quantum│  │  Virtual    │  │ Reed-Solomon│           │  │    │
│  │   │  │   Crypto    │  │  Voting     │  │  Erasure    │           │  │    │
│  │   │  │ (Dilithium3 │  │ (Appendix   │  │  Coding     │           │  │    │
│  │   │  │  + Kyber768)│  │   B.1)      │  │             │           │  │    │
│  │   │  └─────────────┘  └─────────────┘  └─────────────┘           │  │    │
│  │   │                                                               │  │    │
│  │   │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐           │  │    │
│  │   │  │  libp2p     │  │  Swarm      │  │  OES        │           │  │    │
│  │   │  │  Transport  │  │  Runtime    │  │  Crypto     │           │  │    │
│  │   │  │ (QUIC+TCP)  │  │             │  │             │           │  │    │
│  │   │  └─────────────┘  └─────────────┘  └─────────────┘           │  │    │
│  │   │                                                               │  │    │
│  │   │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐           │  │    │
│  │   │  │ Testimony   │  │ Strongly-   │  │ Controlled  │           │  │    │
│  │   │  │ as Strings  │  │ Sees        │  │ Erasure     │           │  │    │
│  │   │  │ (§6.1)      │  │ (§6.3.1)    │  │             │           │  │    │
│  │   │  └─────────────┘  └─────────────┘  └─────────────┘           │  │    │
│  │   │                                                               │  │    │
│  │   │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐           │  │    │
│  │   │  │ Federation  │  │ String/     │  │ Performance │           │  │    │
│  │   │  │ Community   │  │ Lattice     │  │ Benchmarks  │           │  │    │
│  │   │  │             │  │ Core        │  │             │           │  │    │
│  │   │  └─────────────┘  └─────────────┘  └─────────────┘           │  │    │
│  │   └──────────────────────────────────────────────────────────────┘  │    │
│  │                                │                                     │    │
│  │   ┌────────────────────────────┴────────────────────────────────┐   │    │
│  │   │                    NETWORK SERVICES                          │   │    │
│  │   │                                                              │   │    │
│  │   │   erpc.datachain.network     ws.datachain.network           │   │    │
│  │   │   ─────────────────────     ────────────────────            │   │    │
│  │   │   JSON-RPC Endpoint          WebSocket Endpoint              │   │    │
│  │   │   (rope-node:8545)           (rope-node:8546)               │   │    │
│  │   │                                                              │   │    │
│  │   │   faucet.datachain.network   bridge.datachain.network       │   │    │
│  │   │   ─────────────────────────  ────────────────────────       │   │    │
│  │   │   Test DC Token Faucet       Cross-chain Bridge              │   │    │
│  │   └──────────────────────────────────────────────────────────────┘   │    │
│  │                                                                      │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                    │                                         │
│                                    │ Indexes & Fetches Data                  │
│                                    ▼                                         │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                  dcscan.io (BLOCK EXPLORER)                          │    │
│  │                  ═══════════════════════════                         │    │
│  │                                                                      │    │
│  │   ┌──────────────────────────────────────────────────────────────┐  │    │
│  │   │                    INDEXER LAYER                              │  │    │
│  │   │                                                               │  │    │
│  │   │   rope-indexer                                                │  │    │
│  │   │   ────────────                                                │  │    │
│  │   │   • Subscribes to rope-node events                           │  │    │
│  │   │   • Indexes strings, transactions, testimonies               │  │    │
│  │   │   • Stores in PostgreSQL                                     │  │    │
│  │   │   • Caches in Redis                                          │  │    │
│  │   └──────────────────────────────────────────────────────────────┘  │    │
│  │                                │                                     │    │
│  │   ┌────────────────────────────┴────────────────────────────────┐   │    │
│  │   │                    EXPLORER API                              │   │    │
│  │   │                                                              │   │    │
│  │   │   dc-explorer (api.dcscan.io)                               │   │    │
│  │   │   ─────────────────────────────                             │   │    │
│  │   │   • /api/v1/strings       - String data                     │   │    │
│  │   │   • /api/v1/transactions  - Transaction data                │   │    │
│  │   │   • /api/v1/testimonies   - AI testimony data               │   │    │
│  │   │   • /api/v1/validators    - Validator info                  │   │    │
│  │   │   • /api/v1/stats         - Network statistics              │   │    │
│  │   │   • /api/v1/projects      - Federation projects             │   │    │
│  │   │   • /api/v1/federations   - Federation data                 │   │    │
│  │   │   • /api/v1/communities   - Community data                  │   │    │
│  │   └──────────────────────────────────────────────────────────────┘   │    │
│  │                                │                                     │    │
│  │   ┌────────────────────────────┴────────────────────────────────┐   │    │
│  │   │                    EXPLORER FRONTEND                         │   │    │
│  │   │                                                              │   │    │
│  │   │   dcscan.io / testnet.dcscan.io                             │   │    │
│  │   │   ──────────────────────────────                            │   │    │
│  │   │   • View Strings & Transactions                              │   │    │
│  │   │   • Browse AI Agents & Testimonies                           │   │    │
│  │   │   • Explore Tokens & DeFi                                    │   │    │
│  │   │   • Network Statistics & Charts                              │   │    │
│  │   │   • Community Voting                                         │   │    │
│  │   └──────────────────────────────────────────────────────────────┘   │    │
│  │                                                                      │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

## Data Flow

```
┌──────────────┐    P2P Gossip     ┌──────────────┐
│  rope-node   │◄────────────────►│  rope-node   │
│  (Validator) │                   │  (Validator) │
└──────┬───────┘                   └──────────────┘
       │
       │ RPC/WebSocket
       │ (erpc.datachain.network)
       │ (ws.datachain.network)
       ▼
┌──────────────┐
│ rope-indexer │──────────────────┐
│              │                  │
└──────┬───────┘                  │
       │                          │
       │ PostgreSQL               │ Redis
       │ (indexed data)           │ (cache)
       ▼                          ▼
┌──────────────────────────────────┐
│         dc-explorer              │
│    (api.dcscan.io:3001)          │
└──────────────┬───────────────────┘
               │
               │ REST API
               │
               ▼
┌──────────────────────────────────┐
│       dcscan.io Frontend         │
│    (Browser/Client Application)  │
└──────────────────────────────────┘
```

## Component Mapping

### datachain.network Domain

The Datachain Rope Network domain hosts all core blockchain infrastructure:

| Subdomain | Service | Port | Description |
|-----------|---------|------|-------------|
| datachain.network | Main Website | 443 | Landing page, documentation |
| erpc.datachain.network | RPC API | 8545 | JSON-RPC endpoint for rope-node |
| ws.datachain.network | WebSocket | 8546 | Real-time subscriptions |
| faucet.datachain.network | Faucet | 443 | Testnet token distribution |
| bridge.datachain.network | Bridge | 443 | Cross-chain bridge interface |

**Core Components (rope-node):**
- Post-Quantum Cryptography (CRYSTALS-Dilithium3, Kyber768)
- Virtual Voting Algorithm (Appendix B.1)
- Reed-Solomon Erasure Coding
- Network Transport (libp2p with QUIC)
- libp2p Swarm Runtime
- Testimony as Strings (§6.1)
- Strongly-Sees Relation (§6.3.1)
- OES (Organic Encryption System)
- Controlled Erasure Protocol
- Federation/Community Management
- String/Lattice Core

### dcscan.io Domain

The Block Explorer domain provides data visualization and API access:

| Subdomain | Service | Port | Description |
|-----------|---------|------|-------------|
| dcscan.io | Explorer Frontend | 443 | Main explorer UI |
| api.dcscan.io | Explorer API | 3001 | REST API for indexed data |
| testnet.dcscan.io | Testnet Explorer | 443 | Testnet data explorer |

**Explorer Components:**
- dc-explorer (Rust/Axum API server)
- rope-indexer (Blockchain data indexer)
- PostgreSQL (Indexed data storage)
- Redis (Caching layer)

## API Endpoints

### erpc.datachain.network (Blockchain RPC)

```json
// Get network status
POST /
{"jsonrpc":"2.0","method":"rope_networkStatus","params":[],"id":1}

// Get string by ID
POST /
{"jsonrpc":"2.0","method":"rope_getString","params":["0x..."],"id":1}

// Submit string
POST /
{"jsonrpc":"2.0","method":"rope_submitString","params":[{...}],"id":1}

// Get testimony
POST /
{"jsonrpc":"2.0","method":"rope_getTestimony","params":["0x..."],"id":1}
```

### api.dcscan.io (Explorer REST API)

```
GET  /api/v1/strings           - List strings
GET  /api/v1/strings/:id       - Get string details
GET  /api/v1/transactions      - List transactions
GET  /api/v1/transactions/:id  - Get transaction details
GET  /api/v1/testimonies       - List testimonies
GET  /api/v1/validators        - List validators
GET  /api/v1/stats             - Network statistics
GET  /api/v1/projects          - List federation projects
POST /api/v1/projects          - Submit new project
POST /api/v1/projects/:id/vote - Vote on project
GET  /api/v1/federations       - List federations
GET  /api/v1/communities       - List communities
GET  /api/v1/health            - Health check
```

## Deployment Configuration

### Docker Services

```yaml
# datachain.network services
rope-node:        # Core blockchain node
  ports: 8545, 8546, 9000

# dcscan.io services  
dc-explorer:      # Explorer API
  ports: 3001
rope-indexer:     # Blockchain indexer
  depends_on: rope-node
postgres:         # Data storage
  ports: 5432
redis:            # Caching
  ports: 6379
nginx:            # Reverse proxy
  ports: 80, 443
```

### Connection Flow

1. **rope-node** runs on datachain.network infrastructure
2. **rope-indexer** connects to rope-node via RPC/WebSocket
3. **rope-indexer** indexes data into PostgreSQL
4. **dc-explorer** serves indexed data via REST API
5. **dcscan.io** frontend fetches from dc-explorer API
6. **Nginx** routes traffic to appropriate services

## Production Readiness

All components under **datachain.network** are at 100% implementation:

| Component | Crate | Status |
|-----------|-------|--------|
| Post-Quantum Crypto | rope-crypto | ✅ 100% |
| Virtual Voting | rope-consensus | ✅ 100% |
| Reed-Solomon | rope-protocols | ✅ 100% |
| Network Transport | rope-network | ✅ 100% |
| libp2p Swarm | rope-network | ✅ 100% |
| Testimony Strings | rope-consensus | ✅ 100% |
| Strongly-Sees | rope-consensus | ✅ 100% |
| OES | rope-crypto | ✅ 100% |
| Controlled Erasure | rope-protocols | ✅ 100% |
| Federation/Community | rope-federation | ✅ 100% |
| String/Lattice | rope-core | ✅ 100% |
| Benchmarks | rope-benchmarks | ✅ 100% |
| Load Testing | rope-loadtest | ✅ 100% |

## Security Considerations

- **TLS 1.3** on all HTTPS endpoints
- **Post-Quantum** cryptography for long-term security
- **OES** for data encryption
- **CORS** configured for API endpoints
- **Rate limiting** on public APIs
- **Authentication** for write operations

