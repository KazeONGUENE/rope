import { HardhatUserConfig } from "hardhat/config";
import "@nomicfoundation/hardhat-toolbox";
import * as dotenv from "dotenv";

dotenv.config();

const DEPLOYER_KEY = process.env.DEPLOYER_PRIVATE_KEY?.trim();
const accounts = DEPLOYER_KEY ? [DEPLOYER_KEY.startsWith("0x") ? DEPLOYER_KEY : `0x${DEPLOYER_KEY}`] : [];

const config: HardhatUserConfig = {
  solidity: {
    compilers: [
      {
        version: "0.8.17",
        settings: { optimizer: { enabled: true, runs: 200 } },
      },
      {
        version: "0.8.20",
        settings: { optimizer: { enabled: true, runs: 200 }, viaIR: true },
      },
      {
        version: "0.8.27",
        settings: {
          optimizer: { enabled: true, runs: 200 },
          viaIR: true,
          evmVersion: "cancun",
        },
      },
    ],
  },
  networks: {
    mainnet: {
      url: process.env.RPC_URL || "http://127.0.0.1:8545",
      chainId: 271828,
      accounts,
      gasPrice: 1000000000,
    },
    testnet: {
      url: "https://testnet.erpc.datachain.network",
      chainId: 271829,
      accounts,
      gasPrice: 1000000000,
    },
    localhost: {
      url: "http://127.0.0.1:8545",
      chainId: 271828,
      accounts,
    },
    hardhat: {
      chainId: 31337,
      forking: {
        url: "https://erpc.datachain.network",
        enabled: false,
      },
    },
  },
  etherscan: {
    apiKey: {
      mainnet: process.env.ETHERSCAN_API_KEY || "",
    },
    customChains: [
      {
        network: "mainnet",
        chainId: 271828,
        urls: {
          apiURL: "https://api.dcscan.io/api",
          browserURL: "https://dcscan.io",
        },
      },
    ],
  },
  gasReporter: {
    enabled: process.env.REPORT_GAS === "true",
    currency: "USD",
    coinmarketcap: process.env.COINMARKETCAP_API_KEY,
  },
  paths: {
    sources: "./src",
    tests: "./test",
    cache: "./cache",
    artifacts: "./artifacts",
  },
  mocha: {
    timeout: 120000,
  },
};

export default config;
