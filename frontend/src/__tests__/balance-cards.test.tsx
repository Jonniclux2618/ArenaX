import { render, screen } from "@testing-library/react";
import { BalanceCards } from "@/components/wallet/BalanceCards";
import { fetchWalletBalances, formatAssetAmount } from "@/lib/wallet/balances";
import { WalletBalances } from "@/lib/wallet/types";

describe("Balance rendering", () => {
  afterEach(() => {
    jest.restoreAllMocks();
  });

  it("renders balances from mocked Horizon API response", async () => {
    const fetchMock = jest.fn().mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: async () => ({
          balances: [
            { asset_type: "native", balance: "100.5000000" },
            {
              asset_type: "credit_alphanum4",
              asset_code: "USDC",
              asset_issuer: "GUSDCISSUER00000000000000000000000000000000000000000000",
              balance: "22.1000000",
            },
            {
              asset_type: "credit_alphanum12",
              asset_code: "ARENAX",
              asset_issuer: "GARENAXISSUER000000000000000000000000000000000000000000",
              balance: "8.7500000",
            },
          ],
        }),
      } as Response);

    Object.defineProperty(globalThis, "fetch", {
      value: fetchMock,
      writable: true,
    });

    const balances = await fetchWalletBalances(
      "GCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCE",
    );

    render(
      <BalanceCards
        isConnected
        isLoading={false}
        hasError={false}
        isStale
        balances={balances}
      />,
    );

    expect(screen.getByTestId("XLM-available")).toHaveTextContent("100.5");
    expect(screen.getByTestId("USDC-available")).toHaveTextContent("22.1");
    expect(screen.getByTestId("ARENAX-available")).toHaveTextContent("8.75");
    expect(screen.getByTestId("USDC-held")).toHaveTextContent("Unavailable");
    expect(screen.getByTestId("USDC-pending")).toHaveTextContent("Unavailable");

    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("keeps zero, fractional, held, pending, and large balances separate at asset precision", () => {
    const asOf = "2026-10-20T12:00:00.000Z";
    const balances: WalletBalances = {
      XLM: {
        asset: "XLM",
        available: 0,
        held: 1.25,
        pending: 0,
        precision: 7,
        asOf,
        hasTrustline: true,
        source: "native",
      },
      USDC: {
        asset: "USDC",
        available: 12.3456789,
        held: 0,
        pending: 2.5,
        precision: 7,
        asOf,
        hasTrustline: true,
        source: "classic",
      },
      ARENAX: {
        asset: "ARENAX",
        available: 1_234_567_890.12345,
        held: 0,
        pending: 3.456,
        precision: 3,
        asOf,
        hasTrustline: true,
        source: "soroban",
      },
    };

    render(
      <BalanceCards
        isConnected
        isLoading={false}
        hasError={false}
        isStale={false}
        balances={balances}
      />,
    );

    expect(screen.getByTestId("XLM-available")).toHaveTextContent("0");
    expect(screen.getByTestId("XLM-held")).toHaveTextContent("1.25");
    expect(screen.getByTestId("USDC-available")).toHaveTextContent("12.3456789");
    expect(screen.getByTestId("USDC-pending")).toHaveTextContent("2.5");
    expect(screen.getByTestId("ARENAX-available")).toHaveTextContent(
      "1,234,567,890.123",
    );
    expect(screen.getByTestId("ARENAX-pending")).toHaveTextContent("3.456");
    expect(screen.getAllByText(/may be outdated/i)).toHaveLength(3);
    expect(formatAssetAmount(1.23456, 3)).toBe("1.235");
  });

  it("does not render a zero balance as authoritative after an initial fetch failure", () => {
    render(
      <BalanceCards
        isConnected
        isLoading={false}
        hasError
        isStale={false}
        balances={{} as WalletBalances}
      />,
    );

    expect(screen.getByText(/no zero balance is being shown as authoritative/i))
      .toBeInTheDocument();
    expect(screen.queryByTestId("XLM-available")).not.toBeInTheDocument();
  });
});
