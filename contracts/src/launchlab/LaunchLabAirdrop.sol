// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/utils/cryptography/MerkleProof.sol";
import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import "@openzeppelin/contracts/access/Ownable.sol";

/**
 * @title LaunchLabAirdrop
 * @author Datachain Foundation
 * @notice Gas-efficient airdrop contract using Merkle proofs for verification
 * @dev Allows project owners to distribute tokens to large numbers of recipients
 *      without storing individual claims on-chain. Recipients claim tokens by
 *      providing a Merkle proof.
 * 
 * Features:
 * - Merkle tree verification for O(log n) proof verification
 * - Time-bounded claiming period
 * - Configurable claim delay for anti-bot protection
 * - Support for multiple claim rounds
 * - Emergency recovery of unclaimed tokens
 * - Events for off-chain indexing
 */
contract LaunchLabAirdrop is ReentrancyGuard, Ownable {
    using SafeERC20 for IERC20;
    
    // ============ Structs ============
    
    struct AirdropConfig {
        IERC20 token;
        bytes32 merkleRoot;
        uint256 startTime;
        uint256 endTime;
        uint256 totalAmount;
        uint256 claimedAmount;
        uint256 claimDelay;        // Minimum seconds between eligibility check and claim
        bool isPaused;
        string metadataCID;        // IPFS CID with claim list for transparency
    }
    
    // ============ State Variables ============
    
    /// @notice Airdrop configuration
    AirdropConfig public config;
    
    /// @notice Mapping of address to claimed status
    mapping(address => bool) public hasClaimed;
    
    /// @notice Mapping of address to claim timestamp (for claim delay)
    mapping(address => uint256) public claimInitiatedAt;
    
    /// @notice Total number of claims made
    uint256 public totalClaims;
    
    // ============ Events ============
    
    /// @notice Emitted when tokens are claimed
    event Claimed(
        address indexed account, 
        uint256 amount,
        uint256 timestamp
    );
    
    /// @notice Emitted when airdrop is paused/unpaused
    event PauseStateChanged(bool isPaused);
    
    /// @notice Emitted when unclaimed tokens are recovered
    event TokensRecovered(address indexed token, address indexed to, uint256 amount);
    
    /// @notice Emitted when merkle root is updated (for multi-round airdrops)
    event MerkleRootUpdated(bytes32 oldRoot, bytes32 newRoot);
    
    /// @notice Emitted when claim is initiated (for delayed claims)
    event ClaimInitiated(address indexed account, uint256 timestamp);
    
    // ============ Errors ============
    
    error AirdropNotStarted(uint256 startTime, uint256 currentTime);
    error AirdropEnded(uint256 endTime, uint256 currentTime);
    error AirdropPaused();
    error AlreadyClaimed(address account);
    error InvalidProof();
    error ClaimNotInitiated(address account);
    error ClaimDelayNotMet(uint256 initiatedAt, uint256 requiredDelay);
    error ZeroAddress();
    error InvalidAmount();
    error InvalidTimeRange();
    error InsufficientBalance(uint256 available, uint256 required);
    
    // ============ Constructor ============
    
    /**
     * @notice Deploy a new airdrop contract
     * @param _token Token to distribute
     * @param _merkleRoot Root of the Merkle tree containing all claims
     * @param _startTime Unix timestamp when claiming begins
     * @param _endTime Unix timestamp when claiming ends
     * @param _totalAmount Total amount of tokens to be distributed
     * @param _claimDelay Seconds between initiate and claim (0 for instant)
     * @param _metadataCID IPFS CID containing the full claim list
     */
    constructor(
        address _token,
        bytes32 _merkleRoot,
        uint256 _startTime,
        uint256 _endTime,
        uint256 _totalAmount,
        uint256 _claimDelay,
        string memory _metadataCID
    ) Ownable(msg.sender) {
        if (_token == address(0)) revert ZeroAddress();
        if (_totalAmount == 0) revert InvalidAmount();
        if (_startTime >= _endTime) revert InvalidTimeRange();
        
        config = AirdropConfig({
            token: IERC20(_token),
            merkleRoot: _merkleRoot,
            startTime: _startTime,
            endTime: _endTime,
            totalAmount: _totalAmount,
            claimedAmount: 0,
            claimDelay: _claimDelay,
            isPaused: false,
            metadataCID: _metadataCID
        });
    }
    
    // ============ External Functions ============
    
    /**
     * @notice Claim airdrop tokens (instant if no delay configured)
     * @param amount Amount to claim
     * @param proof Merkle proof for the claim
     */
    function claim(uint256 amount, bytes32[] calldata proof) external nonReentrant {
        _validateClaimConditions();
        
        if (hasClaimed[msg.sender]) {
            revert AlreadyClaimed(msg.sender);
        }
        
        // Handle claim delay if configured
        if (config.claimDelay > 0) {
            uint256 initiatedAt = claimInitiatedAt[msg.sender];
            if (initiatedAt == 0) {
                revert ClaimNotInitiated(msg.sender);
            }
            if (block.timestamp < initiatedAt + config.claimDelay) {
                revert ClaimDelayNotMet(initiatedAt, config.claimDelay);
            }
        }
        
        // Verify Merkle proof
        bytes32 leaf = keccak256(bytes.concat(keccak256(abi.encode(msg.sender, amount))));
        if (!MerkleProof.verify(proof, config.merkleRoot, leaf)) {
            revert InvalidProof();
        }
        
        // Mark as claimed and transfer
        hasClaimed[msg.sender] = true;
        config.claimedAmount += amount;
        totalClaims++;
        
        config.token.safeTransfer(msg.sender, amount);
        
        emit Claimed(msg.sender, amount, block.timestamp);
    }
    
    /**
     * @notice Initiate a claim (required if claim delay > 0)
     * @dev Registers intent to claim, actual claim can happen after delay
     * @param amount Amount to claim (for verification)
     * @param proof Merkle proof for the claim
     */
    function initiateClaim(uint256 amount, bytes32[] calldata proof) external {
        _validateClaimConditions();
        
        if (hasClaimed[msg.sender]) {
            revert AlreadyClaimed(msg.sender);
        }
        
        // Verify Merkle proof early to prevent spam
        bytes32 leaf = keccak256(bytes.concat(keccak256(abi.encode(msg.sender, amount))));
        if (!MerkleProof.verify(proof, config.merkleRoot, leaf)) {
            revert InvalidProof();
        }
        
        claimInitiatedAt[msg.sender] = block.timestamp;
        
        emit ClaimInitiated(msg.sender, block.timestamp);
    }
    
    /**
     * @notice Check if an address can claim
     * @param account Address to check
     * @param amount Claimed amount
     * @param proof Merkle proof
     * @return canClaim Whether the address can claim
     * @return reason Explanation if cannot claim
     */
    function canClaim(
        address account,
        uint256 amount,
        bytes32[] calldata proof
    ) external view returns (bool canClaim, string memory reason) {
        if (config.isPaused) {
            return (false, "Airdrop is paused");
        }
        if (block.timestamp < config.startTime) {
            return (false, "Airdrop has not started");
        }
        if (block.timestamp > config.endTime) {
            return (false, "Airdrop has ended");
        }
        if (hasClaimed[account]) {
            return (false, "Already claimed");
        }
        
        // Verify proof
        bytes32 leaf = keccak256(bytes.concat(keccak256(abi.encode(account, amount))));
        if (!MerkleProof.verify(proof, config.merkleRoot, leaf)) {
            return (false, "Invalid proof");
        }
        
        // Check delay requirement
        if (config.claimDelay > 0) {
            uint256 initiatedAt = claimInitiatedAt[account];
            if (initiatedAt == 0) {
                return (false, "Claim not initiated");
            }
            if (block.timestamp < initiatedAt + config.claimDelay) {
                return (false, "Claim delay not met");
            }
        }
        
        return (true, "");
    }
    
    /**
     * @notice Get claim status for an address
     * @param account Address to check
     * @return claimed Whether tokens were claimed
     * @return initiatedAt When claim was initiated (0 if not)
     * @return delayRemaining Seconds until claim is available (0 if ready)
     */
    function getClaimStatus(address account) 
        external 
        view 
        returns (
            bool claimed,
            uint256 initiatedAt,
            uint256 delayRemaining
        ) 
    {
        claimed = hasClaimed[account];
        initiatedAt = claimInitiatedAt[account];
        
        if (initiatedAt > 0 && !claimed) {
            uint256 claimAvailableAt = initiatedAt + config.claimDelay;
            if (block.timestamp < claimAvailableAt) {
                delayRemaining = claimAvailableAt - block.timestamp;
            }
        }
    }
    
    /**
     * @notice Get airdrop statistics
     * @return token Token address
     * @return totalAmount Total tokens in airdrop
     * @return claimedAmount Tokens already claimed
     * @return remainingAmount Tokens still to be claimed
     * @return claims Number of claims made
     * @return isActive Whether airdrop is currently active
     */
    function getStats() 
        external 
        view 
        returns (
            address token,
            uint256 totalAmount,
            uint256 claimedAmount,
            uint256 remainingAmount,
            uint256 claims,
            bool isActive
        ) 
    {
        token = address(config.token);
        totalAmount = config.totalAmount;
        claimedAmount = config.claimedAmount;
        remainingAmount = totalAmount - claimedAmount;
        claims = totalClaims;
        isActive = !config.isPaused && 
                   block.timestamp >= config.startTime && 
                   block.timestamp <= config.endTime;
    }
    
    // ============ Admin Functions ============
    
    /**
     * @notice Pause or unpause the airdrop
     * @param paused New pause state
     */
    function setPaused(bool paused) external onlyOwner {
        config.isPaused = paused;
        emit PauseStateChanged(paused);
    }
    
    /**
     * @notice Update Merkle root for new claim round
     * @dev Use with caution - may invalidate existing unclaimed allocations
     * @param newRoot New Merkle root
     */
    function updateMerkleRoot(bytes32 newRoot) external onlyOwner {
        bytes32 oldRoot = config.merkleRoot;
        config.merkleRoot = newRoot;
        emit MerkleRootUpdated(oldRoot, newRoot);
    }
    
    /**
     * @notice Recover unclaimed tokens after airdrop ends
     * @param to Recipient address
     */
    function recoverTokens(address to) external onlyOwner {
        if (to == address(0)) revert ZeroAddress();
        if (block.timestamp <= config.endTime) {
            revert AirdropNotStarted(config.endTime, block.timestamp);
        }
        
        uint256 balance = config.token.balanceOf(address(this));
        if (balance > 0) {
            config.token.safeTransfer(to, balance);
            emit TokensRecovered(address(config.token), to, balance);
        }
    }
    
    /**
     * @notice Emergency recovery for any token (not the airdrop token)
     * @param token Token to recover
     * @param to Recipient address
     */
    function emergencyRecoverToken(address token, address to) external onlyOwner {
        if (to == address(0)) revert ZeroAddress();
        // Cannot emergency recover the airdrop token until airdrop ends
        if (token == address(config.token) && block.timestamp <= config.endTime) {
            revert AirdropNotStarted(config.endTime, block.timestamp);
        }
        
        uint256 balance = IERC20(token).balanceOf(address(this));
        if (balance > 0) {
            IERC20(token).safeTransfer(to, balance);
            emit TokensRecovered(token, to, balance);
        }
    }
    
    // ============ Internal Functions ============
    
    function _validateClaimConditions() internal view {
        if (config.isPaused) {
            revert AirdropPaused();
        }
        if (block.timestamp < config.startTime) {
            revert AirdropNotStarted(config.startTime, block.timestamp);
        }
        if (block.timestamp > config.endTime) {
            revert AirdropEnded(config.endTime, block.timestamp);
        }
    }
}

