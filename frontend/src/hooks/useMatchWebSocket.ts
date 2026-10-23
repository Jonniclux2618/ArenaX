"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { BracketMatchStatus, ScoreReport } from "@/types/bracket";
import { MatchEventSequencer, mergeMatchUpdate } from "@/lib/matchEventSequencer";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

export interface MatchUpdate {
  matchId: string;
  scorePlayer1?: number;
  scorePlayer2?: number;
  status?: BracketMatchStatus;
  winnerId?: string;
  message?: string;
  /** Monotonic per-match sequence number carried by the server (#33). */
  version?: number;
  timestamp: number;
}

export interface UseMatchWebSocketOptions {
  matchId: string;
  enabled?: boolean;
  /**
   * Callback invoked upon reconnect to refresh authoritative match and tournament
   * state before incremental updates resume (#34).
   */
  onResync?: () => Promise<void> | void;
  /**
   * Optional token / auth renewal function called when an authorization failure
   * occurs or on reconnect before opening a new socket (#34).
   */
  onRenewAuth?: () => Promise<string | boolean | null>;
}

export interface UseMatchWebSocketReturn {
  isConnected: boolean;
  lastUpdate: MatchUpdate | null;
  connectionError: string | null;
  /** True while a reconnect attempt is scheduled or in progress. */
  isReconnecting: boolean;
  /** True briefly after the connection is restored following a drop. */
  connectionRestored: boolean;
  /** True if reconnection was stopped due to permanent authorization failure. */
  isPermanentAuthFailure: boolean;
  /**
   * Epoch ms of the last successful synchronization — either an accepted live
   * event or an adopted authoritative snapshot (#33). `null` before the first
   * sync. Display-only; never gates user actions.
   */
  lastSyncedAt: number | null;
  /** Triggers an immediate authoritative state resync. */
  resyncAuthoritativeState: () => Promise<void>;
  reconnect: () => void;
  disconnect: () => void;
}

// ---------------------------------------------------------------------------
// Constants (#34)
// ---------------------------------------------------------------------------

/** Initial reconnect delay (ms). */
const INITIAL_RECONNECT_DELAY_MS = 500;

/** Maximum reconnect delay (ms). Bounded exponential backoff. */
const MAX_RECONNECT_DELAY_MS = 30_000;

/** Maximum reconnect attempts before surfacing an offline warning. */
const MAX_RECONNECT_ATTEMPTS = 8;

/** How long the "connection restored" flag stays set (ms). */
const RESTORED_NOTICE_MS = 3_000;

/** Heartbeat interval to detect zombie / dropped connections (ms). */
const HEARTBEAT_INTERVAL_MS = 25_000;

/** Maximum time to wait for a pong response before declaring connection dead (ms). */
const PONG_TIMEOUT_MS = 8_000;

/** Close codes indicating permanent authorization failure where reconnect must stop (#34). */
const PERMANENT_AUTH_CODES = [1008, 4001, 4003, 4401, 4403];

// ---------------------------------------------------------------------------
// useMatchWebSocket
// ---------------------------------------------------------------------------

