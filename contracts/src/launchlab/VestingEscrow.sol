// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import "@openzeppelin/contracts/access/Ownable.sol";

/**
 * @title VestingEscrow
 * @author Datachain Foundation
 * @notice Escrow contract for OTC deals with vesting schedules
 * @dev Supports TGE unlock, cliff periods, and linear vesting. Used for
 *      private sales, team allocations, and investor deals.
 * 
 * Features:
 * - Configurable TGE (Token Generation Event) unlock percentage
 * - Cliff period with optional cliff unlock
 * - Linear vesting after cliff
 * - Revocable by admin (returns unvested tokens to owner)
 * - Beneficiary change support (for entity restructuring)
 */
contract VestingEscrow is ReentrancyGuard, Ownable {
    using SafeERC20 for IERC20;
    
    // ============ Structs ============
    
    struct VestingSchedule {
        uint256 totalAmount;           // Total tokens in vesting
        uint256 releasedAmount;        // Already released tokens
        uint256 startTime;             // Vesting start timestamp
        uint256 cliffDuration;         // Cliff period in seconds
        uint256 vestingDuration;       // Total vesting duration after cliff
        uint256 tgeUnlockBps;          // TGE unlock in basis points (10000 = 100%)
        uint256 cliffUnlockBps;        // Cliff unlock in basis points
        bool revocable;                // Whether vesting can be revoked
        bool revoked;                  // Whether vesting has been revoked
    }
    
    // ============ State Variables ============
    
    /// @notice Token being vested
    IERC20 public immutable token;
    
    /// @notice Beneficiary receiving vested tokens
    address public beneficiary;
    
    /// @notice Vesting schedule configuration
    VestingSchedule public schedule;
    
    /// @notice Timestamp when TGE was claimed (0 if not claimed)
    uint256 public tgeClaimedAt;
    
    /// @notice Timestamp when cliff was claimed (0 if not claimed)
    uint256 public cliffClaimedAt;
    
    // ============ Events ============
    
    event TokensReleased(address indexed beneficiary, uint256 amount);
    event TGEClaimed(address indexed beneficiary, uint256 amount);
    event CliffClaimed(address indexed beneficiary, uint256 amount);
    event VestingRevoked(uint256 unvestedAmount, uint256 timestamp);
    event BeneficiaryChanged(address indexed oldBeneficiary, address indexed newBeneficiary);
    
    // ============ Errors ============
    
    error ZeroAddress();
    error InvalidAmount();
    error InvalidSchedule();
    error VestingNotStarted();
    error CliffNotReached();
    error NothingToRelease();
    error NotRevocable();
    error AlreadyRevoked();
    error OnlyBeneficiary();
    error TGEAlreadyClaimed();
    error CliffAlreadyClaimed();
    error InvalidBps();
    
    // ============ Modifiers ============
    
    modifier onlyBeneficiary() {
        if (msg.sender != beneficiary) revert OnlyBeneficiary();
        _;
    }
    
    modifier notRevoked() {
        if (schedule.revoked) revert AlreadyRevoked();
        _;
    }
    
    // ============ Constructor ============
    
    /**
     * @notice Create a new vesting escrow
     * @param _token Token to vest
     * @param _beneficiary Address receiving vested tokens
     * @param _totalAmount Total tokens in vesting
     * @param _startTime Vesting start timestamp
     * @param _cliffDuration Cliff duration in seconds
     * @param _vestingDuration Vesting duration after cliff in seconds
     * @param _tgeUnlockBps TGE unlock percentage in basis points
     * @param _cliffUnlockBps Cliff unlock percentage in basis points
     * @param _revocable Whether vesting can be revoked
     */
    constructor(
        address _token,
        address _beneficiary,
        uint256 _totalAmount,
        uint256 _startTime,
        uint256 _cliffDuration,
        uint256 _vestingDuration,
        uint256 _tgeUnlockBps,
        uint256 _cliffUnlockBps,
        bool _revocable
    ) Ownable(msg.sender) {
        if (_token == address(0)) revert ZeroAddress();
        if (_beneficiary == address(0)) revert ZeroAddress();
        if (_totalAmount == 0) revert InvalidAmount();
        if (_tgeUnlockBps > 10000) revert InvalidBps();
        if (_cliffUnlockBps > 10000) revert InvalidBps();
        if (_tgeUnlockBps + _cliffUnlockBps > 10000) revert InvalidBps();
        if (_vestingDuration == 0 && _tgeUnlockBps + _cliffUnlockBps < 10000) {
            revert InvalidSchedule();
        }
        
        token = IERC20(_token);
        beneficiary = _beneficiary;
        
        schedule = VestingSchedule({
            totalAmount: _totalAmount,
            releasedAmount: 0,
            startTime: _startTime,
            cliffDuration: _cliffDuration,
            vestingDuration: _vestingDuration,
            tgeUnlockBps: _tgeUnlockBps,
            cliffUnlockBps: _cliffUnlockBps,
            revocable: _revocable,
            revoked: false
        });
    }
    
    // ============ External Functions ============
    
    /**
     * @notice Claim TGE unlock
     * @dev Can only be called once, at or after start time
     */
    function claimTGE() external nonReentrant onlyBeneficiary notRevoked {
        if (schedule.tgeUnlockBps == 0) revert NothingToRelease();
        if (tgeClaimedAt != 0) revert TGEAlreadyClaimed();
        if (block.timestamp < schedule.startTime) revert VestingNotStarted();
        
        uint256 tgeAmount = (schedule.totalAmount * schedule.tgeUnlockBps) / 10000;
        
        tgeClaimedAt = block.timestamp;
        schedule.releasedAmount += tgeAmount;
        
        token.safeTransfer(beneficiary, tgeAmount);
        
        emit TGEClaimed(beneficiary, tgeAmount);
    }
    
    /**
     * @notice Claim cliff unlock
     * @dev Can only be called once, after cliff period
     */
    function claimCliff() external nonReentrant onlyBeneficiary notRevoked {
        if (schedule.cliffUnlockBps == 0) revert NothingToRelease();
        if (cliffClaimedAt != 0) revert CliffAlreadyClaimed();
        
        uint256 cliffEnd = schedule.startTime + schedule.cliffDuration;
        if (block.timestamp < cliffEnd) revert CliffNotReached();
        
        uint256 cliffAmount = (schedule.totalAmount * schedule.cliffUnlockBps) / 10000;
        
        cliffClaimedAt = block.timestamp;
        schedule.releasedAmount += cliffAmount;
        
        token.safeTransfer(beneficiary, cliffAmount);
        
        emit CliffClaimed(beneficiary, cliffAmount);
    }
    
    /**
     * @notice Release vested tokens
     * @dev Releases all available vested tokens to beneficiary
     */
    function release() external nonReentrant onlyBeneficiary notRevoked {
        uint256 releasable = getReleasableAmount();
        if (releasable == 0) revert NothingToRelease();
        
        schedule.releasedAmount += releasable;
        
        token.safeTransfer(beneficiary, releasable);
        
        emit TokensReleased(beneficiary, releasable);
    }
    
    /**
     * @notice Release specific amount of vested tokens
     * @param amount Amount to release
     */
    function releaseAmount(uint256 amount) external nonReentrant onlyBeneficiary notRevoked {
        uint256 releasable = getReleasableAmount();
        if (amount > releasable) revert NothingToRelease();
        
        schedule.releasedAmount += amount;
        
        token.safeTransfer(beneficiary, amount);
        
        emit TokensReleased(beneficiary, amount);
    }
    
    // ============ View Functions ============
    
    /**
     * @notice Get currently releasable amount
     * @return Amount that can be released now
     */
    function getReleasableAmount() public view returns (uint256) {
        if (schedule.revoked) return 0;
        return getVestedAmount() - schedule.releasedAmount;
    }
    
    /**
     * @notice Get total vested amount (including already released)
     * @return Total amount that has vested
     */
    function getVestedAmount() public view returns (uint256) {
        if (schedule.revoked) {
            return schedule.releasedAmount;
        }
        
        if (block.timestamp < schedule.startTime) {
            return 0;
        }
        
        // Calculate TGE amount (available at start)
        uint256 tgeAmount = (schedule.totalAmount * schedule.tgeUnlockBps) / 10000;
        
        // Before cliff ends, only TGE is vested
        uint256 cliffEnd = schedule.startTime + schedule.cliffDuration;
        if (block.timestamp < cliffEnd) {
            return tgeAmount;
        }
        
        // Cliff amount (available at cliff end)
        uint256 cliffAmount = (schedule.totalAmount * schedule.cliffUnlockBps) / 10000;
        
        // If no vesting period, everything is vested at cliff
        if (schedule.vestingDuration == 0) {
            return schedule.totalAmount;
        }
        
        // Calculate linear vesting
        uint256 vestingAmount = schedule.totalAmount - tgeAmount - cliffAmount;
        uint256 timeSinceCliff = block.timestamp - cliffEnd;
        
        if (timeSinceCliff >= schedule.vestingDuration) {
            return schedule.totalAmount;
        }
        
        uint256 vestedLinear = (vestingAmount * timeSinceCliff) / schedule.vestingDuration;
        
        return tgeAmount + cliffAmount + vestedLinear;
    }
    
    /**
     * @notice Get unvested amount
     * @return Amount still locked
     */
    function getUnvestedAmount() external view returns (uint256) {
        if (schedule.revoked) return 0;
        return schedule.totalAmount - getVestedAmount();
    }
    
    /**
     * @notice Get schedule details
     */
    function getScheduleDetails() external view returns (
        uint256 totalAmount,
        uint256 releasedAmount,
        uint256 vestedAmount,
        uint256 releasableAmount,
        uint256 startTime,
        uint256 cliffEnd,
        uint256 vestingEnd,
        bool revoked
    ) {
        totalAmount = schedule.totalAmount;
        releasedAmount = schedule.releasedAmount;
        vestedAmount = getVestedAmount();
        releasableAmount = getReleasableAmount();
        startTime = schedule.startTime;
        cliffEnd = schedule.startTime + schedule.cliffDuration;
        vestingEnd = cliffEnd + schedule.vestingDuration;
        revoked = schedule.revoked;
    }
    
    /**
     * @notice Get unlock milestones
     */
    function getMilestones() external view returns (
        uint256 tgeAmount,
        uint256 cliffAmount,
        uint256 vestingAmount,
        bool tgeClaimed,
        bool cliffClaimed
    ) {
        tgeAmount = (schedule.totalAmount * schedule.tgeUnlockBps) / 10000;
        cliffAmount = (schedule.totalAmount * schedule.cliffUnlockBps) / 10000;
        vestingAmount = schedule.totalAmount - tgeAmount - cliffAmount;
        tgeClaimed = tgeClaimedAt != 0;
        cliffClaimed = cliffClaimedAt != 0;
    }
    
    // ============ Admin Functions ============
    
    /**
     * @notice Revoke vesting and return unvested tokens to owner
     * @dev Only works if schedule is revocable
     */
    function revoke() external onlyOwner notRevoked {
        if (!schedule.revocable) revert NotRevocable();
        
        uint256 unvested = schedule.totalAmount - getVestedAmount();
        schedule.revoked = true;
        
        if (unvested > 0) {
            token.safeTransfer(owner(), unvested);
        }
        
        emit VestingRevoked(unvested, block.timestamp);
    }
    
    /**
     * @notice Change beneficiary address
     * @dev Useful for entity restructuring or wallet recovery
     * @param newBeneficiary New beneficiary address
     */
    function changeBeneficiary(address newBeneficiary) external onlyOwner {
        if (newBeneficiary == address(0)) revert ZeroAddress();
        
        address oldBeneficiary = beneficiary;
        beneficiary = newBeneficiary;
        
        emit BeneficiaryChanged(oldBeneficiary, newBeneficiary);
    }
}