/**
 * @title LaunchLabAirdropFactory
 * @author Datachain Foundation
 * @notice Factory for deploying airdrop contracts
 */
contract LaunchLabAirdropFactory is Ownable {
    using SafeERC20 for IERC20;
    
    // ============ State Variables ============
    
    /// @notice Array of all deployed airdrops
    address[] public airdrops;
    
    /// @notice Mapping from project to their airdrops
    mapping(address => address[]) public projectAirdrops;
    
    /// @notice Protocol fee for creating airdrop (in native token)
    uint256 public creationFee;
    
    /// @notice Treasury address for fees
    address public treasury;
    
    // ============ Events ============
    
    event AirdropCreated(
        address indexed airdrop,
        address indexed token,
        address indexed creator,
        uint256 totalAmount,
        bytes32 merkleRoot
    );
    
    event CreationFeeUpdated(uint256 oldFee, uint256 newFee);
    event TreasuryUpdated(address oldTreasury, address newTreasury);
    
    // ============ Errors ============
    
    error InsufficientFee(uint256 sent, uint256 required);
    error ZeroAddress();
    error WithdrawalFailed();
    error TransferFailed();
    
    // ============ Constructor ============
    
    constructor(address _treasury, uint256 _creationFee) Ownable(msg.sender) {
        if (_treasury == address(0)) revert ZeroAddress();
        treasury = _treasury;
        creationFee = _creationFee;
    }
    
    // ============ External Functions ============
    
    /**
     * @notice Create a new airdrop
     * @param token Token to distribute
     * @param merkleRoot Merkle root of claims
     * @param startTime When claiming starts
     * @param endTime When claiming ends
     * @param totalAmount Total tokens to distribute
     * @param claimDelay Delay between initiate and claim
     * @param metadataCID IPFS CID with claim list
     * @return airdrop Address of deployed airdrop contract
     */
    function createAirdrop(
        address token,
        bytes32 merkleRoot,
        uint256 startTime,
        uint256 endTime,
        uint256 totalAmount,
        uint256 claimDelay,
        string calldata metadataCID
    ) external payable returns (address airdrop) {
        if (msg.value < creationFee) {
            revert InsufficientFee(msg.value, creationFee);
        }
        
        // Deploy airdrop contract
        LaunchLabAirdrop newAirdrop = new LaunchLabAirdrop(
            token,
            merkleRoot,
            startTime,
            endTime,
            totalAmount,
            claimDelay,
            metadataCID
        );
        
        airdrop = address(newAirdrop);
        
        // Transfer ownership to creator
        newAirdrop.transferOwnership(msg.sender);
        
        // Record deployment
        airdrops.push(airdrop);
        projectAirdrops[msg.sender].push(airdrop);
        
        // Transfer tokens from creator to airdrop contract
        IERC20(token).safeTransferFrom(msg.sender, airdrop, totalAmount);
        
        // Transfer fee to treasury
        if (msg.value > 0) {
            (bool success, ) = treasury.call{value: msg.value}("");
            if (!success) revert WithdrawalFailed();
        }
        
        emit AirdropCreated(airdrop, token, msg.sender, totalAmount, merkleRoot);
    }
    
    /**
     * @notice Get number of airdrops created
     */
    function airdropCount() external view returns (uint256) {
        return airdrops.length;
    }
    
    /**
     * @notice Get airdrops for a project
     * @param project Project owner address
     */
    function getProjectAirdrops(address project) external view returns (address[] memory) {
        return projectAirdrops[project];
    }
    
    // ============ Admin Functions ============
    
    function setCreationFee(uint256 newFee) external onlyOwner {
        uint256 oldFee = creationFee;
        creationFee = newFee;
        emit CreationFeeUpdated(oldFee, newFee);
    }
    
    function setTreasury(address newTreasury) external onlyOwner {
        if (newTreasury == address(0)) revert ZeroAddress();
        address oldTreasury = treasury;
        treasury = newTreasury;
        emit TreasuryUpdated(oldTreasury, newTreasury);
    }
}
