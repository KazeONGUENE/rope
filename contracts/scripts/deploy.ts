import { ethers } from "hardhat";

async function main() {
  console.log("╔══════════════════════════════════════════════════════════════╗");
  console.log("║        DATACHAIN ROPE CONTRACT DEPLOYMENT                     ║");
  console.log("╚══════════════════════════════════════════════════════════════╝");

  const [deployer] = await ethers.getSigners();
  console.log("\nDeploying with account:", deployer.address);
  console.log("Account balance:", ethers.formatEther(await ethers.provider.getBalance(deployer.address)), "FAT");

  // Configuration
  const config = {
    // Governance
    votingDelay: 1, // 1 block
    votingPeriod: 45818, // ~1 week (assuming 13s blocks)
    proposalThreshold: ethers.parseEther("100000"), // 100k FAT
    quorumPercentage: 4, // 4%
    timelockDelay: 172800, // 2 days in seconds
    
    // Treasury
    spendingLimit: ethers.parseEther("100000"), // 100k FAT per tx
    dailyLimit: ethers.parseEther("500000"), // 500k FAT per day
    
    // Agent Reputation
    minStake: ethers.parseEther("1000"), // 1000 FAT
    maxViolations: 10,
    recoveryRate: 5, // 5 points per epoch
    epochDuration: 86400, // 1 day
  };

  console.log("\n📋 Configuration:");
  console.log("   Voting Delay:", config.votingDelay, "blocks");
  console.log("   Voting Period:", config.votingPeriod, "blocks (~1 week)");
  console.log("   Proposal Threshold:", ethers.formatEther(config.proposalThreshold), "FAT");
  console.log("   Quorum:", config.quorumPercentage, "%");
  console.log("   Timelock Delay:", config.timelockDelay / 86400, "days");

  // Step 1: Deploy FAT Token (if not already deployed)
  console.log("\n🚀 Step 1: Checking FAT Token...");
  const FAT_ADDRESS = process.env.FAT_TOKEN_ADDRESS || "";
  
  let fatToken;
  if (FAT_ADDRESS) {
    fatToken = await ethers.getContractAt("IERC20", FAT_ADDRESS);
    console.log("   Using existing FAT token at:", FAT_ADDRESS);
  } else {
    // Deploy mock token for testing
    const MockToken = await ethers.getContractFactory("MockERC20Votes");
    fatToken = await MockToken.deploy("DC FAT Token", "FAT");
    await fatToken.waitForDeployment();
    console.log("   Deployed mock FAT token at:", await fatToken.getAddress());
  }

  // Step 2: Deploy Timelock Controller
  console.log("\n🚀 Step 2: Deploying Timelock Controller...");
  const TimelockController = await ethers.getContractFactory("TimelockController");
  const timelock = await TimelockController.deploy(
    config.timelockDelay,
    [], // proposers - will be set later
    [], // executors - will be set later
    deployer.address // admin
  );
  await timelock.waitForDeployment();
  console.log("   Timelock deployed at:", await timelock.getAddress());

  // Step 3: Deploy DAO Governor
  console.log("\n🚀 Step 3: Deploying DatachainDAO...");
  const DatachainDAO = await ethers.getContractFactory("DatachainDAO");
  const dao = await DatachainDAO.deploy(
    await fatToken.getAddress(),
    await timelock.getAddress(),
    config.votingDelay,
    config.votingPeriod,
    config.proposalThreshold,
    config.quorumPercentage
  );
  await dao.waitForDeployment();
  console.log("   DAO deployed at:", await dao.getAddress());

  // Step 4: Deploy Treasury
  console.log("\n🚀 Step 4: Deploying Treasury...");
  const Treasury = await ethers.getContractFactory("DatachainTreasury");
  const treasury = await Treasury.deploy(
    await timelock.getAddress(),
    config.spendingLimit,
    config.dailyLimit
  );
  await treasury.waitForDeployment();
  console.log("   Treasury deployed at:", await treasury.getAddress());

  // Step 5: Deploy Agent Reputation
  console.log("\n🚀 Step 5: Deploying Agent Reputation...");
  const AgentReputation = await ethers.getContractFactory("AgentReputation");
  const agentRep = await AgentReputation.deploy(
    await fatToken.getAddress(),
    config.minStake,
    config.maxViolations,
    config.recoveryRate,
    config.epochDuration
  );
  await agentRep.waitForDeployment();
  console.log("   Agent Reputation deployed at:", await agentRep.getAddress());

  // Step 6: Configure Timelock roles
  console.log("\n🔧 Step 6: Configuring Timelock roles...");
  const PROPOSER_ROLE = await timelock.PROPOSER_ROLE();
  const EXECUTOR_ROLE = await timelock.EXECUTOR_ROLE();
  const CANCELLER_ROLE = await timelock.CANCELLER_ROLE();
  const ADMIN_ROLE = await timelock.DEFAULT_ADMIN_ROLE();

  // Grant roles to DAO
  await timelock.grantRole(PROPOSER_ROLE, await dao.getAddress());
  await timelock.grantRole(EXECUTOR_ROLE, await dao.getAddress());
  await timelock.grantRole(CANCELLER_ROLE, await dao.getAddress());
  console.log("   DAO granted proposer, executor, and canceller roles");

  // Optionally revoke admin role from deployer (for production)
  // await timelock.revokeRole(ADMIN_ROLE, deployer.address);

  // Summary
  console.log("\n╔══════════════════════════════════════════════════════════════╗");
  console.log("║                    DEPLOYMENT COMPLETE                        ║");
  console.log("╚══════════════════════════════════════════════════════════════╝");
  console.log("\n📝 Contract Addresses:");
  console.log("   FAT Token:        ", await fatToken.getAddress());
  console.log("   Timelock:         ", await timelock.getAddress());
  console.log("   DatachainDAO:     ", await dao.getAddress());
  console.log("   Treasury:         ", await treasury.getAddress());
  console.log("   Agent Reputation: ", await agentRep.getAddress());

  // Export addresses
  const addresses = {
    fatToken: await fatToken.getAddress(),
    timelock: await timelock.getAddress(),
    dao: await dao.getAddress(),
    treasury: await treasury.getAddress(),
    agentReputation: await agentRep.getAddress(),
    deployer: deployer.address,
    chainId: (await ethers.provider.getNetwork()).chainId.toString(),
    timestamp: new Date().toISOString(),
  };

  console.log("\n📄 Deployment data saved to: deployments/latest.json");
  
  // Write to file (in production, use hardhat-deploy or similar)
  const fs = require("fs");
  if (!fs.existsSync("deployments")) {
    fs.mkdirSync("deployments");
  }
  fs.writeFileSync("deployments/latest.json", JSON.stringify(addresses, null, 2));

  console.log("\n✅ Deployment successful!");
}

main()
  .then(() => process.exit(0))
  .catch((error) => {
    console.error(error);
    process.exit(1);
  });
