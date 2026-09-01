// SPDX-License-Identifier: MIT
pragma solidity 0.8.20;

/// @title WFAT - Wrapped DC FAT (WETH9-style wrapper for native FAT)
contract WFAT {
    string public name = "Wrapped DC FAT";
    string public symbol = "WFAT";
    uint8 public decimals = 18;

    event Deposit(address indexed dst, uint256 wad);
    event Withdrawal(address indexed src, uint256 wad);
    event Approval(address indexed src, address indexed guy, uint256 wad);
    event Transfer(address indexed src, address indexed dst, uint256 wad);

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    receive() external payable {
        deposit();
    }

    function deposit() public payable {
        balanceOf[msg.sender] += msg.value;
        emit Deposit(msg.sender, msg.value);
    }

    /// @notice Withdraw native FAT from the WFAT wrapper.
    /// @dev    Audit-2026-06-11 §F10: migrated from `payable(msg.sender).transfer(wad)`
    ///         to a low-level `.call{value:}("")` for forward-compatibility
    ///         with smart-contract wallets (Gnosis Safe, EIP-4337
    ///         account-abstraction wallets, Argent, etc.) whose `receive`
    ///         handler consumes more than the 2300-gas stipend that
    ///         `.transfer` forwards. Reentrancy is bounded by the
    ///         Checks-Effects-Interactions order on the next two lines
    ///         (`balanceOf[msg.sender] -= wad` happens before the external
    ///         call) - a re-entered `withdraw` would see a zeroed balance
    ///         and revert, and `deposit`/`transferFrom` re-entrancy can
    ///         only add value, not steal it.
    function withdraw(uint256 wad) public {
        require(balanceOf[msg.sender] >= wad, "WFAT: insufficient balance");
        balanceOf[msg.sender] -= wad;
        (bool ok, ) = payable(msg.sender).call{value: wad}("");
        require(ok, "WFAT: native send failed");
        emit Withdrawal(msg.sender, wad);
    }

    function totalSupply() public view returns (uint256) {
        return address(this).balance;
    }

    function approve(address guy, uint256 wad) public returns (bool) {
        allowance[msg.sender][guy] = wad;
        emit Approval(msg.sender, guy, wad);
        return true;
    }

    function transfer(address dst, uint256 wad) public returns (bool) {
        return transferFrom(msg.sender, dst, wad);
    }

    function transferFrom(address src, address dst, uint256 wad) public returns (bool) {
        require(balanceOf[src] >= wad, "WFAT: insufficient balance");

        if (src != msg.sender && allowance[src][msg.sender] != type(uint256).max) {
            require(allowance[src][msg.sender] >= wad, "WFAT: insufficient allowance");
            allowance[src][msg.sender] -= wad;
        }

        balanceOf[src] -= wad;
        balanceOf[dst] += wad;

        emit Transfer(src, dst, wad);
        return true;
    }
}
