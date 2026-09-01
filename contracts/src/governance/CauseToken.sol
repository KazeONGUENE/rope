// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import {IDCR20} from "../interfaces/IDCR20.sol";
import {Ownable2Step} from "@openzeppelin/contracts/access/Ownable2Step.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

/**
 * @title CauseToken
 * @notice Minimal production DCR-20 fungible token minted for a winning NGO
 *         cause (spec §1.6). Deployed by `CauseTokenFactory` with the NGO
 *         treasury as owner and sole minter.
 */
contract CauseToken is IDCR20, Ownable2Step {
    string private _name;
    string private _symbol;
    uint8 public constant decimals = 18;

    uint256 public immutable maxSupply;
    uint256 private _totalSupply;

    mapping(address => uint256) private _balances;
    mapping(address => mapping(address => uint256)) private _allowances;

    address public minter;

    error ZeroAddress(string field);
    error NotMinter(address caller);
    error MaxSupplyExceeded(uint256 requested, uint256 maxSupply);

    event MinterUpdated(address indexed oldMinter, address indexed newMinter);

    modifier onlyMinter() {
        if (msg.sender != minter) revert NotMinter(msg.sender);
        _;
    }

    constructor(string memory name_, string memory symbol_, uint256 maxSupply_, address owner_)
        Ownable(owner_)
    {
        if (owner_ == address(0)) revert ZeroAddress("owner");
        _name = name_;
        _symbol = symbol_;
        maxSupply = maxSupply_;
        minter = owner_;
    }

    function name() external view returns (string memory) {
        return _name;
    }

    function symbol() external view returns (string memory) {
        return _symbol;
    }

    function totalSupply() external view returns (uint256) {
        return _totalSupply;
    }

    function balanceOf(address account) external view returns (uint256) {
        return _balances[account];
    }

    function allowance(address owner_, address spender) external view returns (uint256) {
        return _allowances[owner_][spender];
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        _approve(msg.sender, spender, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 current = _allowances[from][msg.sender];
        if (current != type(uint256).max) {
            if (current < amount) revert();
            unchecked {
                _approve(from, msg.sender, current - amount);
            }
        }
        _transfer(from, to, amount);
        return true;
    }

    function mint(address to, uint256 amount) external onlyMinter {
        if (to == address(0)) revert ZeroAddress("to");
        uint256 newTotal = _totalSupply + amount;
        if (newTotal > maxSupply) revert MaxSupplyExceeded(newTotal, maxSupply);
        _totalSupply = newTotal;
        unchecked {
            _balances[to] += amount;
        }
        emit Transfer(address(0), to, amount);
    }

    function setMinter(address newMinter) external onlyOwner {
        if (newMinter == address(0)) revert ZeroAddress("minter");
        emit MinterUpdated(minter, newMinter);
        minter = newMinter;
    }

    function _transfer(address from, address to, uint256 amount) internal {
        if (from == address(0)) revert ZeroAddress("from");
        if (to == address(0)) revert ZeroAddress("to");
        uint256 fromBal = _balances[from];
        if (fromBal < amount) revert();
        unchecked {
            _balances[from] = fromBal - amount;
            _balances[to] += amount;
        }
        emit Transfer(from, to, amount);
    }

    function _approve(address owner_, address spender, uint256 amount) internal {
        if (owner_ == address(0)) revert ZeroAddress("owner");
        if (spender == address(0)) revert ZeroAddress("spender");
        _allowances[owner_][spender] = amount;
        emit Approval(owner_, spender, amount);
    }
}
