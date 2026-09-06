// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "@openzeppelin/contracts-upgradeable/token/ERC721/ERC721Upgradeable.sol";
import "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import "@openzeppelin/contracts-upgradeable/utils/ReentrancyGuardUpgradeable.sol";

/**
 * @title LaunchLabIdentity
 * @author Datachain Foundation
 * @notice Non-transferable (soulbound) identity NFT for LaunchLab project owners
 * @dev Each wallet can have exactly one identity NFT. Metadata is stored on IPFS
 *      and referenced by CID. The NFT is non-transferable to maintain identity
 *      integrity across the platform.
 * 
 * Design Notes:
 * - One identity per wallet (enforced)
 * - Non-transferable (soulbound) - only minting allowed
 * - Metadata stored on IPFS as JSON document
 * - Supports metadata versioning via previousVersions array in JSON
 * - Owner can update metadata CID anytime
 * - Protocol fee for identity creation (configurable)
 */
contract LaunchLabIdentity is 
    ERC721Upgradeable, 
    OwnableUpgradeable, 
    UUPSUpgradeable,
    ReentrancyGuardUpgradeable 
{
    // ============ State Variables ============
    
    /// @notice Mapping from tokenId to IPFS CID
    mapping(uint256 => string) private _metadataCIDs;
    
    /// @notice Mapping from wallet address to their identity tokenId
    /// @dev Returns 0 if no identity exists (tokenId starts at 1)
    mapping(address => uint256) public walletToIdentity;
    
    /// @notice Counter for token IDs (starts at 1)
    uint256 private _tokenIdCounter;
    
    /// @notice Protocol fee for creating identity (in native token)
    uint256 public identityCreationFee;
    
    /// @notice Treasury address for receiving fees
    address public treasury;
    
    /// @notice Mapping to track identity creation timestamp
    mapping(uint256 => uint256) public identityCreatedAt;
    
    /// @notice Mapping to track last metadata update timestamp
    mapping(uint256 => uint256) public lastMetadataUpdate;
    
    /// @notice Base URI for IPFS gateway
    string public ipfsGateway;
    
    // ============ Events ============
    
    /// @notice Emitted when a new identity is created
    event IdentityCreated(
        address indexed owner, 
        uint256 indexed tokenId, 
        string cid,
        uint256 timestamp
    );
    
    /// @notice Emitted when identity metadata is updated
    event IdentityUpdated(
        uint256 indexed tokenId, 
        string oldCid, 
        string newCid,
        uint256 timestamp
    );
    
    /// @notice Emitted when creation fee is updated
    event CreationFeeUpdated(uint256 oldFee, uint256 newFee);
    
    /// @notice Emitted when treasury address is updated
    event TreasuryUpdated(address oldTreasury, address newTreasury);
    
    /// @notice Emitted when IPFS gateway is updated
    event IPFSGatewayUpdated(string oldGateway, string newGateway);
    
    /// @notice Emitted when fees are withdrawn
    event FeesWithdrawn(address indexed to, uint256 amount);
    
    // ============ Errors ============
    
    error IdentityAlreadyExists(address wallet, uint256 existingTokenId);
    error IdentityNotFound(address wallet);
    error NotIdentityOwner(address caller, uint256 tokenId);
    error EmptyCID();
    error InvalidCIDFormat(string cid);
    error TransferNotAllowed();
    error InsufficientFee(uint256 sent, uint256 required);
    error ZeroAddress();
    error WithdrawalFailed();
    
    // ============ Modifiers ============
    
    modifier onlyIdentityOwner(uint256 tokenId) {
        if (_ownerOf(tokenId) != msg.sender) {
            revert NotIdentityOwner(msg.sender, tokenId);
        }
        _;
    }
    
    modifier validCID(string calldata cid) {
        if (bytes(cid).length == 0) {
            revert EmptyCID();
        }
        // Basic CID validation (v0 starts with Qm, v1 starts with b)
        bytes memory cidBytes = bytes(cid);
        if (cidBytes.length < 46) {
            revert InvalidCIDFormat(cid);
        }
        // CIDv0 check (starts with "Qm")
        bool isV0 = cidBytes[0] == 0x51 && cidBytes[1] == 0x6D; // "Qm"
        // CIDv1 check (starts with "b")
        bool isV1 = cidBytes[0] == 0x62; // "b"
        if (!isV0 && !isV1) {
            revert InvalidCIDFormat(cid);
        }
        _;
    }
    
    // ============ Constructor & Initializer ============
    
    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        _disableInitializers();
    }
    
    /**
     * @notice Initialize the contract
     * @param _treasury Address to receive protocol fees
     * @param _identityCreationFee Fee required to create identity (in wei)
     * @param _ipfsGateway IPFS gateway URL (e.g., "https://ipfs.datachain.network/ipfs/")
     */
    function initialize(
        address _treasury,
        uint256 _identityCreationFee,
        string calldata _ipfsGateway
    ) public initializer {
        if (_treasury == address(0)) revert ZeroAddress();
        
        __ERC721_init("LaunchLab Identity", "LLI");
        __Ownable_init(msg.sender);
        __UUPSUpgradeable_init();
        __ReentrancyGuard_init();
        
        treasury = _treasury;
        identityCreationFee = _identityCreationFee;
        ipfsGateway = _ipfsGateway;
        _tokenIdCounter = 0;
    }
    
    // ============ External Functions ============
    
    /**
     * @notice Create a new identity NFT for the caller
     * @param initialCid IPFS CID of the initial metadata document
     * @return tokenId The ID of the newly minted identity NFT
     */
    function createIdentity(string calldata initialCid) 
        external 
        payable 
        nonReentrant
        validCID(initialCid)
        returns (uint256) 
    {
        // Check if identity already exists
        if (walletToIdentity[msg.sender] != 0) {
            revert IdentityAlreadyExists(msg.sender, walletToIdentity[msg.sender]);
        }
        
        // Check fee payment
        if (msg.value < identityCreationFee) {
            revert InsufficientFee(msg.value, identityCreationFee);
        }
        
        // Increment counter first to ensure tokenId starts at 1
        _tokenIdCounter++;
        uint256 newTokenId = _tokenIdCounter;
        
        // Mint the NFT
        _safeMint(msg.sender, newTokenId);
        
        // Store metadata CID
        _metadataCIDs[newTokenId] = initialCid;
        
        // Link wallet to identity
        walletToIdentity[msg.sender] = newTokenId;
        
        // Record timestamps
        identityCreatedAt[newTokenId] = block.timestamp;
        lastMetadataUpdate[newTokenId] = block.timestamp;
        
        // Transfer fee to treasury if any
        if (msg.value > 0 && treasury != address(0)) {
            (bool success, ) = treasury.call{value: msg.value}("");
            if (!success) revert WithdrawalFailed();
        }
        
        emit IdentityCreated(msg.sender, newTokenId, initialCid, block.timestamp);
        
        return newTokenId;
    }
    
    /**
     * @notice Update the metadata CID for the caller's identity
     * @param newCid New IPFS CID containing updated metadata
     */
    function updateMetadata(string calldata newCid) 
        external 
        validCID(newCid)
    {
        uint256 tokenId = walletToIdentity[msg.sender];
        if (tokenId == 0) {
            revert IdentityNotFound(msg.sender);
        }
        
        // Verify ownership (should always be true due to soulbound nature)
        if (_ownerOf(tokenId) != msg.sender) {
            revert NotIdentityOwner(msg.sender, tokenId);
        }
        
        string memory oldCid = _metadataCIDs[tokenId];
        _metadataCIDs[tokenId] = newCid;
        lastMetadataUpdate[tokenId] = block.timestamp;
        
        emit IdentityUpdated(tokenId, oldCid, newCid, block.timestamp);
    }
    
    // ============ View Functions ============
    
    /**
     * @notice Get the current metadata CID for an identity
     * @param tokenId The identity token ID
     * @return The IPFS CID of the metadata
     */
    function getMetadataCID(uint256 tokenId) external view returns (string memory) {
        if (_ownerOf(tokenId) == address(0)) {
            revert IdentityNotFound(address(0));
        }
        return _metadataCIDs[tokenId];
    }
    
    /**
     * @notice Get identity token ID for a wallet
     * @param wallet The wallet address
     * @return The token ID (0 if no identity exists)
     */
    function getIdentityByWallet(address wallet) external view returns (uint256) {
        return walletToIdentity[wallet];
    }
    
    /**
     * @notice Check if a wallet has an identity
     * @param wallet The wallet address
     * @return True if the wallet has an identity
     */
    function hasIdentity(address wallet) external view returns (bool) {
        return walletToIdentity[wallet] != 0;
    }
    
    /**
     * @notice Get total number of identities created
     * @return The total count of identities
     */
    function totalIdentities() external view returns (uint256) {
        return _tokenIdCounter;
    }
    
    /**
     * @notice Get identity details
     * @param tokenId The identity token ID
     * @return owner The owner address
     * @return cid The metadata CID
     * @return createdAt Creation timestamp
     * @return updatedAt Last update timestamp
     */
    function getIdentityDetails(uint256 tokenId) 
        external 
        view 
        returns (
            address owner,
            string memory cid,
            uint256 createdAt,
            uint256 updatedAt
        ) 
    {
        owner = _ownerOf(tokenId);
        if (owner == address(0)) {
            revert IdentityNotFound(address(0));
        }
        cid = _metadataCIDs[tokenId];
        createdAt = identityCreatedAt[tokenId];
        updatedAt = lastMetadataUpdate[tokenId];
    }
    
    /**
     * @notice Returns the token URI pointing to IPFS metadata
     * @param tokenId The identity token ID
     * @return The full IPFS URI
     */
    function tokenURI(uint256 tokenId) public view override returns (string memory) {
        if (_ownerOf(tokenId) == address(0)) {
            revert IdentityNotFound(address(0));
        }
        
        string memory cid = _metadataCIDs[tokenId];
        
        // If gateway is set, use it; otherwise use ipfs:// protocol
        if (bytes(ipfsGateway).length > 0) {
            return string(abi.encodePacked(ipfsGateway, cid));
        }
        return string(abi.encodePacked("ipfs://", cid));
    }
    
    // ============ Admin Functions ============
    
    /**
     * @notice Update the identity creation fee
     * @param newFee New fee in wei
     */
    function setCreationFee(uint256 newFee) external onlyOwner {
        uint256 oldFee = identityCreationFee;
        identityCreationFee = newFee;
        emit CreationFeeUpdated(oldFee, newFee);
    }
    
    /**
     * @notice Update the treasury address
     * @param newTreasury New treasury address
     */
    function setTreasury(address newTreasury) external onlyOwner {
        if (newTreasury == address(0)) revert ZeroAddress();
        address oldTreasury = treasury;
        treasury = newTreasury;
        emit TreasuryUpdated(oldTreasury, newTreasury);
    }
    
    /**
     * @notice Update the IPFS gateway URL
     * @param newGateway New gateway URL
     */
    function setIPFSGateway(string calldata newGateway) external onlyOwner {
        string memory oldGateway = ipfsGateway;
        ipfsGateway = newGateway;
        emit IPFSGatewayUpdated(oldGateway, newGateway);
    }
    
    /**
     * @notice Withdraw accumulated fees (emergency only, normally fees go directly to treasury)
     * @param to Recipient address
     */
    function withdrawFees(address to) external onlyOwner {
        if (to == address(0)) revert ZeroAddress();
        uint256 balance = address(this).balance;
        if (balance > 0) {
            (bool success, ) = to.call{value: balance}("");
            if (!success) revert WithdrawalFailed();
            emit FeesWithdrawn(to, balance);
        }
    }
    
    // ============ Internal Functions ============
    
    /**
     * @notice Override to make tokens non-transferable (soulbound)
     * @dev Only allows minting (from == address(0)), prevents all transfers
     */
    function _update(
        address to, 
        uint256 tokenId, 
        address auth
    ) internal override returns (address) {
        address from = _ownerOf(tokenId);
        
        // Allow minting (from == address(0)) but prevent all transfers
        if (from != address(0)) {
            revert TransferNotAllowed();
        }
        
        return super._update(to, tokenId, auth);
    }
    
    /**
     * @notice Authorization for UUPS upgrades
     * @param newImplementation Address of new implementation
     */
    function _authorizeUpgrade(address newImplementation) internal override onlyOwner {}
    
    // ============ ERC165 Support ============
    
    /**
     * @notice Check interface support
     * @param interfaceId Interface identifier
     * @return True if interface is supported
     */
    function supportsInterface(bytes4 interfaceId) 
        public 
        view 
        override(ERC721Upgradeable) 
        returns (bool) 
    {
        return super.supportsInterface(interfaceId);
    }
}
