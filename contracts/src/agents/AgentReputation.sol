// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import "@openzeppelin/contracts/access/AccessControl.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

/**
 * @title AgentReputation
 * @notice On-chain reputation system for AI agents with staking and slashing
 * @dev Manages agent registration, reputation scoring, and slashing for misbehavior
 * 
 * Features:
 * - Agent registration with stake requirement
 * - Reputation scoring (0-1000)
 * - Slashing for violations
 * - Reputation decay and recovery
 * - Agent deactivation for repeated violations
 */
contract AgentReputation is AccessControl, ReentrancyGuard {
    using SafeERC20 for IERC20;

    /// @notice Role for slashing agents
    bytes32 public constant SLASHER_ROLE = keccak256("SLASHER_ROLE");
    
    /// @notice Role for governance
    bytes32 public constant GOVERNANCE_ROLE = keccak256("GOVERNANCE_ROLE");

    /// @notice Maximum reputation score
    uint256 public constant MAX_REPUTATION = 1000;
    
    /// @notice Minimum reputation for participation
    uint256 public constant MIN_REPUTATION = 100;
    
    /// @notice Initial reputation for new agents
    uint256 public constant INITIAL_REPUTATION = 500;

    /// @notice Staking token (FAT)
    IERC20 public immutable stakingToken;
    
    /// @notice Minimum stake required
    uint256 public minStake;
    
    /// @notice Maximum violations before deactivation
    uint256 public maxViolations;
    
    /// @notice Reputation recovery per epoch
    uint256 public recoveryRate;
    
    /// @notice Epoch duration (seconds)
    uint256 public epochDuration;

    /// @notice Violation types
    enum ViolationType {
        InvalidTestimony,    // 5% slash
        DoubleVoting,        // 50% slash
        Downtime,            // 1% slash
        MaliciousBehavior,   // 100% slash
        Spam,                // 10% slash
        TaskFailure,         // 2% slash
        Collusion,           // 100% slash
        DataCorruption       // 25% slash
    }

    /// @notice Agent record
    struct Agent {
        address owner;
        uint256 stake;
        uint256 reputation;
        uint256 positiveActions;
        uint256 negativeActions;
        uint256 totalSlashed;
        uint256 lastActivity;
        uint256 registeredAt;
        uint256 violationCount;
        bool active;
    }

    /// @notice Violation record
    struct Violation {
        bytes32 agentId;
        ViolationType violationType;
        uint256 slashAmount;
        uint256 newReputation;
        string evidence;
        uint256 timestamp;
        address reporter;
    }

    /// @notice Agent records
    mapping(bytes32 => Agent) public agents;
    
    /// @notice Violation history per agent
    mapping(bytes32 => Violation[]) public violations;
    
    /// @notice Total slashed amount
    uint256 public totalSlashed;
    
    /// @notice Agent count
    uint256 public agentCount;

    /// @notice Events
    event AgentRegistered(bytes32 indexed agentId, address indexed owner, uint256 stake);
    event AgentDeactivated(bytes32 indexed agentId, string reason);
    event ReputationUpdated(bytes32 indexed agentId, uint256 oldRep, uint256 newRep);
    event Slashed(
        bytes32 indexed agentId,
        ViolationType violationType,
        uint256 amount,
        uint256 newReputation
    );
    event StakeAdded(bytes32 indexed agentId, uint256 amount);
    event StakeWithdrawn(bytes32 indexed agentId, uint256 amount);

    /**
     * @notice Initialize the reputation system
     * @param _stakingToken The staking token address
     * @param _minStake Minimum stake requirement
     * @param _maxViolations Maximum violations before deactivation
     * @param _recoveryRate Reputation recovery per epoch
     * @param _epochDuration Epoch duration in seconds
     */
    constructor(
        address _stakingToken,
        uint256 _minStake,
        uint256 _maxViolations,
        uint256 _recoveryRate,
        uint256 _epochDuration
    ) {
        require(_stakingToken != address(0), "Invalid token");
        
        stakingToken = IERC20(_stakingToken);
        minStake = _minStake;
        maxViolations = _maxViolations;
        recoveryRate = _recoveryRate;
        epochDuration = _epochDuration;
        
        _grantRole(DEFAULT_ADMIN_ROLE, msg.sender);
        _grantRole(GOVERNANCE_ROLE, msg.sender);
    }

    /**
     * @notice Register a new AI agent
     * @param agentId Unique agent identifier
     * @param stake Initial stake amount
     */
    function registerAgent(bytes32 agentId, uint256 stake) external nonReentrant {
        require(agents[agentId].owner == address(0), "Agent already registered");
        require(stake >= minStake, "Insufficient stake");
        
        // Transfer stake
        stakingToken.safeTransferFrom(msg.sender, address(this), stake);
        
        // Create agent record
        agents[agentId] = Agent({
            owner: msg.sender,
            stake: stake,
            reputation: INITIAL_REPUTATION,
            positiveActions: 0,
            negativeActions: 0,
            totalSlashed: 0,
            lastActivity: block.timestamp,
            registeredAt: block.timestamp,
            violationCount: 0,
            active: true
        });
        
        agentCount++;
        
        emit AgentRegistered(agentId, msg.sender, stake);
    }

    /**
     * @notice Add stake to an agent
     * @param agentId The agent identifier
     * @param amount Amount to add
     */
    function addStake(bytes32 agentId, uint256 amount) external nonReentrant {
        Agent storage agent = agents[agentId];
        require(agent.owner == msg.sender, "Not owner");
        require(amount > 0, "Amount must be positive");
        
        stakingToken.safeTransferFrom(msg.sender, address(this), amount);
        agent.stake += amount;
        
        emit StakeAdded(agentId, amount);
    }

    /**
     * @notice Withdraw stake (only if agent is deactivated or enough buffer)
     * @param agentId The agent identifier
     * @param amount Amount to withdraw
     */
    function withdrawStake(bytes32 agentId, uint256 amount) external nonReentrant {
        Agent storage agent = agents[agentId];
        require(agent.owner == msg.sender, "Not owner");
        
        if (agent.active) {
            require(agent.stake - amount >= minStake, "Would go below minimum");
        }
        
        require(agent.stake >= amount, "Insufficient stake");
        
        agent.stake -= amount;
        stakingToken.safeTransfer(msg.sender, amount);
        
        emit StakeWithdrawn(agentId, amount);
    }

    /**
     * @notice Record positive action
     * @param agentId The agent identifier
     * @param points Reputation points to add
     */
    function recordPositive(bytes32 agentId, uint256 points) external onlyRole(SLASHER_ROLE) {
        Agent storage agent = agents[agentId];
        require(agent.active, "Agent not active");
        
        uint256 oldRep = agent.reputation;
        agent.reputation = _min(agent.reputation + points, MAX_REPUTATION);
        agent.positiveActions++;
        agent.lastActivity = block.timestamp;
        
        emit ReputationUpdated(agentId, oldRep, agent.reputation);
    }

    /**
     * @notice Report a violation and slash the agent
     * @param agentId The agent identifier
     * @param violationType Type of violation
     * @param evidence Evidence string
     */
    function reportViolation(
        bytes32 agentId,
        ViolationType violationType,
        string calldata evidence
    ) external onlyRole(SLASHER_ROLE) {
        Agent storage agent = agents[agentId];
        require(agent.active, "Agent not active");
        
        // Calculate slash amount
        uint256 slashPercentage = _getSlashPercentage(violationType);
        uint256 slashAmount = (agent.stake * slashPercentage) / 100;
        
        // Calculate reputation penalty
        uint256 repPenalty = _getReputationPenalty(violationType);
        uint256 oldRep = agent.reputation;
        agent.reputation = agent.reputation > repPenalty ? agent.reputation - repPenalty : 0;
        
        // Apply slash
        agent.stake -= slashAmount;
        agent.totalSlashed += slashAmount;
        agent.negativeActions++;
        agent.violationCount++;
        agent.lastActivity = block.timestamp;
        totalSlashed += slashAmount;
        
        // Record violation
        violations[agentId].push(Violation({
            agentId: agentId,
            violationType: violationType,
            slashAmount: slashAmount,
            newReputation: agent.reputation,
            evidence: evidence,
            timestamp: block.timestamp,
            reporter: msg.sender
        }));
        
        emit Slashed(agentId, violationType, slashAmount, agent.reputation);
        emit ReputationUpdated(agentId, oldRep, agent.reputation);
        
        // Check for deactivation
        if (agent.violationCount >= maxViolations || agent.reputation < MIN_REPUTATION) {
            agent.active = false;
            emit AgentDeactivated(agentId, "Too many violations or low reputation");
        }
    }

    /**
     * @notice Process epoch - recover reputation for good agents
     * @param agentIds Array of agent IDs to process
     */
    function processEpoch(bytes32[] calldata agentIds) external {
        for (uint256 i = 0; i < agentIds.length; i++) {
            Agent storage agent = agents[agentIds[i]];
            
            if (agent.active && agent.reputation < MAX_REPUTATION) {
                // Only recover if no recent violations (24 hours)
                uint256 lastViolationTime = 0;
                if (violations[agentIds[i]].length > 0) {
                    lastViolationTime = violations[agentIds[i]][violations[agentIds[i]].length - 1].timestamp;
                }
                
                if (block.timestamp - lastViolationTime > 1 days) {
                    uint256 oldRep = agent.reputation;
                    agent.reputation = _min(agent.reputation + recoveryRate, MAX_REPUTATION);
                    
                    if (oldRep != agent.reputation) {
                        emit ReputationUpdated(agentIds[i], oldRep, agent.reputation);
                    }
                }
            }
        }
    }

    /**
     * @notice Check if agent can participate
     * @param agentId The agent identifier
     */
    function canParticipate(bytes32 agentId) external view returns (bool) {
        Agent storage agent = agents[agentId];
        return agent.active && agent.reputation >= MIN_REPUTATION && agent.stake >= minStake;
    }

    /**
     * @notice Get agent reputation
     * @param agentId The agent identifier
     */
    function getReputation(bytes32 agentId) external view returns (uint256) {
        return agents[agentId].reputation;
    }

    /**
     * @notice Get violation count
     * @param agentId The agent identifier
     */
    function getViolationCount(bytes32 agentId) external view returns (uint256) {
        return violations[agentId].length;
    }

    /**
     * @notice Get slash percentage for violation type
     */
    function _getSlashPercentage(ViolationType vt) internal pure returns (uint256) {
        if (vt == ViolationType.InvalidTestimony) return 5;
        if (vt == ViolationType.DoubleVoting) return 50;
        if (vt == ViolationType.Downtime) return 1;
        if (vt == ViolationType.MaliciousBehavior) return 100;
        if (vt == ViolationType.Spam) return 10;
        if (vt == ViolationType.TaskFailure) return 2;
        if (vt == ViolationType.Collusion) return 100;
        if (vt == ViolationType.DataCorruption) return 25;
        return 0;
    }

    /**
     * @notice Get reputation penalty for violation type
     */
    function _getReputationPenalty(ViolationType vt) internal pure returns (uint256) {
        if (vt == ViolationType.InvalidTestimony) return 50;
        if (vt == ViolationType.DoubleVoting) return 200;
        if (vt == ViolationType.Downtime) return 10;
        if (vt == ViolationType.MaliciousBehavior) return 500;
        if (vt == ViolationType.Spam) return 100;
        if (vt == ViolationType.TaskFailure) return 20;
        if (vt == ViolationType.Collusion) return 500;
        if (vt == ViolationType.DataCorruption) return 150;
        return 0;
    }

    /**
     * @notice Min helper
     */
    function _min(uint256 a, uint256 b) internal pure returns (uint256) {
        return a < b ? a : b;
    }

    /**
     * @notice Update governance parameters
     */
    function updateParams(
        uint256 _minStake,
        uint256 _maxViolations,
        uint256 _recoveryRate,
        uint256 _epochDuration
    ) external onlyRole(GOVERNANCE_ROLE) {
        minStake = _minStake;
        maxViolations = _maxViolations;
        recoveryRate = _recoveryRate;
        epochDuration = _epochDuration;
    }
}
