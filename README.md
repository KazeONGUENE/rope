# Datachain Rope

<div align="center">

![Datachain Rope](https://img.shields.io/badge/Datachain-Rope-blue?style=for-the-badge)
![Rust](https://img.shields.io/badge/Rust-1.75+-orange?style=for-the-badge&logo=rust)
![License](https://img.shields.io/badge/License-Apache%202.0-green?style=for-the-badge)

**A Distributed Information Communication Protocol Inspired by DNA**

*Replacing blockchain's linear chains with intertwined strings that can heal, adapt, and forget*

</div>

---

## 🧬 Overview

Datachain Rope is a revolutionary distributed information communication protocol that fundamentally reconceptualizes how data is transmitted, stored, and secured across decentralized networks. Unlike blockchain architectures that rely on sequential block structures, Datachain Rope implements a **string-based topology** where information strands interweave to form resilient cords—directly analogous to the **double helix of DNA**.

### Key Innovations

| Innovation | Description |
|------------|-------------|
| **String Lattice Architecture** | Continuous, parallel DAG replacing discrete blocks |
| **Testimony Consensus** | Virtual voting + accountable attestations (2-3s finality) |
| **Organic Encryption System** | Self-evolving post-quantum cryptography |
| **Regeneration Protocol** | DNA-inspired data repair and recovery |
| **Controlled Erasure Protocol** | GDPR-compliant, privacy-preserving deletion |
| **Rope Distribution Protocol** | BitTorrent-inspired decentralized distribution |

### Performance Targets

- **Throughput**: 50,000+ TPS
- **Finality**: 2-3 seconds
- **Quantum Resistance**: CRYSTALS-Dilithium3 + CRYSTALS-Kyber768
- **Byzantine Tolerance**: n ≥ 3f + 1

---

## 🏗️ Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    DATACHAIN ROPE ARCHITECTURE                  │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │   L0 CORE    │  │   L1 RELAY   │  │   L2 BRIDGE  │          │
│  │  Federation  │  │    Public    │  │Cross-Protocol│          │
│  │  Validators  │  │   Seeders    │  │  Connectors  │          │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘          │
│         │                 │                  │                  │
│         └────────┬────────┴──────────────────┘                  │
│                  │                                              │
│  ┌───────────────▼───────────────────────────────────────────┐ │
│  │                   STRING LATTICE (DAG)                     │ │
│  │                                                            │ │
│  │    S₁ ──┬──► S₃ ──┬──► S₅ (anchor)    Primary Strands     │ │
│  │         │         │                                        │ │
│  │    S₂ ──┘         └──► S₆                                 │ │
│  │         ╲               ╲                                  │ │
│  │    S̄₁ ──┴──► S̄₃ ──┴──► S̄₅ (complement) Verification     │ │
│  │                                                            │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                 │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │           ORGANIC ENCRYPTION SYSTEM (OES)                  │ │
│  │   Generation 0 ──► Gen 1 ──► Gen 2 ──► ... (evolves)      │ │
│  │   Ed25519 + Dilithium3 | X25519 + Kyber768 | BLAKE3       │ │
│  └────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

---

## 📦 Crate Structure

```
datachain-rope/
├── Cargo.toml              # Workspace configuration
├── crates/
│   ├── rope-core/          # Core data structures
│   │   ├── types.rs        # StringId, NodeId, MutabilityClass
│   │   ├── string.rs       # RopeString (σ, τ, π, ρ, μ)
│   │   ├── nucleotide.rs   # Atomic information units
│   │   ├── complement.rs   # Reed-Solomon encoded parity
│   │   ├── lattice.rs      # StringLattice DAG implementation
│   │   └── clock.rs        # Lamport clock for causality
│   │
│   ├── rope-crypto/        # Cryptographic primitives
│   │   ├── oes.rs          # Organic Encryption System
│   │   ├── hybrid.rs       # Ed25519+Dilithium, X25519+Kyber
│   │   ├── hash.rs         # BLAKE3 utilities
│   │   └── keys.rs         # Key management
│   │
│   ├── rope-consensus/     # Testimony Consensus Protocol
│   │   ├── testimony.rs    # Attestation mechanism
│   │   ├── anchor.rs       # Anchor string creation
│   │   ├── virtual_voting.rs # Hashgraph-style voting
│   │   └── finality.rs     # Finality determination
│   │
│   ├── rope-network/       # P2P networking (libp2p)
│   │   ├── transport.rs    # QUIC transport
│   │   ├── gossip.rs       # Gossip-about-gossip
│   │   ├── discovery.rs    # DHT and peer discovery
│   │   └── rpc.rs          # gRPC API server
│   │
│   ├── rope-storage/       # Persistence (RocksDB)
│   │   ├── lattice_db.rs   # Lattice storage
│   │   ├── complement_db.rs # Complement storage
│   │   └── state_db.rs     # OES/Federation state
│   │
│   ├── rope-node/          # Node implementation
│   └── rope-cli/           # Command-line interface
│
└── docs/                   # Documentation
    └── ROADMAP.md          # Development roadmap
```

---

## 🚀 Quick Start

### Prerequisites

- **Rust 1.75+** - Install via [rustup](https://rustup.rs/)
- **RocksDB** - Usually bundled, may need system libraries

### Build

```bash
# Clone the repository
git clone https://github.com/datachain-foundation/datachain-rope.git
cd datachain-rope

# Build all crates
cargo build --release

# Run tests
cargo test --all

# Run the CLI
cargo run --release --bin rope -- info
```

### Configuration

```toml
# rope.toml
[node]
mode = "validator"  # validator, relay, seeder
chain_id = "datachain-mainnet-1"

[lattice]
replication_factor = 5
erasure_enabled = true
regeneration_enabled = true

[oes]
evolution_interval = 100  # anchors
genome_dimension = 992
mutation_rate = 0.1

[distribution]
max_peers = 50
seeding_ratio = 2.0
```

---

## 📐 Mathematical Foundation

### String Formal Definition

A String S in Datachain Rope is a 5-tuple:

```
S = (σ, τ, π, ρ, μ)

Where:
  σ (Sigma)   - Sequence of nucleotides (content)
  τ (Tau)     - Temporal marker (Lamport clock)
  π (Pi)      - Parentage (parent StringIds, forming DAG)
  ρ (Rho)     - Replication factor (default: 5)
  μ (Mu)      - Mutability class (erasure policy)
```

### String Lattice Definition

```
L = (S, ≺, ⊗, R)

Where:
  S           - Set of all strings
  ≺ (Precedes) - Partial ordering (causal DAG)
  ⊗ (Intertwine) - Complementary pairing (double helix)
  R (Regeneration) - Repair relation
```

---

## 🔐 Security

### Cryptographic Algorithms

| Function | Algorithm | Security Level |
|----------|-----------|----------------|
| Signatures | Ed25519 + CRYSTALS-Dilithium3 | 256-bit + NIST PQ-3 |
| Hashing | BLAKE3 | 256-bit |
| Key Exchange | X25519 + CRYSTALS-Kyber768 | 256-bit + NIST PQ-3 |
| Complement | Reed-Solomon erasure codes | Regeneration capable |

### Security Properties

- **Perfect Forward Secrecy**: OES evolution destroys past state
- **Quantum Resistance**: Post-quantum primitives throughout
- **Byzantine Tolerance**: n ≥ 3f + 1 validator threshold
- **Controlled Erasure**: GDPR Article 17 compliant

---

## 📊 Comparison with Existing Protocols

| Property | Bitcoin | Ethereum | Hashgraph | **Datachain Rope** |
|----------|---------|----------|-----------|-------------------|
| Paradigm | Blockchain | Blockchain | Hashgraph | **String Lattice** |
| Data Structure | Linear | Linear | DAG | **Double Helix DAG** |
| Finality | ~60 min | ~15 min | ~3-5 sec | **~2-3 sec** |
| Throughput | ~7 TPS | ~30 TPS | ~10K TPS | **~50K+ TPS** |
| Erasability | No | No | No | **Yes (controlled)** |
| Regeneration | No | No | No | **Yes (DNA-like)** |
| Quantum Resistant | No | Partial | No | **Yes (OES)** |

---

## 🛤️ Development Roadmap

See [ROADMAP.md](docs/ROADMAP.md) for detailed development phases.

| Phase | Duration | Focus |
|-------|----------|-------|
| Phase 1 | Weeks 1-8 | Core Foundation |
| Phase 2 | Weeks 9-16 | Consensus Layer |
| Phase 3 | Weeks 17-24 | Network Layer |
| Phase 4 | Weeks 25-32 | Advanced Protocols |
| Phase 5 | Weeks 33-40 | Bridges & Integration |

---

## 📝 License

Apache License 2.0

---

## 📧 Contact

**Datachain Foundation DDMI**  
Paris, France  
contact@datachain.foundation

---

<div align="center">

*"Datachain Rope is not a blockchain. It has no blocks. It has strings that interweave to form a resilient, regenerative, and when necessary, erasable structure."*

**Author: Kazé A. ONGUENE, CEO & Visionary**

</div>

