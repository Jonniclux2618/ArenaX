import { walletConfig } from "@/lib/wallet/config";
import {
  AssetBalance,
  WalletAssetCode,
  WalletBalances,
} from "@/lib/wallet/types";

interface HorizonBalanceLine {
  asset_type: string;
  balance: string;
  asset_code?: string;
  asset_issuer?: string;
}

interface HorizonAccountResponse {
  balances: HorizonBalanceLine[];
}

interface EscrowBalanceRecord {
  held: Record<WalletAssetCode, number | null>;
  pending: Record<WalletAssetCode, number | null>;
}

interface SorobanBalance {
  amount: number;
  precision: number;
}

const ASSET_CODES: WalletAssetCode[] = ["XLM", "USDC", "ARENAX"];

const toNumber = (value: unknown): number => {
  const parsed =
    typeof value === "number" || typeof value === "string"
      ? Number(value)
      : Number.NaN;
  if (!Number.isFinite(parsed)) {
    throw new Error("Wallet balance response contains an invalid amount.");
  }
  return parsed;
};

const unavailableEscrowBalances = (): EscrowBalanceRecord => ({
  held: { XLM: null, USDC: null, ARENAX: null },
  pending: { XLM: null, USDC: null, ARENAX: null },
});

const buildAssetBalance = (
  asset: WalletAssetCode,
  available: number,
  held: number | null,
  pending: number | null,
  hasTrustline: boolean,
  precision: number,
  asOf: string,
): AssetBalance => {
  const config = walletConfig.assets[asset];

  return {
    asset,
    available,
    held,
    pending,
    precision,
    asOf,
    hasTrustline,
    source: config.source,
    issuer: config.issuer,
    contractId: config.contractId,
  };
};

const resolveUrl = (endpoint: string, publicKey: string) => {
  const origin =
    typeof window !== "undefined" ? window.location.origin : "http://localhost";
  const url = new URL(endpoint, origin);
  url.searchParams.set("address", publicKey);
  return url.toString();
};

const normalizeEscrowBalances = (raw: unknown): EscrowBalanceRecord => {
  if (!raw || typeof raw !== "object") {
    throw new Error("Escrow balance response is invalid.");
  }

  const response = raw as Record<string, unknown>;
  const categoryValues = (
    category: unknown,
  ): Record<WalletAssetCode, number | null> => {
    if (!category || typeof category !== "object") {
      return { XLM: null, USDC: null, ARENAX: null };
    }

    const values = category as Record<string, unknown>;
    return {
      XLM: values.XLM === undefined ? null : toNumber(values.XLM),
      USDC: values.USDC === undefined ? null : toNumber(values.USDC),
      ARENAX: values.ARENAX === undefined ? null : toNumber(values.ARENAX),
    };
  };

  return {
    held: categoryValues(
      response.held ??
        response.locked ??
        (ASSET_CODES.some((asset) => asset in response) ? response : undefined),
    ),
    pending: categoryValues(response.pending),
  };
};

const fetchHorizonAccount = async (
  publicKey: string,
): Promise<HorizonAccountResponse | null> => {
  const response = await fetch(
    `${walletConfig.horizonUrl.replace(/\/$/, "")}/accounts/${publicKey}`,
    { cache: "no-store" },
  );

  if (response.status === 404) {
    return null;
  }

  if (!response.ok) {
    throw new Error("Unable to load wallet balances from Horizon.");
  }

  return (await response.json()) as HorizonAccountResponse;
};

const findClassicTrustline = (
  balanceLines: HorizonBalanceLine[],
  assetCode: WalletAssetCode,
  issuer?: string,
): HorizonBalanceLine | undefined => {
  return balanceLines.find((line) => {
    if (line.asset_type === "native") {
      return false;
    }

    if (line.asset_code !== assetCode) {
      return false;
    }

    if (issuer) {
      return line.asset_issuer === issuer;
    }

    return true;
  });
};