export function useMatchWebSocket({
  matchId,
  enabled = true,
  onResync,
  onRenewAuth,
}: UseMatchWebSocketOptions): UseMatchWebSocketReturn {
  const queryClient = useQueryClient();

  const [isConnected, setIsConnected] = useState(false);
  const [lastUpdate, setLastUpdate] = useState<MatchUpdate | null>(null);
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [isReconnecting, setIsReconnecting] = useState(false);
  const [connectionRestored, setConnectionRestored] = useState(false);
  const [isPermanentAuthFailure, setIsPermanentAuthFailure] = useState(false);
  const [lastSyncedAt, setLastSyncedAt] = useState<number | null>(null);

  const wsRef = useRef<WebSocket | null>(null);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const heartbeatTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const pongTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const restoredTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const retryCountRef = useRef(0);
  const closedRef = useRef(false);
  const isResyncingRef = useRef(false);
  const sequencerRef = useRef(new MatchEventSequencer());

  // ── Helpers ──────────────────────────────────────────────────────────────

  const clearTimers = useCallback(() => {
    if (reconnectTimerRef.current !== null) {
      clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
    if (heartbeatTimerRef.current !== null) {
      clearInterval(heartbeatTimerRef.current);
      heartbeatTimerRef.current = null;
    }
    if (pongTimerRef.current !== null) {
      clearTimeout(pongTimerRef.current);
      pongTimerRef.current = null;
    }
  }, []);

  const buildWsUrl = useCallback(() => {
    if (typeof window === "undefined") return "";
    const protocol = window.location.protocol === "https:" ? "wss" : "ws";
    return `${protocol}://${window.location.host}/ws/match/${encodeURIComponent(matchId)}`;
  }, [matchId]);

  // ── Authoritative State Resync (#34) ─────────────────────────────────────

  const resyncAuthoritativeState = useCallback(async () => {
    if (isResyncingRef.current || !matchId) return;
    isResyncingRef.current = true;
    try {
      // 1. Invalidate and refetch authoritative query state in React Query
      await Promise.allSettled([
        queryClient.invalidateQueries({ queryKey: ["match", matchId] }),
        queryClient.invalidateQueries({ queryKey: ["tournament"] }),
        queryClient.invalidateQueries({ queryKey: ["bracket"] }),
      ]);
      // 2. Invoke caller's custom resync handler if provided
      if (onResync) {
        await onResync();
      }
    } catch {
      // Non-fatal: resync failure should not break connection flow
    } finally {
      isResyncingRef.current = false;
    }
  }, [matchId, onResync, queryClient]);

  // ── Sequence baseline (#33) ───────────────────────────────────────────────

  /**
   * Re-anchor the event sequencer on the authoritative snapshot currently in
   * the query cache, re-opening the live stream after a gap or reconnect.
   */
  const adoptSnapshotBaseline = useCallback(() => {
    const cached = queryClient.getQueryData(["match", matchId]) as
      | { version?: number }
      | undefined;
    sequencerRef.current.adoptSnapshot(cached?.version);
    setLastSyncedAt(Date.now());
  }, [matchId, queryClient]);

  // ── Disconnect ────────────────────────────────────────────────────────────

  const disconnect = useCallback(() => {
    clearTimers();
    retryCountRef.current = 0;
    if (wsRef.current) {
      wsRef.current.onopen = null;
      wsRef.current.onmessage = null;
      wsRef.current.onclose = null;
      wsRef.current.onerror = null;
      try {
        wsRef.current.close(1000, "Client unmount/disconnect");
      } catch {}
      wsRef.current = null;
    }
    setIsConnected(false);
    setIsReconnecting(false);
  }, [clearTimers]);

  // ── Heartbeat Management ─────────────────────────────────────────────────

  const startHeartbeat = useCallback(() => {
    if (heartbeatTimerRef.current) clearInterval(heartbeatTimerRef.current);
    if (pongTimerRef.current) clearTimeout(pongTimerRef.current);

    heartbeatTimerRef.current = setInterval(() => {
      if (wsRef.current?.readyState === WebSocket.OPEN) {
        try {
          wsRef.current.send(JSON.stringify({ type: "ping", timestamp: Date.now() }));
          pongTimerRef.current = setTimeout(() => {
            // Heartbeat missed (e.g. mobile sleep / broken socket)
            if (wsRef.current) {
              try {
                wsRef.current.close(4000, "Heartbeat timeout");
              } catch {}
            }
          }, PONG_TIMEOUT_MS);
        } catch {
          // Send error
        }
      }
    }, HEARTBEAT_INTERVAL_MS);
  }, []);

  const stopHeartbeat = useCallback(() => {
    if (heartbeatTimerRef.current) {
      clearInterval(heartbeatTimerRef.current);
      heartbeatTimerRef.current = null;
    }
    if (pongTimerRef.current) {
      clearTimeout(pongTimerRef.current);
      pongTimerRef.current = null;
    }
  }, []);

  // ── Connect & Session Recovery ────────────────────────────────────────────

  const connect = useCallback(async () => {
    if (closedRef.current || !enabled || !matchId) return;
    if (isPermanentAuthFailure) return;

    // Clean up any existing connection
    if (wsRef.current) {
      wsRef.current.onopen = null;
      wsRef.current.onmessage = null;
      wsRef.current.onclose = null;
      wsRef.current.onerror = null;
      try {
        wsRef.current.close(1000, "Opening new connection");
      } catch {}
      wsRef.current = null;
    }

    setConnectionError(null);

    // Optional auth renewal before reconnecting
    if (onRenewAuth && retryCountRef.current > 0) {
      try {
        const renewed = await onRenewAuth();
        if (renewed === false) {
          setIsPermanentAuthFailure(true);
          setIsReconnecting(false);
          setConnectionError("Session expired. Please sign in again.");
          return;
        }
      } catch {
        // Renewal threw — back off
      }
    }

    let ws: WebSocket;
    try {
      ws = new WebSocket(buildWsUrl());
    } catch {
      setConnectionError("Unable to open match feed connection.");
      return;
    }

    wsRef.current = ws;

    ws.onopen = async () => {
      if (closedRef.current) {
        try {
          ws.close(1000, "Component unmounted");
        } catch {}
        return;
      }

      const wasReconnecting = retryCountRef.current > 0;
      retryCountRef.current = 0;
      setIsConnected(true);
      setIsReconnecting(false);
      setConnectionError(null);
      setIsPermanentAuthFailure(false);
      startHeartbeat();

      if (wasReconnecting) {
        setConnectionRestored(true);
        if (restoredTimerRef.current) clearTimeout(restoredTimerRef.current);
        restoredTimerRef.current = setTimeout(
          () => setConnectionRestored(false),
          RESTORED_NOTICE_MS,
        );

        // Always refresh authoritative match and tournament state on reconnect (#34)
        await resyncAuthoritativeState();
        // Re-open the sequenced stream from the refreshed snapshot (#33)
        adoptSnapshotBaseline();
      } else {
        // First connection: adopt the snapshot's version (if any) so the
        // initial REST state and the live stream share one baseline (#33).
        adoptSnapshotBaseline();
      }
    };

    ws.onmessage = (event: MessageEvent) => {
      try {
        const data = JSON.parse(event.data as string) as Partial<MatchUpdate> & {
          type?: string;
          error?: string;
        };

        // Handle keepalive pong
        if (data.type === "pong" || data.type === "heartbeat") {
          if (pongTimerRef.current) {
            clearTimeout(pongTimerRef.current);
            pongTimerRef.current = null;
          }
          return;
        }

        // Server auth rejection message
        if (data.type === "auth_error" || data.error === "unauthorized") {
          setIsPermanentAuthFailure(true);
          setIsConnected(false);
          setConnectionError("Authentication failed. Please sign in again.");
          disconnect();
          return;
        }

        if (data.matchId ?? (data as Record<string, unknown>).match_id) {
          // Tolerate both camelCase and the Rust backend's snake_case frames.
          const raw = data as Record<string, unknown>;
          const frameMatchId = String(raw.matchId ?? raw.match_id);
          if (frameMatchId !== matchId) return;

          const version =
            typeof raw.version === "number" || typeof raw.version === "string"
              ? raw.version
              : undefined;

          // Sequence check (#33): duplicates and stale/reordered frames are
          // dropped before they can touch the cache; a gap suppresses the
          // stream and refetches authoritative state instead of patching.
          const verdict = sequencerRef.current.check(version);
          if (verdict.action === "drop_stale") {
            return;
          }
          if (verdict.action === "resync") {
            void resyncAuthoritativeState().then(adoptSnapshotBaseline);
            return;
          }

          const rawTimestamp = raw.timestamp;
          const timestamp =
            typeof rawTimestamp === "number"
              ? rawTimestamp
              : typeof rawTimestamp === "string" && !Number.isNaN(Date.parse(rawTimestamp))
                ? Date.parse(rawTimestamp)
                : Date.now();

          const update: MatchUpdate = {
            matchId: frameMatchId,
            scorePlayer1: (raw.scorePlayer1 ?? raw.score_player1) as number | undefined,
            scorePlayer2: (raw.scorePlayer2 ?? raw.score_player2) as number | undefined,
            status: (raw.status ?? undefined) as BracketMatchStatus | undefined,
            winnerId: (raw.winnerId ?? raw.winner_id) as string | undefined,
            message: (raw.message ?? raw.detail) as string | undefined,
            version: verdict.version ?? undefined,
            timestamp,
          };

          // Patch the query cache so every consumer (hub, detail view,
          // mobile view) converges on the same ordered state (#33).
          queryClient.setQueryData<Record<string, unknown>>(
            ["match", matchId],
            (previous) =>
              mergeMatchUpdate(previous as never, {
                status: update.status,
                scorePlayer1: update.scorePlayer1,
                scorePlayer2: update.scorePlayer2,
                winnerId: update.winnerId,
                version: verdict.version ?? undefined,
              }) as Record<string, unknown> | undefined,
          );

          setLastUpdate(update);
          setLastSyncedAt(Date.now());
        }
      } catch {
        // Ignore malformed frames
      }
    };

    ws.onclose = (event: CloseEvent) => {
      stopHeartbeat();
      setIsConnected(false);
      wsRef.current = null;

      if (closedRef.current) return;

      // Permanent authorization failures: stop reconnecting (#34)
      if (PERMANENT_AUTH_CODES.includes(event.code)) {
        setIsPermanentAuthFailure(true);
        setIsReconnecting(false);
        setConnectionError("Authentication failed. Reconnection stopped.");
        return;
      }

      // Normal closure
      if (event.code === 1000) return;

      if (retryCountRef.current >= MAX_RECONNECT_ATTEMPTS) {
        setIsReconnecting(false);
        setConnectionError("Connection lost. Showing last known state.");
        return;
      }

      // Bounded exponential reconnect with jitter (#34)
      const jitter = Math.random() * 300;
      const baseDelay = INITIAL_RECONNECT_DELAY_MS * 2 ** retryCountRef.current;
      const delay = Math.min(MAX_RECONNECT_DELAY_MS, baseDelay + jitter);

      retryCountRef.current += 1;
      setIsReconnecting(true);

      if (reconnectTimerRef.current) clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = setTimeout(() => {
        reconnectTimerRef.current = null;
        connect();
      }, delay);
    };

    ws.onerror = () => {
      try {
        ws.close();
      } catch {}
    };
  }, [
    adoptSnapshotBaseline,
    buildWsUrl,
    disconnect,
    enabled,
    isPermanentAuthFailure,
    matchId,
    onRenewAuth,
    resyncAuthoritativeState,
    startHeartbeat,
    stopHeartbeat,
  ]);

  // ── Public reconnect ──────────────────────────────────────────────────────

  const reconnect = useCallback(() => {
    clearTimers();
    retryCountRef.current = 0;
    setIsPermanentAuthFailure(false);
    connect();
  }, [clearTimers, connect]);

  // ── Network Handoff and Visibility Handling (#34) ─────────────────────────

  useEffect(() => {
    if (typeof window === "undefined" || !enabled || !matchId) return;

    const handleVisibilityChange = () => {
      if (document.visibilityState === "visible") {
        // App woke up from sleep / tab became active: check connection
        if (!wsRef.current || wsRef.current.readyState === WebSocket.CLOSED) {
          if (!isPermanentAuthFailure) {
            connect();
          }
        } else if (wsRef.current.readyState === WebSocket.OPEN) {
          // Send instant ping to verify socket isn't half-dead
          try {
            wsRef.current.send(JSON.stringify({ type: "ping", timestamp: Date.now() }));
          } catch {
            connect();
          }
        }
      }
    };

    const handleOnline = () => {
      // Network restored after offline / handoff
      if (!isPermanentAuthFailure) {
        clearTimers();
        retryCountRef.current = 0;
        connect();
      }
    };

    const handleOffline = () => {
      setIsConnected(false);
      setConnectionError("Network offline. Waiting for connection...");
    };

    document.addEventListener("visibilitychange", handleVisibilityChange);
    window.addEventListener("online", handleOnline);
    window.addEventListener("offline", handleOffline);

    return () => {
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      window.removeEventListener("online", handleOnline);
      window.removeEventListener("offline", handleOffline);
    };
  }, [clearTimers, connect, enabled, isPermanentAuthFailure, matchId]);

  // ── Mount / MatchId / Route Change Lifecycle (#34) ────────────────────────

  useEffect(() => {
    closedRef.current = false;
    setIsPermanentAuthFailure(false);
    // New match view: forget any previous stream baseline (#33)
    sequencerRef.current.reset();
    setLastSyncedAt(null);

    if (enabled && matchId) {
      connect();
    }

    return () => {
      closedRef.current = true;
      if (restoredTimerRef.current) clearTimeout(restoredTimerRef.current);
      stopHeartbeat();
      disconnect();
    };
  }, [connect, disconnect, enabled, matchId, stopHeartbeat]);

  return {
    isConnected,
    lastUpdate,
    connectionError,
    isReconnecting,
    connectionRestored,
    isPermanentAuthFailure,
    lastSyncedAt,
    resyncAuthoritativeState,
    reconnect,
    disconnect,
  };
}

// ---------------------------------------------------------------------------
// useMatchScoreReporting
// ---------------------------------------------------------------------------

interface ScoreSubmission {
  matchId: string;
  player1Score: number;
  player2Score: number;
  reporterId: string;
  reporterName?: string;
}

interface UseMatchScoreReportingOptions {
  expectedReport?: ScoreReport | null;
  matchId?: string;
}

class ScoreConflictError extends Error {
  constructor(public readonly expectedReport: ScoreReport) {
    super("SCORE_CONFLICT");
    this.name = "ScoreConflictError";
  }
}

interface UseMatchScoreReportingReturn {
  reportScore: (report: ScoreSubmission) => Promise<boolean>;
  pendingReport: ScoreReport | null;
  isReporting: boolean;
  conflictDetected: boolean;
  conflictingReport: ScoreReport | null;
  clearConflict: () => void;
}

export function useMatchScoreReporting(
  options: UseMatchScoreReportingOptions = {},
): UseMatchScoreReportingReturn {
  const queryClient = useQueryClient();
  const [conflictDetected, setConflictDetected] = useState(false);
  const [conflictingReport, setConflictingReport] = useState<ScoreReport | null>(null);

  const pendingKey = ["pendingScoreReport", options.matchId ?? "unknown"] as const;

  const { data: pendingReport = null } = useQuery({
    queryKey: pendingKey,
    queryFn: () => null as ScoreReport | null,
    staleTime: Infinity,
    enabled: false,
  });

  const mutation = useMutation({
    mutationFn: async (report: ScoreSubmission) => {
      await new Promise((resolve) => setTimeout(resolve, 900));

      if (
        options.expectedReport &&
        (options.expectedReport.player1Score !== report.player1Score ||
          options.expectedReport.player2Score !== report.player2Score)
      ) {
        throw new ScoreConflictError(options.expectedReport);
      }
      return report;
    },
    onMutate: (report) => {
      const submittedReport: ScoreReport = {
        reporterId: report.reporterId,
        reporterName: report.reporterName ?? "You",
        player1Score: report.player1Score,
        player2Score: report.player2Score,
        submittedAt: new Date().toISOString(),
      };
      queryClient.setQueryData(pendingKey, submittedReport);
    },
    onError: (error) => {
      if (error instanceof ScoreConflictError) {
        setConflictDetected(true);
        setConflictingReport(error.expectedReport);
      }
    },
    onSuccess: () => {
      setConflictDetected(false);
      setConflictingReport(null);
    },
  });

  const reportScore = useCallback(
    async (report: ScoreSubmission): Promise<boolean> => {
      try {
        await mutation.mutateAsync(report);
        return true;
      } catch (error) {
        if (error instanceof ScoreConflictError) return false;
        throw error;
      }
    },
    [mutation],
  );

  const clearConflict = useCallback(() => {
    setConflictDetected(false);
    setConflictingReport(null);
  }, []);

  return {
    reportScore,
    pendingReport,
    isReporting: mutation.isPending,
    conflictDetected,
    conflictingReport,
    clearConflict,
  };
}
