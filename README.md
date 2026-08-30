# Datachain Rope

<div align="center">

![Datachain Rope](https://img.shields.io/badge/Datachain-Rope-blue?style=for-the-badge)
![Rust](https://img.shields.io/badge/Rust-1.75+-orange?style=for-the-badge&logo=rust)
![License](https://img.shields.io/badge/License-Apache%202.0-green?style=for-the-badge)
![Chain ID](https://img.shields.io/badge/Chain%20ID-271828-purple?style=for-the-badge)

**The First Smartchain - An Intelligent Distributed Information Protocol**

*Not a blockchain. A Smartchain with AI-powered testimonies that can heal, adapt, and forget*

[🌐 datachain.network](https://datachain.network) | [🔗 rope.network](https://rope.network) | [📊 DC Scan](https://dcscan.io)

</div>

---

## 🌐 Network Information

Add **Datachain Rope** to MetaMask or any EVM-compatible wallet:

| Parameter | Value |
|-----------|-------|
| **Network Name** | Datachain Rope |
| **Chain ID** | `271828` (0x425D4) |
| **Currency Symbol** | `FAT` |
| **Currency Name** | DC FAT |
| **RPC URL** | `https://erpc.datachain.network` |
| **RPC URL (Alt)** | `https://erpc.rope.network` |
| **WebSocket** | `wss://ws.datachain.network` |
| **Block Explorer** | `https://dcscan.io` |

### Domains

| Domain | Purpose |
|--------|---------|
| [datachain.network](https://datachain.network) | Primary network domain |
| [rope.network](https://rope.network) | Secondary/alias domain |
| [dcscan.io](https://dcscan.io) | DC Explorer |

### Bridge Contracts

| Bridge | Contract Address |
|--------|-----------------|
| Ethereum | `0x0b44547be0a0df5dcd5327de8ea73680517c5a54` |
| XDC | `0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a` |
| Polkadot | Coming Soon |

### Testnet

| Parameter | Value |
|-----------|-------|
| **Network Name** | Datachain Rope Testnet |
| **Chain ID** | `271829` (0x425D5) |
| **Currency Symbol** | `xFAT` |
| **Currency Name** | Testnet xFAT (gas credit, not mainnet FAT) |
| **RPC URL** | `https://testnet.erpc.datachain.network` |
| **Block Explorer** | `https://testnet.dcscan.io` |
| **Faucet** | `https://faucet.datachain.network` |

---

## What is a Smartchain?

**Datachain Rope is NOT a blockchain. It is a Smartchain.**

A Smartchain is an intelligent distributed system that goes beyond cryptographic proofs:

| Blockchain | Smartchain |
|------------|------------|
| Validates: "Is the signature valid?" | Validates: "Is this valid AND does it make business sense?" |
| Static consensus rules | AI-powered adaptive validation |
| Single protocol | Invokes any vetted tool (blockchain, banking, finance, etc.) |
| Just stores data | Understands, validates, and executes with intelligence |

### AI Testimony Agents

Traditional consensus relies on mathematical proofs. Smartchain extends this with **AI Testimony Agents**:

- **ValidationAgent**: Validates transaction semantics and business rules
- **ContractAgent**: Monitors and enforces smart contract conditions  
- **ComplianceAgent**: Ensures regulatory compliance (KYC/AML/GDPR)
- **InsuranceAgent**: Validates claims before payout
- **OracleAgent**: Bridges external data for contract evaluation

### Vetted Tool Registry

The Smartchain can invoke external protocols to execute transactions:

```
┌─────────────────────────────────────────────────────────────────┐
│                    SMARTCHAIN INVOCATION                         │
│                                                                  │
│   Contract Condition → AI Testimony Validation → Tool Execution │
│                                                                  │
│   ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐       │
│   │ Ethereum │  │  SWIFT   │  │  Asset   │  │ Insurance│       │
│   │ Protocol │  │ Banking  │  │Management│  │ Platform │       │
│   └──────────┘  └──────────┘  └──────────┘  └──────────┘       │
└─────────────────────────────────────────────────────────────────┘
```

---

##Overview

Datachain Rope is a revolutionary distributed information communication protocol that fundamentally reconceptualizes how data is transmitted, stored, and secured across decentralized networks. Unlike blockchain architectures that rely on sequential block structures, Datachain Rope implements a **string-based topology** where information strands interweave to form resilient cords, directly analogous to the **double helix of DNA**.

### Key Innovations

| Innovation | Description |
|------------|-------------|
| **String Lattice Architecture** | Continuous, parallel DAG replacing discrete blocks |
| **AI Testimony Protocol** | AI agents validate conditions before execution |
| **Vetted Tool Invocation** | Connect to any protocol (blockchain, banking, etc.) |
| **Organic Encryption System** | Self-evolving post-quantum cryptography |
| **Digitized Contracts** | Smart contracts with AI-validated conditions |
| **Regeneration Protocol** | DNA-inspired data repair and recovery |
| **Controlled Erasure Protocol** | GDPR-compliant, privacy-preserving deletion |

### Performance Targets

- **Throughput**: 50,000+ TPS
- **Finality**: 2-3 seconds
- **Quantum Resistance**: CRYSTALS-Dilithium3 + CRYSTALS-Kyber768
- **Byzantine Tolerance**: n ≥ 3f + 1

---

##Architecture

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

##Crate Structure

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
│   ├── rope-smartchain/    # AI-Powered Smartchain Layer
│   │   ├── testimony_agent.rs  # AI agents for validation
│   │   ├── tool_registry.rs    # Vetted tool registry
│   │   ├── invocation_engine.rs # Orchestration engine
│   │   ├── security_policy.rs  # Adaptive security
│   │   └── protocol_adapters.rs # External protocol adapters
│   │
│   ├── rope-consensus/     # Testimony Consensus Protocol
│   │   ├── testimony.rs    # Attestation mechanism
│   │   ├── anchor.rs       # Anchor string creation
│   │   ├── virtual_voting.rs # Hashgraph-style voting
│   │   └── finality.rs     # Finality determination
│   │
│   ├── rope-bridge/        # External protocol bridges
│   │   ├── ethereum.rs     # Ethereum EVM bridge
│   │   ├── swift.rs        # SWIFT banking bridge
│   │   └── xdc.rs          # XDC Network bridge
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

## 🆔 Entity Identity (Datawallet)

Every entity in Datachain Rope is linked to a distributed identity (Datawallet):

```
┌─────────────────────────────────────────────────────────────────┐
│                         ENTITY (Datawallet)                     │
│  ┌─────────────────┐   ┌─────────────────┐   ┌──────────────┐  │
│  │ NodeId (32 bytes)│   │ HybridPublicKey │   │ Credentials  │  │
│  │ = BLAKE3(pk)    │   │ Ed25519+Dilithium│   │ & Contracts  │  │
│  └─────────────────┘   └─────────────────┘   └──────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

| Component | Description |
|-----------|-------------|
| **NodeId** | 32-byte identifier derived from public key (like wallet address) |
| **HybridPublicKey** | Ed25519 + Dilithium (quantum-resistant identity) |
| **Credentials** | Verifiable claims stored in lattice |
| **Contracts** | Digitized contracts with conditions |

---

## 📜 Digitized Contracts

Smart contracts in Smartchain are **Digitized Contracts** with AI-validated conditions:

```rust
DigitizedContract {
    contract_id: [u8; 32],           // StringId in lattice
    parties: Vec<ContractParty>,      // All signatories
    conditions: Vec<ContractCondition>, // AI-validated conditions
    actions: Vec<ContractAction>,     // Execute via vetted tools
}
```

### Condition Types

- **Temporal**: "Execute after date X"
- **ValueThreshold**: "If payment > $10,000"
- **InsuranceClaim**: "If damage verified"
- **ComplianceCheck**: "If KYC/AML passed"
- **MultiSig**: "If 3/5 parties sign"

### Example: Insurance Claim

```
1. Policy holder submits claim → Creates String in lattice
2. InsuranceAgent (AI) validates damage evidence
3. ComplianceAgent (AI) verifies identity
4. If conditions met → Invoke banking protocol for payout
5. Record result in lattice with proof
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
- **AI Validation**: Context-aware security beyond cryptography

---

## 📊 Comparison with Existing Protocols

| Property | Bitcoin | Ethereum | Hashgraph | **Datachain Rope** |
|----------|---------|----------|-----------|-------------------|
| Paradigm | Blockchain | Blockchain | Hashgraph | **Smartchain** |
| Data Structure | Linear | Linear | DAG | **Double Helix DAG** |
| Finality | ~60 min | ~15 min | ~3-5 sec | **~2-3 sec** |
| Throughput | ~7 TPS | ~30 TPS | ~10K TPS | **~50K+ TPS** |
| AI Validation | No | No | No | **Yes (Testimony Agents)** |
| External Protocol | Manual | Manual | No | **Automatic (Tool Registry)** |
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

**Datachain Foundation**
**DDMI Division**   
We Are Powering NextGen Internet With Our
Decentralized Data Management Infrastructure
HQ: 54 Boulevard de Clichy, 75018 Paris, FR
Fr. +33 1 42 59 71 14
contact@datachain.one
www.datachain.one

---

<div align="center">

*"Datachain Rope is not a blockchain. It is a Smartchain.*

*It has no blocks—it has strings that interweave to form a resilient, regenerative, and when necessary, erasable structure.*

*Its Testimonies are not just signatures—they are AI agents that understand, validate, and execute.*

*It doesn't just store transactions—it invokes the right tools to make them happen."*

**Author: Kazé A. ONGUENE, CEO & Visionary**

</div>


# Repository now public

