// SPDX-License-Identifier: MIT
pragma solidity 0.8.17;

// Hardhat only compiles artifacts reachable (by import) from `src/`. Nothing
// in production source imports the concrete `@onchain-id/solidity` `Identity`
// implementation (only its `IIdentity` interface is used), so its artifact
// is never produced and `ethers.getContractFactory("Identity")` fails in
// tests. This file's sole purpose is to force that artifact to exist so
// Hardhat/Foundry test suites can deploy a *real* ERC-734/735 Identity
// contract (rather than a hand-rolled mock) when exercising
// `DatawalletClaimIssuer.issueClaimToIdentity()`, whose `identity.addClaim()`
// call is `onlyClaimKey` per ERC-735 and therefore requires a real Identity,
// not a bare EOA. It deploys no state and is never referenced by any
// production contract.
import {Identity as OnchainIdIdentity} from "@onchain-id/solidity/contracts/Identity.sol";
