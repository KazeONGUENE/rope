# DC FAT Tokenomics

## Overview

DC FAT (Datachain Future Access Token) is the native utility and governance token of the Datachain Rope network. Inspired by Bitcoin's scarcity model and Ethereum's staking economics, DC FAT features a predictable emission schedule with Bitcoin-style halvings and performance-based reward multipliers.

## Supply Schedule

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     DC FAT TOTAL SUPPLY PROJECTION                          │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Genesis (2026):        10,000,000,000 FAT (10 billion)                     │
│  Year 5 (2030):         12,000,000,000 FAT                                  │
│  Year 10 (2035):        13,500,000,000 FAT                                  │
│  Year 20 (2045):        14,500,000,000 FAT                                  │
│  Asymptotic Maximum:    ~18,000,000,000 FAT                                 │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Emission Schedule (Bitcoin-Style Halving)

| Era | Years | Annual Emission | Block Reward | Inflation Rate |
|-----|-------|-----------------|--------------|----------------|
| 1 | 2026-2029 | 500,000,000 FAT | ~66.6 FAT | 5.00% → 4.00% |
| 2 | 2030-2033 | 250,000,000 FAT | ~33.3 FAT | 2.08% → 1.85% |
| 3 | 2034-2037 | 125,000,000 FAT | ~16.65 FAT | 0.93% → 0.87% |
| 4 | 2038-2041 | 62,500,000 FAT | ~8.33 FAT | 0.43% → 0.42% |
| 5 | 2042-2045 | 31,250,000 FAT | ~4.16 FAT | 0.21% → 0.21% |
| 6+ | 2046+ | Halving continues | → 1M floor | ≈0.01% |

### Key Constants

```rust
const GENESIS_SUPPLY: u128 = 10_000_000_000 * ONE_FAT;  // 10 billion
const ANNUAL_EMISSION_ERA1: u128 = 500_000_000 * ONE_FAT;  // 500 million
const HALVING_INTERVAL: u64 = 4 years;  // Like Bitcoin
const MINIMUM_ANNUAL_EMISSION: u128 = 1_000_000 * ONE_FAT;  // 1 million floor
const ANCHORS_PER_YEAR: u64 = 7_500_000;  // ~4.2 second anchors
```

## Reward Distribution

### Anchor Block Rewards

Each anchor block (finalized every ~4.2 seconds) distributes rewards:

| Recipient | Share | Era 1 Reward | Description |
|-----------|-------|--------------|-------------|
| Anchor Proposer | 30% | ~20 FAT | Validator creating the anchor |
| Testimony Pool | 45% | ~30 FAT | Validators providing AI testimonies |
| Node Operators | 20% | ~13.3 FAT | Storage and bandwidth providers |
| Federation/Community | 5% | ~3.3 FAT | Active federations/communities |

### Performance Multipliers

Rewards are adjusted based on performance:

| Metric | Weight | Scoring |
|--------|--------|---------|
| Uptime | 30% | 99.9%=1.0, 99%=0.9, 95%=0.5 |
| Testimony Speed | 20% | <100ms=1.0, <500ms=0.5 |
| Bandwidth | 15% | 25Gbps=1.0, 1Gbps=0.5 |
| Storage | 10% | 100TB=1.0, 10TB=0.5 |
| Green Energy | 10% | 100%=1.0, 50%=0.5 |
| Security | 10% | Based on infrastructure |
| Geographic | 5% | Bonus for underserved regions |

**Multiplier Range:** 0.3x (poor) → 2.0x (excellent)

### Bonus Multipliers

| Bonus Type | Multiplier | Requirements |
|------------|------------|--------------|
| Federation Operator | 1.3x | Running a federation |
| Community Operator | 1.2x | Running a community |
| Green Energy (100%) | 1.25x | Certified renewable |
| Long-term (12+ months) | 1.2x | Active >12 months |
| Hardware Security | 1.15x | HSM/TPM verified |
| Geographic Diversity | 1.1x | Underserved regions |

## Staking Requirements

### Validator Tiers

| Tier | Minimum Stake | Lock Period | Unbond Time | Reward Boost |
|------|---------------|-------------|-------------|--------------|
| Standard | 1,000,000 FAT | 3 months | 14 days | 1.0x |
| Professional | 5,000,000 FAT | 6 months | 14 days | 1.1x |
| Enterprise | 25,000,000 FAT | 12 months | 21 days | 1.2x |
| Foundation | 100,000,000 FAT | 24 months | 30 days | 1.3x |

### Other Stakes

| Role | Minimum Stake |
|------|---------------|
| Databox Node | 100,000 FAT |
| Federation Creator | 10,000,000 FAT |
| Community Creator | 1,000,000 FAT |

## Expected Returns

### APY Projections (Era 1)

