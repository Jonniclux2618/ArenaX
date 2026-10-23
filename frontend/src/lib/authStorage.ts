const LEGACY_AUTH_KEYS = [
  "auth_token",
  "refresh_token",
  "auth_refresh_token",
] as const;

/** Remove browser-persisted tokens from earlier client-auth implementations. */
export function clearLegacyAuthTokens(): void {
  if (typeof window === "undefined") return;
  for (const key of LEGACY_AUTH_KEYS) {
    window.localStorage.removeItem(key);
    window.sessionStorage.removeItem(key);
  }
}

clearLegacyAuthTokens();
