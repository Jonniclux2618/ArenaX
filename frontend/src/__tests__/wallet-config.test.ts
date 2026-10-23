import { parseStellarNetwork } from "@/lib/wallet/config";

describe("parseStellarNetwork", () => {
  test.each([
    [undefined, "testnet"],
    ["testnet", "testnet"],
    [" MAINNET ", "mainnet"],
    ["public", "mainnet"],
  ])("maps %s to %s", (value, expected) => {
    expect(parseStellarNetwork(value)).toBe(expected);
  });

  test.each(["", "preview", "production", "invalid"]) (
    "rejects unsupported network %s",
    (value) => {
      expect(() => parseStellarNetwork(value)).toThrow(
        "NEXT_PUBLIC_STELLAR_NETWORK must be mainnet, public, or testnet"
      );
    }
  );
});