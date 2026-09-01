// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

/**
 * @title RopeComplianceModule
 * @notice ModularCompliance module for ERC-3643 tokens on Datachain Rope.
 *         Enforces MiFID II rules: restricted countries, max holders per
 *         jurisdiction, minimum investment, accredited-investor gate, and
 *         lockup periods.  Works alongside the off-chain ComplianceAgent AI
 *         Testimony system.
 *
 * @dev    Plugged into the ModularCompliance contract via bindCompliance().
 *         The token calls canTransfer() before every transfer; this module
 *         checks on-chain rules then emits a TestimonyRequested event that
 *         the ComplianceAgent node picks up for additional AI validation.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import "@openzeppelin/contracts/access/AccessControl.sol";

interface IModularCompliance {
    function isModuleBound(address _module) external view returns (bool);
}

interface IIdentityRegistry {
    function isVerified(address _userAddress) external view returns (bool);
    function investorCountry(address _userAddress) external view returns (uint16);
    function identity(address _userAddress) external view returns (address);
}

/// @dev Minimal ERC-735 claim-holder interface (ONCHAINID Identity). Used to
///      enumerate and read the accredited-investor (topic 4) claim(s) on a
///      holder's identity for the {requireAccreditedInvestor} gate below.
interface IClaimHolder {
    function getClaimIdsByTopic(uint256 _topic) external view returns (bytes32[] memory);
    function getClaim(bytes32 _claimId)
        external
        view
        returns (
            uint256 topic,
            uint256 scheme,
            address issuer,
            bytes memory sig,
            bytes memory data,
            string memory uri
        );
}

/// @dev Matches `IDatawalletClaimIssuer.isClaimValid` (and any ERC-735
///      IClaimIssuer-compatible issuer) at the ABI level — Solidity encodes
///      contract/interface-typed parameters as `address` for selector
///      purposes, so this resolves to the same call regardless of whether
///      the issuer's own source declares its first parameter as `address`
///      or as an identity interface type.
interface IAccreditationClaimIssuer {
    function isClaimValid(
        address _identity,
        uint256 _claimTopic,
        bytes memory _sig,
        bytes memory _data
    ) external view returns (bool valid);
}

contract RopeComplianceModule is AccessControl {
    // =========================================================================
    // Roles
    // =========================================================================

    bytes32 public constant COMPLIANCE_ADMIN_ROLE = keccak256("COMPLIANCE_ADMIN_ROLE");
    bytes32 public constant TESTIMONY_AGENT_ROLE = keccak256("TESTIMONY_AGENT_ROLE");

    // =========================================================================
    // State — Rules
    // =========================================================================

    /// @notice ISO 3166-1 numeric codes of restricted countries.
    mapping(uint16 => bool) public restrictedCountries;

    /// @notice Max token holders per jurisdiction.
    mapping(uint16 => uint256) public maxHoldersPerCountry;

    /// @notice Current holder count per jurisdiction.
    mapping(uint16 => uint256) public holderCountByCountry;

    /// @notice Minimum transfer / investment amount (in token decimals).
    uint256 public minTransferAmount;

    /// @notice When true, both sender and receiver must hold
    ///         ACCREDITED_INVESTOR claim (topic 4).
    bool public requireAccreditedInvestor;

    /// @notice Lockup period in seconds from first mint.
    uint256 public lockupPeriodSeconds;

    /// @notice Timestamp of first mint per holder.
    mapping(address => uint256) public firstMintTimestamp;

    /// @notice Identity registry used for claim lookups.
    IIdentityRegistry public identityRegistry;

    /// @notice Bound compliance contract.
    address public complianceContract;

    /// @notice ERC-735 claim topic for "accredited investor" — matches
    ///         `DatawalletClaimIssuer.ACCREDITED_INVESTOR`.
    uint256 public constant ACCREDITED_INVESTOR_TOPIC = 4;

    /// @notice Trusted issuer consulted by {requireAccreditedInvestor}.
    ///         SECURITY (2026-07-26 counter-audit fix): {canTransfer}
    ///         previously read this flag from storage but never checked
    ///         it against anything — flipping it via
    ///         {setRequireAccreditedInvestor}/{updateRules} had zero
    ///         on-chain effect. Until an admin calls
    ///         {setAccreditationClaimIssuer}, this stays `address(0)` and
    ///         the gate below fails CLOSED (denies every transfer) rather
    ///         than silently passing everyone, which is what actually
    ///         happened before this fix.
    IAccreditationClaimIssuer public accreditationClaimIssuer;

    // =========================================================================
    // State — Testimony
    // =========================================================================

    /// @notice Incremented for each transfer validated.
    uint256 public testimonyNonce;

    struct Testimony {
        address from;
        address to;
        uint256 amount;
        bool allowed;
        string reason;
        bytes32 testimonyHash;
        uint256 timestamp;
    }

    mapping(uint256 => Testimony) public testimonies;

    // =========================================================================
    // Events
    // =========================================================================

    event TestimonyRequested(
        uint256 indexed nonce,
        address indexed from,
        address indexed to,
        uint256 amount
    );

    event TestimonyRecorded(
        uint256 indexed nonce,
        bool allowed,
        bytes32 testimonyHash
    );

    event CountryRestrictionUpdated(uint16 indexed country, bool restricted);
    event MaxHoldersUpdated(uint16 indexed country, uint256 max);
    event RulesUpdated(uint256 minAmount, bool accredited, uint256 lockup);
    event AccreditationClaimIssuerUpdated(address indexed issuer);

    // =========================================================================
    // Modifiers
    // =========================================================================

    /// @notice SECURITY (2026-07-26 counter-audit fix): {canTransfer} and
    ///         {transferred} previously had NO access control at all —
    ///         any address could call {transferred} directly to arbitrarily
    ///         inflate/deflate {holderCountByCountry} for any jurisdiction
    ///         (griefing the max-holders check for everyone else), or call
    ///         {canTransfer} directly to reset a target's
    ///         {firstMintTimestamp} early / spam fabricated `allowed=true`
    ///         entries into the {testimonies} audit log. Per this module's
    ///         own docstring ("The token calls canTransfer() before every
    ///         transfer" via the bound ModularCompliance), the only
    ///         legitimate caller of either function is the bound
    ///         `complianceContract` set via {bindCompliance}.
    modifier onlyComplianceContract() {
        require(
            complianceContract != address(0) && msg.sender == complianceContract,
            "caller is not the bound compliance contract"
        );
        _;
    }

    // =========================================================================
    // Constructor
    // =========================================================================

    constructor(address _identityRegistry, address _admin) {
        require(_identityRegistry != address(0), "registry = 0");
        require(_admin != address(0), "admin = 0");

        identityRegistry = IIdentityRegistry(_identityRegistry);

        _grantRole(DEFAULT_ADMIN_ROLE, _admin);
        _grantRole(COMPLIANCE_ADMIN_ROLE, _admin);
        _grantRole(TESTIMONY_AGENT_ROLE, _admin);
    }

    // =========================================================================
    // Compliance Check — called by ModularCompliance
    // =========================================================================

    /**
     * @notice Evaluates whether a transfer is compliant.
     * @return allowed True when all on-chain rules pass.
     */
    function canTransfer(
        address _from,
        address _to,
        uint256 _amount
    ) external onlyComplianceContract returns (bool allowed) {
        if (_from == address(0)) {
            if (firstMintTimestamp[_to] == 0) {
                firstMintTimestamp[_to] = block.timestamp;
            }
        }

        if (_amount < minTransferAmount && _from != address(0)) {
            _recordTestimony(_from, _to, _amount, false, "below minimum");
            return false;
        }

        if (!identityRegistry.isVerified(_to)) {
            _recordTestimony(_from, _to, _amount, false, "receiver not verified");
            return false;
        }

        uint16 toCountry = identityRegistry.investorCountry(_to);

        if (restrictedCountries[toCountry]) {
            _recordTestimony(_from, _to, _amount, false, "restricted country");
            return false;
        }

        if (
            maxHoldersPerCountry[toCountry] > 0 &&
            holderCountByCountry[toCountry] >= maxHoldersPerCountry[toCountry]
        ) {
            _recordTestimony(_from, _to, _amount, false, "max holders reached");
            return false;
        }

        if (_from != address(0) && lockupPeriodSeconds > 0) {
            uint256 mintTime = firstMintTimestamp[_from];
            if (mintTime > 0 && block.timestamp < mintTime + lockupPeriodSeconds) {
                _recordTestimony(_from, _to, _amount, false, "lockup active");
                return false;
            }
        }

        // SECURITY (2026-07-26 counter-audit fix, finding #2): this gate
        // previously read `requireAccreditedInvestor` from storage and
        // then never checked it against anything, so toggling the flag
        // had zero effect and every transfer passed regardless of
        // accreditation status.
        if (requireAccreditedInvestor) {
            if (_from != address(0) && !_hasAccreditedClaim(_from)) {
                _recordTestimony(_from, _to, _amount, false, "sender not accredited");
                return false;
            }
            if (!_hasAccreditedClaim(_to)) {
                _recordTestimony(_from, _to, _amount, false, "receiver not accredited");
                return false;
            }
        }

        _recordTestimony(_from, _to, _amount, true, "");
        return true;
    }

    /**
     * @notice Called by ModularCompliance after a successful transfer to
     *         update jurisdiction holder counts.
     */
    function transferred(
        address _from,
        address _to,
        uint256 /* _amount */
    ) external onlyComplianceContract {
        if (_from != address(0)) {
            uint16 fromCountry = identityRegistry.investorCountry(_from);
            if (holderCountByCountry[fromCountry] > 0) {
                holderCountByCountry[fromCountry]--;
            }
        }
        if (_to != address(0)) {
            uint16 toCountry = identityRegistry.investorCountry(_to);
            holderCountByCountry[toCountry]++;
        }
    }

    /// @dev Returns true only when `_user`'s ONCHAINID carries a claim for
    ///      {ACCREDITED_INVESTOR_TOPIC} issued by (and still considered
    ///      valid by) {accreditationClaimIssuer}. Fails CLOSED — returns
    ///      false whenever no issuer is configured, the user has no
    ///      identity on file, or no matching claim is found — so an admin
    ///      flipping {requireAccreditedInvestor} on before configuring
    ///      {setAccreditationClaimIssuer} blocks transfers instead of
    ///      silently allowing them.
    function _hasAccreditedClaim(address _user) internal view returns (bool) {
        if (address(accreditationClaimIssuer) == address(0)) return false;

        address identityAddr = identityRegistry.identity(_user);
        if (identityAddr == address(0)) return false;

        IClaimHolder holder = IClaimHolder(identityAddr);
        bytes32[] memory claimIds = holder.getClaimIdsByTopic(ACCREDITED_INVESTOR_TOPIC);

        for (uint256 i = 0; i < claimIds.length; i++) {
            (, , address issuer, bytes memory sig, bytes memory data, ) = holder.getClaim(claimIds[i]);
            if (issuer != address(accreditationClaimIssuer)) continue;
            if (accreditationClaimIssuer.isClaimValid(identityAddr, ACCREDITED_INVESTOR_TOPIC, sig, data)) {
                return true;
            }
        }
        return false;
    }

    // =========================================================================
    // Testimony Agent — records AI Testimony on-chain
    // =========================================================================

    function recordExternalTestimony(
        uint256 _nonce,
        bool _allowed,
        bytes32 _testimonyHash
    ) external onlyRole(TESTIMONY_AGENT_ROLE) {
        require(testimonies[_nonce].timestamp > 0, "nonce not found");
        testimonies[_nonce].allowed = _allowed;
        testimonies[_nonce].testimonyHash = _testimonyHash;
        emit TestimonyRecorded(_nonce, _allowed, _testimonyHash);
    }

    function getLastTestimonyHash() external view returns (bytes32) {
        if (testimonyNonce == 0) return bytes32(0);
        return testimonies[testimonyNonce - 1].testimonyHash;
    }

    // =========================================================================
    // Admin — Rule Configuration
    // =========================================================================

    function setRestrictedCountry(uint16 _country, bool _restricted)
        external
        onlyRole(COMPLIANCE_ADMIN_ROLE)
    {
        restrictedCountries[_country] = _restricted;
        emit CountryRestrictionUpdated(_country, _restricted);
    }

    function setMaxHoldersPerCountry(uint16 _country, uint256 _max)
        external
        onlyRole(COMPLIANCE_ADMIN_ROLE)
    {
        maxHoldersPerCountry[_country] = _max;
        emit MaxHoldersUpdated(_country, _max);
    }

    function setMinTransferAmount(uint256 _amount)
        external
        onlyRole(COMPLIANCE_ADMIN_ROLE)
    {
        minTransferAmount = _amount;
    }

    function setRequireAccreditedInvestor(bool _required)
        external
        onlyRole(COMPLIANCE_ADMIN_ROLE)
    {
        requireAccreditedInvestor = _required;
    }

    /// @notice Configure the trusted claim issuer consulted by
    ///         {_hasAccreditedClaim} when {requireAccreditedInvestor} is
    ///         true. Pass `address(0)` to intentionally fail-closed (deny
    ///         all transfers while the flag is on) — e.g. while rotating
    ///         issuers.
    function setAccreditationClaimIssuer(address _issuer)
        external
        onlyRole(COMPLIANCE_ADMIN_ROLE)
    {
        accreditationClaimIssuer = IAccreditationClaimIssuer(_issuer);
        emit AccreditationClaimIssuerUpdated(_issuer);
    }

    function setLockupPeriod(uint256 _seconds)
        external
        onlyRole(COMPLIANCE_ADMIN_ROLE)
    {
        lockupPeriodSeconds = _seconds;
    }

    function updateRules(
        uint256 _minAmount,
        bool _accredited,
        uint256 _lockup
    ) external onlyRole(COMPLIANCE_ADMIN_ROLE) {
        minTransferAmount = _minAmount;
        requireAccreditedInvestor = _accredited;
        lockupPeriodSeconds = _lockup;
        emit RulesUpdated(_minAmount, _accredited, _lockup);
    }

    function setIdentityRegistry(address _registry)
        external
        onlyRole(DEFAULT_ADMIN_ROLE)
    {
        require(_registry != address(0), "registry = 0");
        identityRegistry = IIdentityRegistry(_registry);
    }

    function bindCompliance(address _compliance)
        external
        onlyRole(DEFAULT_ADMIN_ROLE)
    {
        complianceContract = _compliance;
    }

    // =========================================================================
    // ERC-165
    // =========================================================================

    function supportsInterface(bytes4 interfaceId)
        public
        view
        override(AccessControl)
        returns (bool)
    {
        return super.supportsInterface(interfaceId);
    }

    // =========================================================================
    // Internal
    // =========================================================================

    function _recordTestimony(
        address _from,
        address _to,
        uint256 _amount,
        bool _allowed,
        string memory _reason
    ) private {
        uint256 nonce = testimonyNonce++;
        bytes32 tHash = keccak256(
            abi.encode(nonce, _from, _to, _amount, _allowed, block.timestamp)
        );

        testimonies[nonce] = Testimony({
            from: _from,
            to: _to,
            amount: _amount,
            allowed: _allowed,
            reason: _reason,
            testimonyHash: tHash,
            timestamp: block.timestamp
        });

        emit TestimonyRequested(nonce, _from, _to, _amount);
        if (_allowed || bytes(_reason).length > 0) {
            emit TestimonyRecorded(nonce, _allowed, tHash);
        }
    }
}
