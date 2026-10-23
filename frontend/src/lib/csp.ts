/**
 * Nonce-based Content Security Policy (#1091).
 *
 * A per-request nonce lets `script-src` drop `'unsafe-inline'`/`'unsafe-eval'`
 * (which effectively disables CSP's script protection) while still allowing
 * Next.js's own inline scripts (`__NEXT_DATA__`, JSON-LD in `layout.tsx`) and
 * any `next/script` tag that's given the same nonce.
 *
 * Used from `middleware.ts` (generates the nonce + header per request) and
 * read back in `layout.tsx` via `headers()` from `next/headers`.
 */

export const NONCE_HEADER = "x-nonce";

/** 16 random bytes, base64-encoded — using Web Crypto so this runs in the Edge middleware runtime. */
export function generateNonce(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

export function buildContentSecurityPolicy(nonce: string): string {
  const stellarOrigins = [
    process.env.NEXT_PUBLIC_STELLAR_HORIZON_URL,
    process.env.NEXT_PUBLIC_SOROBAN_RPC_URL,
  ]
    .map((endpoint) => {
      if (!endpoint) return null;
      try {
        const url = new URL(endpoint);
        return url.protocol === "https:" ? url.origin : null;
      } catch {
        return null;
      }
    })
    .filter((origin): origin is string => origin !== null);
  const connectSources = [
    "'self'",
    "https://horizon-testnet.stellar.org",
    "https://horizon.stellar.org",
    "https://soroban-testnet.stellar.org",
    "https://soroban-mainnet.stellar.org",
    ...new Set(stellarOrigins),
  ];

  return [
    "default-src 'self'",
    // 'strict-dynamic' lets scripts the nonce'd script itself loads run too,
    // without falling back to 'unsafe-inline' for older browsers — a
    // fallback would silently defeat the nonce.
    `script-src 'self' 'nonce-${nonce}' 'strict-dynamic'`,
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data: blob: https:",
    "font-src 'self' data:",
    // Stellar Horizon/Soroban RPC plus same-origin app WebSockets only.
    `connect-src ${connectSources.join(" ")}`,
    "frame-ancestors 'none'",
    "object-src 'none'",
    "base-uri 'self'",
    "form-action 'self'",
  ]
    .join("; ")
    .concat(";");
}

/** Report-only in development so violations surface in the console without breaking the app; enforced in production (#1091). */
export function cspHeaderName(isDev: boolean): string {
  return isDev ? "Content-Security-Policy-Report-Only" : "Content-Security-Policy";
}