const fetchArenaxSorobanBalance = async (
  publicKey: string,
): Promise<SorobanBalance> => {
  const endpoint = walletConfig.arenaxSorobanBalanceEndpoint;
  if (!endpoint) {
    throw new Error("ArenaX contract balance endpoint is not configured.");
  }

  const response = await fetch(resolveUrl(endpoint, publicKey), {
    cache: "no-store",
  });
  if (!response.ok) {
    throw new Error("Unable to load ArenaX contract balance.");
  }

  const payload = (await response.json()) as Record<string, unknown>;
  const amount = Number(payload.balance);
  const precision = Number(payload.decimals ?? payload.precision);
  if (
    !Number.isFinite(amount) ||
    !Number.isInteger(precision) ||
    precision < 0 ||
    precision > 20
  ) {
    throw new Error("ArenaX contract balance response is invalid.");
  }

  return { amount, precision };
};

export const fetchEscrowBalances = async (
  publicKey: string,
): Promise<EscrowBalanceRecord> => {
  const endpoint = walletConfig.escrowBalancesEndpoint;
  if (!endpoint) {
    return unavailableEscrowBalances();
  }

  const response = await fetch(resolveUrl(endpoint, publicKey), {
    cache: "no-store",
    headers: {
      "Content-Type": "application/json",
    },
  });

  if (!response.ok) {
    throw new Error("Unable to load held and pending wallet balances.");
  }

  return normalizeEscrowBalances(await response.json());
};

export const fetchWalletBalances = async (
  publicKey: string,
): Promise<WalletBalances> => {
  const [account, escrow] = await Promise.all([
    fetchHorizonAccount(publicKey),
    fetchEscrowBalances(publicKey),
  ]);

  const lines = account?.balances ?? [];
  const asOf = new Date().toISOString();

  const xlmLine = lines.find((line) => line.asset_type === "native");
  const xlmAvailable = toNumber(xlmLine?.balance ?? 0);

  const usdcTrustline = findClassicTrustline(
    lines,
    "USDC",
    walletConfig.assets.USDC.issuer,
  );
  const usdcAvailable = toNumber(usdcTrustline?.balance ?? 0);

  const arenaConfig = walletConfig.assets.ARENAX;

  let arenaxAvailable = 0;
  let arenaxPrecision = 7;
  let arenaxTrustline = true;

  if (arenaConfig.source === "soroban") {
    const contractBalance = await fetchArenaxSorobanBalance(publicKey);
    arenaxAvailable = contractBalance.amount;
    arenaxPrecision = contractBalance.precision;
  } else {
    const arenaTrustlineLine = findClassicTrustline(
      lines,
      "ARENAX",
      arenaConfig.issuer,
    );
    arenaxAvailable = toNumber(arenaTrustlineLine?.balance ?? 0);
    arenaxTrustline = Boolean(arenaTrustlineLine);
  }

  const balances: WalletBalances = {
    XLM: buildAssetBalance(
      "XLM",
      xlmAvailable,
      escrow.held.XLM,
      escrow.pending.XLM,
      true,
      7,
      asOf,
    ),
    USDC: buildAssetBalance(
      "USDC",
      usdcAvailable,
      escrow.held.USDC,
      escrow.pending.USDC,
      Boolean(usdcTrustline),
      7,
      asOf,
    ),
    ARENAX: buildAssetBalance(
      "ARENAX",
      arenaxAvailable,
      escrow.held.ARENAX,
      escrow.pending.ARENAX,
      arenaxTrustline,
      arenaxPrecision,
      asOf,
    ),
  };

  if (!account) {
    balances.USDC.hasTrustline = false;
    if (arenaConfig.source === "classic") {
      balances.ARENAX.hasTrustline = false;
    }
  }

  return balances;
};

export const createEmptyBalances = (): WalletBalances => {
  return ASSET_CODES.reduce((acc, asset) => {
    acc[asset] = buildAssetBalance(
      asset,
      0,
      null,
      null,
      asset === "XLM",
      7,
      new Date(0).toISOString(),
    );
    return acc;
  }, {} as WalletBalances);
};

export const formatAssetAmount = (amount: number, precision = 7) => {
  return amount.toLocaleString("en-US", {
    minimumFractionDigits: 0,
    maximumFractionDigits: precision,
  });
};
