//! Published contract sources for dcscan's Etherscan-style Contract tab.
//!
//! These are the canonical repository copies of contracts that live on
//! chain 271828. dcscan surfaces the source + ABI next to the live
//! `eth_getCode` bytecode. This is **not** a compiler-matched Sourcify
//! verification: the explorer does not recompile and byte-compare. The
//! UI must label the status as "Source published" rather than a fake
//! green "Contract Source Code Verified" badge.

/// Published source descriptor for a known Datachain Rope contract.
pub struct PublishedSource {
    pub contract_name: &'static str,
    pub compiler: &'static str,
    pub license: &'static str,
    pub optimization: &'static str,
    pub evm_version: &'static str,
    pub source_path: &'static str,
    pub source: &'static str,
    pub abi: serde_json::Value,
}

/// Look up a published source by on-chain address (any case).
pub fn published_source(addr: &str) -> Option<PublishedSource> {
    match addr.to_lowercase().as_str() {
        "0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4"
        | "0xddbf887982a2a1c03cb8705fef9e09c46122fff6"
        | "0x90e2e170b0fc133343f0d7fde128c1fb716aab25" => Some(wfat()),
        _ => None,
    }
}

fn wfat() -> PublishedSource {
    PublishedSource {
        contract_name: "WFAT",
        compiler: "solc 0.8.20",
        license: "MIT",
        optimization: "as deployed (DCSwap contracts/src/WFAT.sol)",
        evm_version: "default (solc 0.8.20)",
        source_path: "dcswap/contracts/src/WFAT.sol",
        source: include_str!("../verified-contracts/WFAT.sol"),
        abi: wfat_abi(),
    }
}

fn wfat_abi() -> serde_json::Value {
    serde_json::json!([
        { "type": "receive", "stateMutability": "payable" },
        { "type": "function", "name": "name", "stateMutability": "view",
          "inputs": [], "outputs": [{ "name": "", "type": "string" }] },
        { "type": "function", "name": "symbol", "stateMutability": "view",
          "inputs": [], "outputs": [{ "name": "", "type": "string" }] },
        { "type": "function", "name": "decimals", "stateMutability": "view",
          "inputs": [], "outputs": [{ "name": "", "type": "uint8" }] },
        { "type": "function", "name": "totalSupply", "stateMutability": "view",
          "inputs": [], "outputs": [{ "name": "", "type": "uint256" }] },
        { "type": "function", "name": "balanceOf", "stateMutability": "view",
          "inputs": [{ "name": "", "type": "address" }],
          "outputs": [{ "name": "", "type": "uint256" }] },
        { "type": "function", "name": "allowance", "stateMutability": "view",
          "inputs": [
            { "name": "", "type": "address" },
            { "name": "", "type": "address" }
          ],
          "outputs": [{ "name": "", "type": "uint256" }] },
        { "type": "function", "name": "deposit", "stateMutability": "payable",
          "inputs": [], "outputs": [] },
        { "type": "function", "name": "withdraw", "stateMutability": "nonpayable",
          "inputs": [{ "name": "wad", "type": "uint256" }], "outputs": [] },
        { "type": "function", "name": "approve", "stateMutability": "nonpayable",
          "inputs": [
            { "name": "guy", "type": "address" },
            { "name": "wad", "type": "uint256" }
          ],
          "outputs": [{ "name": "", "type": "bool" }] },
        { "type": "function", "name": "transfer", "stateMutability": "nonpayable",
          "inputs": [
            { "name": "dst", "type": "address" },
            { "name": "wad", "type": "uint256" }
          ],
          "outputs": [{ "name": "", "type": "bool" }] },
        { "type": "function", "name": "transferFrom", "stateMutability": "nonpayable",
          "inputs": [
            { "name": "src", "type": "address" },
            { "name": "dst", "type": "address" },
            { "name": "wad", "type": "uint256" }
          ],
          "outputs": [{ "name": "", "type": "bool" }] },
        { "type": "event", "name": "Deposit", "anonymous": false,
          "inputs": [
            { "indexed": true, "name": "dst", "type": "address" },
            { "indexed": false, "name": "wad", "type": "uint256" }
          ] },
        { "type": "event", "name": "Withdrawal", "anonymous": false,
          "inputs": [
            { "indexed": true, "name": "src", "type": "address" },
            { "indexed": false, "name": "wad", "type": "uint256" }
          ] },
        { "type": "event", "name": "Approval", "anonymous": false,
          "inputs": [
            { "indexed": true, "name": "src", "type": "address" },
            { "indexed": true, "name": "guy", "type": "address" },
            { "indexed": false, "name": "wad", "type": "uint256" }
          ] },
        { "type": "event", "name": "Transfer", "anonymous": false,
          "inputs": [
            { "indexed": true, "name": "src", "type": "address" },
            { "indexed": true, "name": "dst", "type": "address" },
            { "indexed": false, "name": "wad", "type": "uint256" }
          ] }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wfat_source_is_the_live_wrap_contract() {
        let src = published_source("0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4")
            .expect("WFAT must have published source");
        assert_eq!(src.contract_name, "WFAT");
        assert!(src.source.contains("contract WFAT"));
        assert!(src.source.contains("function deposit()"));
        assert!(src.source.contains("function withdraw(uint256 wad)"));
        assert!(src.abi.as_array().map(|a| a.len()).unwrap_or(0) >= 12);
    }

    #[test]
    fn unknown_address_has_no_fabricated_source() {
        assert!(published_source("0x000000000000000000000000000000000000d004").is_none());
    }
}
