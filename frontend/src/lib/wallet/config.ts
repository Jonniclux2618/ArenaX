import { Networks } from "@stellar/stellar-sdk";
import { AssetConfig, StellarNetwork, TxKind, WalletAssetCode } from "@/lib/wallet/types";

export const parseStellarNetwork = (value?: string): StellarNetwork => {
  if (value === undefined) return "testnet";
  const normalized = value.trim().toLowerCase();
  if (normalized === "mainnet" || normalized === "public") return "mainnet";
  if (normalized === "testnet") return "testnet";
  throw new Error(
    "NEXT_PUBLIC_STELLAR_NETWORK must be mainnet, public, or testnet"
  );
};

const network = parseStellarNetwork(process.env.NEXT_PUBLIC_STELLAR_NETWORK);

const isMainnet = network === "mainnet";

const defaultHorizon = isMainnet
  ? "https://horizon.stellar.org"
  : "https://horizon-testnet.stellar.org";

const defaultSorobanRpc = isMainnet
  ? "https://soroban-mainnet.stellar.org"
  : "https://soroban-testnet.stellar.org";

const appEnvironment = process.env.NEXT_PUBLIC_APP_ENV?.toLowerCase() ?? "development";
if (!["development", "preview", "production"].includes(appEnvironment)) {
  throw new Error(
    "NEXT_PUBLIC_APP_ENV must be development, preview, or production"
  );
}
const productionEnvironment = appEnvironment === "production";
if (productionEnvironment && !isMainnet) {
  throw new Error(
    "Production frontend builds require NEXT_PUBLIC_STELLAR_NETWORK=mainnet"
  );
}

const validateStellarEndpoint = (name: string, value: string): void => {
  let endpoint: URL;
  try {
    endpoint = new URL(value);
  } catch {
    throw new Error(`${name} must be a valid URL`);
  }
  if (endpoint.protocol !== "https:" && endpoint.protocol !== "http:") {
    throw new Error(`${name} must use HTTP or HTTPS`);
  }
  const hostname = endpoint.hostname.toLowerCase();
  const knownMainnet =
    hostname === "horizon.stellar.org" ||
    hostname === "soroban-mainnet.stellar.org";
  if (isMainnet && hostname.includes("testnet")) {
    throw new Error(`${name} points to testnet while mainnet is selected`);
  }
  if (!isMainnet && knownMainnet) {
    throw new Error(`${name} points to mainnet while testnet is selected`);
  }
  if (productionEnvironment && endpoint.protocol !== "https:") {
    throw new Error(`${name} must use HTTPS in production`);
  }
};

validateStellarEndpoint(
  "NEXT_PUBLIC_STELLAR_HORIZON_URL",
  process.env.NEXT_PUBLIC_STELLAR_HORIZON_URL ?? defaultHorizon
);
validateStellarEndpoint(
  "NEXT_PUBLIC_SOROBAN_RPC_URL",
  process.env.NEXT_PUBLIC_SOROBAN_RPC_URL ?? defaultSorobanRpc
);

const arenaxIssuer = process.env.NEXT_PUBLIC_ARENAX_ISSUER;
const arenaxContractId = process.env.NEXT_PUBLIC_ARENAX_CONTRACT_ID;

if (productionEnvironment) {
  for (const [name, value] of [
    ["NEXT_PUBLIC_USDC_ISSUER", process.env.NEXT_PUBLIC_USDC_ISSUER, "G"],
    ["NEXT_PUBLIC_ARENAX_ISSUER", arenaxIssuer, "G"],
    ["NEXT_PUBLIC_ARENAX_CONTRACT_ID", arenaxContractId, "C"],
  ] as const) {
    if (value && !new RegExp(`^${prefix}[A-Z2-7]{55}$`).test(value)) {
      throw new Error(`${name} must be a valid Stellar address`);
    }
  }
  if (!arenaxIssuer && !arenaxContractId) {
    throw new Error(
      "Production requires NEXT_PUBLIC_ARENAX_ISSUER or NEXT_PUBLIC_ARENAX_CONTRACT_ID"
    );
  }
}

const arenaxSource: AssetConfig["source"] =
  arenaxContractId && !arenaxIssuer ? "soroban" : "classic";

const explorerNetworkSegment = isMainnet ? "public" : "testnet";

const defaultStellarExplorerBase = `https://stellar.expert/explorer/${explorerNetworkSegment}/tx`;
const defaultSorobanExplorerBase = `https://stellar.expert/explorer/${explorerNetworkSegment}/tx`;

export const walletConfig = {
  network,
  networkPassphrase: isMainnet ? Networks.PUBLIC : Networks.TESTNET,
  horizonUrl: process.env.NEXT_PUBLIC_STELLAR_HORIZON_URL ?? defaultHorizon,
  sorobanRpcUrl: process.env.NEXT_PUBLIC_SOROBAN_RPC_URL ?? defaultSorobanRpc,
  stellarExplorerBase:
    process.env.NEXT_PUBLIC_STELLAR_EXPLORER_BASE_URL ??
    defaultStellarExplorerBase,
  sorobanExplorerBase:
    process.env.NEXT_PUBLIC_SOROBAN_EXPLORER_BASE_URL ??
    defaultSorobanExplorerBase,
  escrowBalancesEndpoint: process.env.NEXT_PUBLIC_ESCROW_BALANCES_ENDPOINT,
  arenaxSorobanBalanceEndpoint:
    process.env.NEXT_PUBLIC_ARENAX_SOROBAN_BALANCE_ENDPOINT,
  assets: {
    XLM: {
      code: "XLM",
      source: "native",
      issuer: undefined,
      contractId: undefined,
    },
    USDC: {
      code: "USDC",
      source: "classic",
      issuer: process.env.NEXT_PUBLIC_USDC_ISSUER,
      contractId: undefined,
    },
    ARENAX: {
      code: "ARENAX",
      source: arenaxSource,
      issuer: arenaxIssuer,
      contractId: arenaxContractId,
    },
  } satisfies Record<WalletAssetCode, AssetConfig>,
};

export const buildExplorerLink = (hash: string, kind: TxKind = "classic") => {
  const base =
    kind === "soroban"
      ? walletConfig.sorobanExplorerBase
      : walletConfig.stellarExplorerBase;

  return `${base.replace(/\/$/, "")}/${encodeURIComponent(hash)}`;
};
