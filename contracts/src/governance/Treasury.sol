// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import "@openzeppelin/contracts/access/AccessControl.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import "@openzeppelin/contracts/utils/Pausable.sol";

/**
 * @title DatachainTreasury
 * @notice Treasury management contract for Datachain DAO
 * @dev Manages protocol funds with multi-sig and governance control
 * 
 * Features:
 * - Multi-sig spending approval
 * - Governance-controlled spending limits
 * - Emergency pause functionality
 * - Token and native asset management
 * - Spending categories and tracking
 */
contract DatachainTreasury is AccessControl, ReentrancyGuard, Pausable {
    using SafeERC20 for IERC20;

    /// @notice Role for governance actions
    bytes32 public constant GOVERNANCE_ROLE = keccak256("GOVERNANCE_ROLE");
    
    /// @notice Role for spending (multi-sig)
    bytes32 public constant SPENDER_ROLE = keccak256("SPENDER_ROLE");
    
    /// @notice Role for emergency actions
    bytes32 public constant GUARDIAN_ROLE = keccak256("GUARDIAN_ROLE");

    /// @notice Spending limit per transaction (in wei)
    uint256 public spendingLimit;

    /// @notice Daily spending limit (in wei)
    uint256 public dailyLimit;

    /// @notice Current day's spending
    uint256 public dailySpent;

    /// @notice Last spending day timestamp
    uint256 public lastSpendingDay;

    /// @notice Spending category
    enum SpendingCategory {
        Operations,
        Development,
        Marketing,
        Grants,
        Security,
        Legal,
        Other
    }

    /// @notice Spending record
    struct SpendingRecord {
        address token;
        address recipient;
        uint256 amount;
        SpendingCategory category;
        string description;
        uint256 timestamp;
        address approver;
    }

    /// @notice All spending records
    SpendingRecord[] public spendingHistory;

    /// @notice Total spent per category
    mapping(SpendingCategory => uint256) public categoryTotals;

    /// @notice Events
    event SpendingLimitUpdated(uint256 oldLimit, uint256 newLimit);
    event DailyLimitUpdated(uint256 oldLimit, uint256 newLimit);
    event FundsSpent(
        address indexed token,
        address indexed recipient,
        uint256 amount,
        SpendingCategory category,
        string description
    );
    event FundsReceived(address indexed token, address indexed from, uint256 amount);
    event EmergencyWithdrawal(address indexed token, address indexed recipient, uint256 amount);

    /**
     * @notice Initialize the treasury
     * @param _governance The governance address (DAO timelock)
     * @param _spendingLimit Per-transaction spending limit
     * @param _dailyLimit Daily spending limit
     */
    constructor(
        address _governance,
        uint256 _spendingLimit,
        uint256 _dailyLimit
    ) {
        require(_governance != address(0), "Invalid governance");
        
        _grantRole(DEFAULT_ADMIN_ROLE, _governance);
        _grantRole(GOVERNANCE_ROLE, _governance);
        
        spendingLimit = _spendingLimit;
        dailyLimit = _dailyLimit;
        lastSpendingDay = block.timestamp / 1 days;
    }

    /**
     * @notice Receive native tokens
     */
    receive() external payable {
        emit FundsReceived(address(0), msg.sender, msg.value);
    }

    /**
     * @notice Spend native tokens
     * @param recipient The recipient address
     * @param amount The amount to send
     * @param category The spending category
     * @param description Description of the spending
     */
    function spend(
        address payable recipient,
        uint256 amount,
        SpendingCategory category,
        string calldata description
    ) external onlyRole(SPENDER_ROLE) nonReentrant whenNotPaused {
        require(recipient != address(0), "Invalid recipient");
        require(amount > 0, "Amount must be positive");
        require(amount <= spendingLimit, "Exceeds spending limit");
        
        _checkDailyLimit(amount);
        
        require(address(this).balance >= amount, "Insufficient balance");
        
        // Record spending
        spendingHistory.push(SpendingRecord({
            token: address(0),
            recipient: recipient,
            amount: amount,
            category: category,
            description: description,
            timestamp: block.timestamp,
            approver: msg.sender
        }));
        
        categoryTotals[category] += amount;
        
        // Transfer
        (bool success, ) = recipient.call{value: amount}("");
        require(success, "Transfer failed");
        
        emit FundsSpent(address(0), recipient, amount, category, description);
    }

    /**
     * @notice Spend ERC20 tokens
     * @param token The token address
     * @param recipient The recipient address
     * @param amount The amount to send
     * @param category The spending category
     * @param description Description of the spending
     */
    function spendToken(
        address token,
        address recipient,
        uint256 amount,
        SpendingCategory category,
        string calldata description
    ) external onlyRole(SPENDER_ROLE) nonReentrant whenNotPaused {
        require(token != address(0), "Invalid token");
        require(recipient != address(0), "Invalid recipient");
        require(amount > 0, "Amount must be positive");
        require(amount <= spendingLimit, "Exceeds spending limit");
        
        _checkDailyLimit(amount);
        
        // Record spending
        spendingHistory.push(SpendingRecord({
            token: token,
            recipient: recipient,
            amount: amount,
            category: category,
            description: description,
            timestamp: block.timestamp,
            approver: msg.sender
        }));
        
        categoryTotals[category] += amount;
        
        // Transfer
        IERC20(token).safeTransfer(recipient, amount);
        
        emit FundsSpent(token, recipient, amount, category, description);
    }

    /**
     * @notice Check and update daily limit
     */
    function _checkDailyLimit(uint256 amount) internal {
        uint256 currentDay = block.timestamp / 1 days;
        
        if (currentDay > lastSpendingDay) {
            // Reset daily spending
            dailySpent = 0;
            lastSpendingDay = currentDay;
        }
        
        require(dailySpent + amount <= dailyLimit, "Exceeds daily limit");
        dailySpent += amount;
    }

    /**
     * @notice Update spending limit
     * @param newLimit The new limit
     */
    function setSpendingLimit(uint256 newLimit) external onlyRole(GOVERNANCE_ROLE) {
        uint256 oldLimit = spendingLimit;
        spendingLimit = newLimit;
        emit SpendingLimitUpdated(oldLimit, newLimit);
    }

    /**
     * @notice Update daily limit
     * @param newLimit The new limit
     */
    function setDailyLimit(uint256 newLimit) external onlyRole(GOVERNANCE_ROLE) {
        uint256 oldLimit = dailyLimit;
        dailyLimit = newLimit;
        emit DailyLimitUpdated(oldLimit, newLimit);
    }

    /**
     * @notice Emergency withdrawal (guardian only)
     * @param token The token address (0x0 for native)
     * @param recipient The recipient address
     * @param amount The amount to withdraw
     */
    function emergencyWithdraw(
        address token,
        address payable recipient,
        uint256 amount
    ) external onlyRole(GUARDIAN_ROLE) nonReentrant {
        require(recipient != address(0), "Invalid recipient");
        
        if (token == address(0)) {
            require(address(this).balance >= amount, "Insufficient balance");
            (bool success, ) = recipient.call{value: amount}("");
            require(success, "Transfer failed");
        } else {
            IERC20(token).safeTransfer(recipient, amount);
        }
        
        emit EmergencyWithdrawal(token, recipient, amount);
    }

    /**
     * @notice Pause the treasury
     */
    function pause() external onlyRole(GUARDIAN_ROLE) {
        _pause();
    }

    /**
     * @notice Unpause the treasury
     */
    function unpause() external onlyRole(GOVERNANCE_ROLE) {
        _unpause();
    }

    /**
     * @notice Get spending history count
     */
    function getSpendingCount() external view returns (uint256) {
        return spendingHistory.length;
    }

    /**
     * @notice Get spending records in range
     */
    function getSpendingRecords(
        uint256 start,
        uint256 count
    ) external view returns (SpendingRecord[] memory) {
        uint256 end = start + count;
        if (end > spendingHistory.length) {
            end = spendingHistory.length;
        }
        
        SpendingRecord[] memory records = new SpendingRecord[](end - start);
        for (uint256 i = start; i < end; i++) {
            records[i - start] = spendingHistory[i];
        }
        
        return records;
    }

    /**
     * @notice Get balance of a token
     */
    function getBalance(address token) external view returns (uint256) {
        if (token == address(0)) {
            return address(this).balance;
        }
        return IERC20(token).balanceOf(address(this));
    }
}