/**
 * @title VestingEscrowFactory
 * @author Datachain Foundation
 * @notice Factory for deploying vesting escrow contracts
 */
contract VestingEscrowFactory is Ownable {
    using SafeERC20 for IERC20;
    
    // ============ State Variables ============
    
    /// @notice All deployed escrows
    address[] public escrows;
    
    /// @notice Escrows by creator
    mapping(address => address[]) public creatorEscrows;
    
    /// @notice Escrows by beneficiary
    mapping(address => address[]) public beneficiaryEscrows;
    
    /// @notice Protocol fee for creating escrow
    uint256 public creationFee;
    
    /// @notice Treasury address
    address public treasury;
    
    // ============ Events ============
    
    event EscrowCreated(
        address indexed escrow,
        address indexed token,
        address indexed beneficiary,
        address creator,
        uint256 totalAmount
    );
    
    event CreationFeeUpdated(uint256 oldFee, uint256 newFee);
    event TreasuryUpdated(address oldTreasury, address newTreasury);
    
    // ============ Errors ============
    
    error InsufficientFee(uint256 sent, uint256 required);
    error ZeroAddress();
    error WithdrawalFailed();
    
    // ============ Constructor ============
    
    constructor(address _treasury, uint256 _creationFee) Ownable(msg.sender) {
        if (_treasury == address(0)) revert ZeroAddress();
        treasury = _treasury;
        creationFee = _creationFee;
    }
    
    // ============ External Functions ============
    
    /**
     * @notice Create a new vesting escrow
     * @param token Token to vest
     * @param beneficiary Beneficiary address
     * @param totalAmount Total tokens
     * @param startTime Start timestamp
     * @param cliffDuration Cliff in seconds
     * @param vestingDuration Vesting after cliff in seconds
     * @param tgeUnlockBps TGE unlock in bps
     * @param cliffUnlockBps Cliff unlock in bps
     * @param revocable Whether revocable
     * @return escrow Deployed escrow address
     */
    function createEscrow(
        address token,
        address beneficiary,
        uint256 totalAmount,
        uint256 startTime,
        uint256 cliffDuration,
        uint256 vestingDuration,
        uint256 tgeUnlockBps,
        uint256 cliffUnlockBps,
        bool revocable
    ) external payable returns (address escrow) {
        if (msg.value < creationFee) {
            revert InsufficientFee(msg.value, creationFee);
        }
        
        VestingEscrow newEscrow = new VestingEscrow(
            token,
            beneficiary,
            totalAmount,
            startTime,
            cliffDuration,
            vestingDuration,
            tgeUnlockBps,
            cliffUnlockBps,
            revocable
        );
        
        escrow = address(newEscrow);
        
        // Transfer ownership to creator
        newEscrow.transferOwnership(msg.sender);
        
        // Record
        escrows.push(escrow);
        creatorEscrows[msg.sender].push(escrow);
        beneficiaryEscrows[beneficiary].push(escrow);
        
        // Fund escrow
        IERC20(token).safeTransferFrom(msg.sender, escrow, totalAmount);
        
        // Transfer fee
        if (msg.value > 0) {
            (bool success, ) = treasury.call{value: msg.value}("");
            if (!success) revert WithdrawalFailed();
        }
        
        emit EscrowCreated(escrow, token, beneficiary, msg.sender, totalAmount);
    }
    
    /**
     * @notice Get total escrow count
     */
    function escrowCount() external view returns (uint256) {
        return escrows.length;
    }
    
    /**
     * @notice Get escrows created by an address
     */
    function getCreatorEscrows(address creator) external view returns (address[] memory) {
        return creatorEscrows[creator];
    }
    
    /**
     * @notice Get escrows for a beneficiary
     */
    function getBeneficiaryEscrows(address beneficiary) external view returns (address[] memory) {
        return beneficiaryEscrows[beneficiary];
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
