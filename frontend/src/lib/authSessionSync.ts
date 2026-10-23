const AUTH_CHANNEL_NAME = "arenax-auth-session";
const REFRESH_MARKER_KEY = "arenax_auth_refresh_sync";
const REFRESH_LOCK_KEY = "arenax_auth_refresh_lock";
const LOGOUT_MARKER_KEY = "arenax_auth_logout_sync";
const REFRESH_LOCK_TTL_MS = 30_000;
const REFRESH_WAIT_MS = 100;
const REFRESH_WAIT_TIMEOUT_MS = 35_000;

type AuthSessionEvent =
  | { type: "refreshed"; at: number; expiresIn: number }
  | { type: "logout"; at: number };

type RefreshMarker = { at: number; expiresIn: number };
type RefreshLock = { id: string; at: number };

let channel: BroadcastChannel | null = null;
let latestRefresh: RefreshMarker | null = null;
let refreshInFlight: Promise<number> | null = null;
const listeners = new Set<(event: AuthSessionEvent) => void>();

function readRefreshMarker(): RefreshMarker | null {
  if (typeof window === "undefined") return null;
  try {
    const value = window.localStorage.getItem(REFRESH_MARKER_KEY);
    if (!value) return null;
    const marker = JSON.parse(value) as RefreshMarker;
    return Number.isFinite(marker.at) && Number.isFinite(marker.expiresIn)
      ? marker
      : null;
  } catch {
    return null;
  }
}

function readRefreshLock(): RefreshLock | null {
  try {
    const value = window.localStorage.getItem(REFRESH_LOCK_KEY);
    if (!value) return null;
    const lock = JSON.parse(value) as RefreshLock;
    return typeof lock.id === "string" && Number.isFinite(lock.at)
      ? lock
      : null;
  } catch {
    return null;
  }
}

function waitForRefreshLock(): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, REFRESH_WAIT_MS));
}

function dispatch(event: AuthSessionEvent): void {
  if (event.type === "refreshed") latestRefresh = { at: event.at, expiresIn: event.expiresIn };
  listeners.forEach((listener) => listener(event));
}

function getChannel(): BroadcastChannel | null {
  if (typeof window === "undefined" || typeof BroadcastChannel === "undefined") {
    return null;
  }
  if (channel) return channel;

  channel = new BroadcastChannel(AUTH_CHANNEL_NAME);
  channel.onmessage = (message: MessageEvent<AuthSessionEvent>) => {
    dispatch(message.data);
  };
  window.addEventListener("storage", (event) => {
    if (event.key === REFRESH_MARKER_KEY) {
      const marker = readRefreshMarker();
      if (marker) dispatch({ type: "refreshed", ...marker });
    } else if (event.key === LOGOUT_MARKER_KEY && event.newValue) {
      const at = Number(event.newValue);
      if (Number.isFinite(at)) dispatch({ type: "logout", at });
    }
  });
  return channel;
}

export function subscribeToAuthSession(
  listener: (event: AuthSessionEvent) => void,
): () => void {
  getChannel();
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function broadcastAuthLogout(): void {
  if (typeof window === "undefined") return;
  const event: AuthSessionEvent = { type: "logout", at: Date.now() };
  try {
    window.localStorage.setItem(LOGOUT_MARKER_KEY, String(event.at));
  } catch {
    // BroadcastChannel remains available when storage is restricted.
  }
  getChannel()?.postMessage(event);
}

export async function coordinateTokenRefresh(
  refresh: () => Promise<number>,
): Promise<number> {
  if (refreshInFlight) return refreshInFlight;

  const startedAt = Date.now();
  const runRefresh = async (): Promise<number> => {
    latestRefresh = readRefreshMarker() ?? latestRefresh;
    if (
      latestRefresh &&
      (latestRefresh.at > startedAt || Date.now() - latestRefresh.at < 5_000)
    ) {
      return latestRefresh.expiresIn;
    }

    const expiresIn = await refresh();
    const marker = { at: Date.now(), expiresIn };
    latestRefresh = marker;
    try {
      window.localStorage.setItem(REFRESH_MARKER_KEY, JSON.stringify(marker));
    } catch {
      // The refresh remains valid; the channel still informs open tabs.
    }
    getChannel()?.postMessage({ type: "refreshed", ...marker } satisfies AuthSessionEvent);
    return expiresIn;
  };

  if (typeof window === "undefined") return refresh();

  const locks = typeof navigator !== "undefined" ? navigator.locks : undefined;
  if (!locks) {
    refreshInFlight = (async () => {
      const lockId = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      const deadline = Date.now() + REFRESH_WAIT_TIMEOUT_MS;

      while (Date.now() < deadline) {
        const currentLock = readRefreshLock();
        if (!currentLock || Date.now() - currentLock.at > REFRESH_LOCK_TTL_MS) {
          try {
            window.localStorage.setItem(
              REFRESH_LOCK_KEY,
              JSON.stringify({ id: lockId, at: Date.now() }),
            );
          } catch {
            return refresh();
          }

          if (readRefreshLock()?.id === lockId) {
            try {
              return await runRefresh();
            } finally {
              if (readRefreshLock()?.id === lockId) {
                window.localStorage.removeItem(REFRESH_LOCK_KEY);
              }
            }
          }
        }

        const marker = readRefreshMarker();
        if (marker && marker.at > startedAt) return marker.expiresIn;
        await waitForRefreshLock();
      }

      throw new Error("Token refresh coordination timed out");
    })().finally(() => {
      refreshInFlight = null;
    });
    return refreshInFlight;
  }

  refreshInFlight = locks
    .request("arenax-auth-token-refresh", runRefresh)
    .finally(() => {
      refreshInFlight = null;
    });
  return refreshInFlight;
}