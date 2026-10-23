"use client";

import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/Card";
import { formatAssetAmount } from "@/lib/wallet/balances";
import { WalletBalances, WalletAssetCode } from "@/lib/wallet/types";

interface BalanceCardsProps {
  isConnected: boolean;
  isLoading: boolean;
  hasError: boolean;
  isStale: boolean;
  balances: WalletBalances;
}

const ASSET_ORDER: WalletAssetCode[] = ["XLM", "USDC", "ARENAX"];

export function BalanceCards({
  isConnected,
  isLoading,
  hasError,
  isStale,
  balances,
}: BalanceCardsProps) {
  if (!isConnected) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Balances</CardTitle>
          <CardDescription>Connect a wallet to view balances.</CardDescription>
        </CardHeader>
      </Card>
    );
  }

  if (isLoading) {
    return (
      <div className="grid gap-4 md:grid-cols-3">
        {ASSET_ORDER.map((asset) => (
          <Card key={asset}>
            <CardHeader>
              <CardTitle className="text-base">{asset}</CardTitle>
            </CardHeader>
            <CardContent>
              <p className="text-sm text-muted-foreground">Loading balance...</p>
            </CardContent>
          </Card>
        ))}
      </div>
    );
  }

  if (hasError) {
    return (
      <Card role="alert">
        <CardHeader>
          <CardTitle>Balances unavailable</CardTitle>
          <CardDescription>
            Balance data could not be loaded. No zero balance is being shown as authoritative.
          </CardDescription>
        </CardHeader>
      </Card>
    );
  }

  return (
    <div className="grid gap-4 md:grid-cols-3">
      {ASSET_ORDER.map((asset) => {
        const balance = balances[asset];

        return (
          <Card key={asset}>
            <CardHeader className="pb-3">
              <CardTitle className="text-base">{asset}</CardTitle>
            </CardHeader>
            <CardContent className="space-y-3 text-sm">
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">Available</span>
                <span className="font-semibold" data-testid={`${asset}-available`}>
                  {formatAssetAmount(balance.available, balance.precision)}
                </span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">Held (Escrow)</span>
                <span className="font-semibold" data-testid={`${asset}-held`}>
                  {balance.held === null
                    ? "Unavailable"
                    : formatAssetAmount(balance.held, balance.precision)}
                </span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">Pending settlement</span>
                <span className="font-semibold" data-testid={`${asset}-pending`}>
                  {balance.pending === null
                    ? "Unavailable"
                    : formatAssetAmount(balance.pending, balance.precision)}
                </span>
              </div>
              <p className="text-xs text-muted-foreground">
                As of {new Date(balance.asOf).toLocaleString()}
                {isStale && " · May be outdated"}
              </p>

              {!balance.hasTrustline && asset !== "XLM" && (
                <p className="rounded-md bg-amber-100/70 px-2 py-1 text-xs text-amber-900 dark:bg-amber-900/20 dark:text-amber-200">
                  Trustline missing for {asset}. Add trustline in your wallet before transferring.
                </p>
              )}
            </CardContent>
          </Card>
        );
      })}
    </div>
  );
}
