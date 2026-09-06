# LaunchLab Smart Contracts

Production-ready smart contracts for the LaunchLab platform on DCSwap/Datachain Rope.

## Contracts

### LaunchLabIdentity.sol
Soulbound (non-transferable) ERC-721 NFT for project owner identity.

**Features:**
- One identity per wallet (enforced)
- Metadata stored on IPFS as JSON document
- Protocol fee for creation
- UUPS upgradeable

**Key Functions:**
```solidity
function createIdentity(string calldata initialCid) external payable returns (uint256);
function updateMetadata(string calldata newCid) external;
function getMetadataCID(uint256 tokenId) external view returns (string memory);
```

### LaunchLabAirdrop.sol
Merkle-tree based airdrop for gas-efficient token distribution.

**Features:**
- O(log n) proof verification
- Time-bounded claiming
- Configurable claim delay (anti-bot)
- Multi-round support
- Unclaimed token recovery

**Key Functions:**
```solidity
function claim(uint256 amount, bytes32[] calldata proof) external;
function initiateClaim(uint256 amount, bytes32[] calldata proof) external;
function canClaim(address account, uint256 amount, bytes32[] calldata proof) external view;
```

### VestingEscrow.sol
Escrow contract for OTC deals with customizable vesting schedules.

**Features:**
- TGE (Token Generation Event) unlock
- Cliff period with optional cliff unlock
- Linear vesting after cliff
- Revocable option
- Beneficiary change support

**Key Functions:**
```solidity
function claimTGE() external;
function claimCliff() external;
function release() external;
function getVestedAmount() public view returns (uint256);
```

## Installation

```bash
# Navigate to contracts directory
cd contracts

# Install dependencies with Foundry
forge install OpenZeppelin/openzeppelin-contracts@v5.0.2 --no-commit
forge install OpenZeppelin/openzeppelin-contracts-upgradeable@v5.0.2 --no-commit
forge install foundry-rs/forge-std --no-commit

# Build contracts
forge build

# Run tests
forge test

# Run tests with gas report
forge test --gas-report
```

## Deployment

### Testnet (Datachain Rope Testnet - Chain ID 271829)

```bash
# Set environment variables
export PRIVATE_KEY=your_private_key
export RPC_URL=https://testnet.erpc.datachain.network

# Deploy LaunchLabIdentity
forge create --rpc-url $RPC_URL \
  --private-key $PRIVATE_KEY \
  src/launchlab/LaunchLabIdentity.sol:LaunchLabIdentity

# Deploy proxy with initialization
# (Use a deployment script for production)
```

### Mainnet (Datachain Rope - Chain ID 271828)

```bash
export RPC_URL=https://erpc.datachain.network

# Follow same deployment steps with mainnet RPC
```

## Configuration

### LaunchLabIdentity Constructor
```solidity
function initialize(
    address _treasury,           // Address receiving protocol fees
    uint256 _identityCreationFee, // Fee in wei (e.g., 0.01 ether)
    string calldata _ipfsGateway  // IPFS gateway URL
)
```

### LaunchLabAirdrop Constructor
```solidity
constructor(
    address _token,              // Token to distribute
    bytes32 _merkleRoot,         // Merkle root of claims
    uint256 _startTime,          // Claim start timestamp
    uint256 _endTime,            // Claim end timestamp
    uint256 _totalAmount,        // Total tokens
    uint256 _claimDelay,         // Seconds between initiate and claim
    string memory _metadataCID   // IPFS CID with claim list
)
```

### VestingEscrow Constructor
```solidity
constructor(
    address _token,              // Token to vest
    address _beneficiary,        // Recipient
    uint256 _totalAmount,        // Total tokens
    uint256 _startTime,          // Vesting start
    uint256 _cliffDuration,      // Cliff in seconds
    uint256 _vestingDuration,    // Vesting after cliff in seconds
    uint256 _tgeUnlockBps,       // TGE unlock (10000 = 100%)
    uint256 _cliffUnlockBps,     // Cliff unlock (10000 = 100%)
    bool _revocable              // Whether owner can revoke
)
```

## Security Considerations

1. **Upgrade Safety**: All upgradeable contracts use UUPS pattern. Only owner can upgrade.
2. **Reentrancy Protection**: All state-changing functions use ReentrancyGuard.
3. **Access Control**: Ownable pattern for admin functions.
4. **Input Validation**: Custom errors for all invalid inputs.
5. **Safe Transfers**: Uses OpenZeppelin's SafeERC20.

## Gas Optimization

- Custom errors instead of require strings
- Efficient storage packing
- Minimal storage reads/writes
- Via-IR compilation enabled

## Audit Status

- [ ] Internal review completed
- [ ] External audit pending

## License

MIT License

## References

- [LaunchLab Specification v1.0](../../../docs/LAUNCHLAB_SPECIFICATION_v1.0.md)
- [OpenZeppelin Contracts](https://docs.openzeppelin.com/contracts)
- [Foundry Book](https://book.getfoundry.sh/)