| Total Staked | Validator APY | Node APY |
|--------------|---------------|----------|
| 100M FAT | ~375% | ~75% |
| 500M FAT | ~75% | ~15% |
| 1B FAT | ~37.5% | ~7.5% |
| 2B FAT | ~18.75% | ~3.75% |
| 5B FAT | ~7.5% | ~1.5% |

*APY decreases as more FAT is staked (supply/demand equilibrium)*

### Daily Reward Examples (Era 1)

| Scenario | Stake | Performance | Daily Reward |
|----------|-------|-------------|--------------|
| Average Validator | 1M FAT | 1.0x | ~41 FAT |
| Elite Validator | 1M FAT | 2.0x | ~82 FAT |
| Federation + Green | 25M FAT | 2.5x | ~2,562 FAT |
| Poor Performer | 1M FAT | 0.3x | ~12 FAT |

## Federation & Community Rewards

### Activity Tiers

| Tier | Requirements | Pool Share |
|------|--------------|------------|
| Platinum | 10M+ tx/month, 1M+ users | 40% |
| Gold | 1M+ tx/month, 100K+ users | 30% |
| Silver | 100K+ tx/month, 10K+ users | 20% |
| Bronze | <100K tx/month | 10% |

### Eligibility

- Minimum 1,000 transactions/month
- Minimum 100 active users
- Valid node infrastructure

## Green Energy Incentives

### Energy Source Multipliers

| Source | Multiplier | Carbon Intensity |
|--------|------------|------------------|
| Solar | 1.25x | 40 gCO2/kWh |
| Wind | 1.25x | 10 gCO2/kWh |
| Hydro | 1.20x | 20 gCO2/kWh |
| Nuclear | 1.15x | 12 gCO2/kWh |
| Grid Mix | 1.0x | 400 gCO2/kWh |
| High Carbon | 0.85x | 900 gCO2/kWh |

### Certification

Accepted certificate issuers:
- I-REC Standard
- Guarantee of Origin (EU)
- REC (US)
- GreenPower (Australia)
- J-Credit (Japan)

## Slashing & Penalties

### Offense Types

| Offense | Penalty | Jail Duration |
|---------|---------|---------------|
| Double Signing | 5% stake | 30 days |
| Extended Downtime | 1,000 FAT | 7 days |
| Invalid Testimony | 1% stake | 14 days |
| Data Corruption | 10% stake | 60 days |
| Collusion | 20% stake | Permanent ban |

### Slashed Funds Distribution

| Destination | Share |
|-------------|-------|
| Burned | 50% |
| Insurance Fund | 30% |
| Reporter Reward | 20% |

## Foundation Minting Rights

Datachain Foundation retains the ability to mint additional DC FAT under specific conditions:

### Minting Governance

Required approvals (12 total):
- 5 AI Testimony Agents
- 5 Random Governors (elected validators)
- 2 Foundation Members

### Approved Use Cases

1. **Ecosystem Development** - Grants, developer incentives
2. **Strategic Partnerships** - Enterprise adoption
3. **Emergency Treasury** - Critical network operations
4. **Community Initiatives** - DAO proposals

### Limits

- Maximum 100M FAT per minting event
- 30-day cooldown between mints
- Full on-chain transparency

## Comparison with Bitcoin & Ethereum

| Aspect | Bitcoin | Ethereum | DC FAT |
|--------|---------|----------|--------|
| Genesis Supply | 0 | 72M (pre-mine) | 10B |
| Emission Model | Mining | Issuance + Burn | Halving + Floor |
| Halving Interval | 4 years | N/A | 4 years |
| Max Supply | 21M | Unlimited | ~18B |
| Block Time | 10 min | 12 sec | 4.2 sec |
| Consensus | PoW | PoS | DkP/PoA |
| Staking Min | N/A | 32 ETH | 1M FAT |
| Target APY | N/A | 3-5% | 5% (eq.) |

## Implementation

The tokenomics are implemented in the `rope-economics` crate:

```rust
use rope_economics::{
    EmissionSchedule,
    RewardCalculator,
    StakeManager,
    FederationRewards,
    GreenEnergyVerification,
    SlashingEngine,
};

// Create emission schedule
let emission = EmissionSchedule::mainnet();

// Calculate current anchor reward
let reward = emission.current_anchor_reward(timestamp);

// Create reward calculator
let mut calculator = RewardCalculator::new(emission);

// Calculate validator reward with performance
let reward = calculator.calculate_proposer_reward(validator_id, timestamp);
```

## Summary

DC FAT combines:
- **Bitcoin's scarcity** via halving emission schedule
- **Ethereum's staking** with tiered validator requirements
- **Performance incentives** rewarding uptime, speed, and efficiency
- **Green energy rewards** encouraging sustainable operations
- **Federation/Community** activity-based distributions
- **Fair slashing** protecting network integrity

This creates a sustainable economic model that rewards long-term participation, environmental responsibility, and network contribution while maintaining predictable token supply growth.
