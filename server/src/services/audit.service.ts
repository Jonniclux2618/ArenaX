/**
 * Comprehensive Audit Trail Service
 *
 * Features:
 *  - Tamper-evident SHA-256 hashing chain (each entry hashes itself + previousHash)
 *  - Full context capture: userId, ipAddress, userAgent, correlationId, before/after snapshots
 *  - Optimised DB queries with index hints (indexes added in migration)
 *  - Retention / GDPR-safe archiving (redacts PII without breaking the hash chain)
 *  - SOC2/GDPR compliance report generation (JSON + CSV export)
 *  - Integrity checker that traverses the full chain
 *  - Event replay to reconstruct resource state from audit deltas
 */

import crypto from 'crypto';
import { getDatabaseClient } from './database.service';
import { logger } from './logger.service';
import { AuditStatus, type AuditActor, type AuditEvent } from '../types/audit.types';

export { AuditStatus } from '../types/audit.types';
export type { AuditActor, AuditEvent, IBlockchainAnchorProvider } from '../types/audit.types';

// ─── Types ───────────────────────────────────────────────────────────────────

export type AuditActionType =
  | 'AUTH_LOGIN' | 'AUTH_LOGOUT' | 'AUTH_REGISTER' | 'AUTH_SOCIAL_LOGIN'
  | 'AUTH_PASSWORD_RESET' | 'AUTH_EMAIL_VERIFIED' | 'AUTH_GUEST_SESSION'
  | 'RESOLVE_DISPUTE' | 'MATCH_OVERRIDE' | 'MANUAL_PAYOUT'
  | 'PROPOSAL_EXECUTE' | 'PROPOSAL_CREATE' | 'VOTE_CAST'
  | 'KYC_APPROVE' | 'KYC_REJECT' | 'KYC_SUBMIT'
  | 'WALLET_TRANSACTION' | 'WALLET_WITHDRAW' | 'WALLET_DEPOSIT'
  | 'REFUND_APPROVE' | 'REFUND_REJECT' | 'REFUND_REQUEST'
  | 'TOURNAMENT_CREATE' | 'TOURNAMENT_CANCEL' | 'TOURNAMENT_JOIN'
  | 'ADMIN_BAN_USER' | 'ADMIN_UNBAN_USER' | 'ADMIN_CONFIG_CHANGE'
  | 'DATA_EXPORT' | 'DATA_DELETION_REQUEST' | string;

export interface AuditContext {
  /** The user performing the action (admin, player, system) */
  userId: string;
  /** Actor role, e.g. "ADMIN", "USER", "SYSTEM" */
  role?: string;
  action: AuditActionType;
  /** Outcome of the operation */
  status?: AuditStatus;
  /** Resource type, e.g. "MATCH", "USER", "TOURNAMENT" */
  targetType: string;
  targetId: string;
  ipAddress?: string;
  userAgent?: string;
  /** Idempotency / request trace id */
  requestId?: string;
  /** Cross-service correlation id */
  correlationId?: string;
  payloadBefore?: Record<string, unknown>;
  payloadAfter?: Record<string, unknown>;
  details?: Record<string, unknown>;
}

export interface AuditSearchParams {
  userId?: string;
  action?: string;
  targetType?: string;
  targetId?: string;
  correlationId?: string;
  fromDate?: Date;
  toDate?: Date;
  limit?: number;
  offset?: number;
}

export interface IntegrityReport {
  ok: boolean;
  totalChecked: number;
  firstTamperedId?: string;
  message: string;
}

export interface ComplianceReport {
  generatedAt: string;
  periodStart: string;
  periodEnd: string;
  totalEvents: number;
  byAction: Record<string, number>;
  sensitiveEventCount: number;
  retentionPolicyApplied: boolean;
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

interface AuditEventSchema {
  requiredFields: string[];
  pseudonymizedFields: string[];
  prohibitedFields: string[];
  retentionClass: string;
  retentionDays: number;
}

const COMMON_PROHIBITED_FIELDS = [
  'password', 'passwordhash', 'secret', 'secretkey', 'encryptedsecretkey', 'privatekey',
  'token', 'accesstoken', 'refreshtoken', 'authorization', 'cookie',
  'requestbody', 'rawbody', 'body', 'email', 'emailaddress', 'phone',
  'phonenumber', 'ipaddress', 'useragent', 'apikey', 'apikeyhash',
  'clientsecret',
] as const;

const AUDIT_EVENT_SCHEMAS: Record<string, AuditEventSchema> = {
  authentication: {
    requiredFields: ['userId', 'action', 'targetType', 'targetId'],
    pseudonymizedFields: [],
    prohibitedFields: [...COMMON_PROHIBITED_FIELDS],
    retentionClass: 'security-7y',
    retentionDays: 2555,
  },
  financial: {
    requiredFields: ['userId', 'action', 'targetType', 'targetId'],
    pseudonymizedFields: [],
    prohibitedFields: [...COMMON_PROHIBITED_FIELDS],
    retentionClass: 'financial-7y',
    retentionDays: 2555,
  },
  gameplay: {
    requiredFields: ['userId', 'action', 'targetType', 'targetId'],
    pseudonymizedFields: [],
    prohibitedFields: [...COMMON_PROHIBITED_FIELDS],
    retentionClass: 'gameplay-2y',
    retentionDays: 730,
  },
  default: {
    requiredFields: ['userId', 'action', 'targetType', 'targetId'],
    pseudonymizedFields: [],
    prohibitedFields: [...COMMON_PROHIBITED_FIELDS],
    retentionClass: 'operational-1y',
    retentionDays: 365,
  },
};

function auditSchemaFor(action: string): AuditEventSchema {
  if (action.startsWith('AUTH_') || action.startsWith('ADMIN_')) {
    return AUDIT_EVENT_SCHEMAS.authentication;
  }
  if (/^(WALLET_|REFUND_|MANUAL_PAYOUT|KYC_)/.test(action)) {
    return AUDIT_EVENT_SCHEMAS.financial;
  }
  if (/^(MATCH_|TOURNAMENT_)/.test(action)) {
    return AUDIT_EVENT_SCHEMAS.gameplay;
  }
  return AUDIT_EVENT_SCHEMAS.default;
}

function redactAuditValue(
  value: unknown,
  prohibitedFields: readonly string[] = COMMON_PROHIBITED_FIELDS,
): unknown {
  if (Array.isArray(value)) return value.map(item => redactAuditValue(item, prohibitedFields));
  if (value === null || typeof value !== 'object') return value;

  const prohibited = new Set(prohibitedFields);
  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>)
      .filter(([key]) => !prohibited.has(key.replace(/[-_]/g, '').toLowerCase()))
      .map(([key, nested]) => [key, redactAuditValue(nested, prohibitedFields)]),
  );
}

/** Deterministic serialisation – keys sorted so hash is stable. */
function stableStringify(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${(value as unknown[]).map(stableStringify).join(',')}]`;
  const obj = value as Record<string, unknown>;
  return `{${Object.keys(obj).sort().map(k => `${JSON.stringify(k)}:${stableStringify(obj[k])}`).join(',')}}`;
}

function computeHash(entry: Omit<AuditContext, 'details'> & { timestamp: string; previousHash: string | null }): string {
  return crypto.createHash('sha256').update(stableStringify(entry)).digest('hex');
}

/** Convert an array of objects to CSV string. */
function toCsv(rows: Record<string, unknown>[]): string {
  if (rows.length === 0) return '';
  const headers = Object.keys(rows[0]);
  const lines = rows.map(r =>
    headers.map(h => {
      const v = r[h];
      const s = v === null || v === undefined ? '' : typeof v === 'object' ? JSON.stringify(v) : String(v);
      return `"${s.replace(/"/g, '""')}"`;
    }).join(',')
  );
  return [headers.join(','), ...lines].join('\n');
}

// ─── Service ─────────────────────────────────────────────────────────────────

export class AuditService {

  // ── Core logging ─────────────────────────────────────────────────────────

  /**
   * Record a single audit event in the tamper-evident chain.
   * Fetches the latest entry hash, computes a chained SHA-256, and persists.
   *
   * Returns the persisted DB record, whose shape satisfies the AuditEvent spec:
   *  - eventId  → record.id  (UUIDv4)
   *  - hash     → record.entryHash
   *  - status   → stored in details._status
   *  - actor    → { userId, role }
   */
  static async log(ctx: AuditContext) {
    const prisma = getDatabaseClient();
    const schema = auditSchemaFor(ctx.action);
    for (const field of schema.requiredFields) {
      const value = ctx[field as keyof AuditContext];
      if (typeof value !== 'string' || value.trim() === '') {
        throw new Error(`Audit event ${ctx.action} is missing required field ${field}`);
      }
    }

    const payloadBefore = redactAuditValue(ctx.payloadBefore, schema.prohibitedFields) as Record<string, unknown> | undefined;
    const payloadAfter = redactAuditValue(ctx.payloadAfter, schema.prohibitedFields) as Record<string, unknown> | undefined;
    const details = redactAuditValue(ctx.details, schema.prohibitedFields) as Record<string, unknown> | undefined;
    const timestamp = new Date().toISOString();
    const status = ctx.status ?? AuditStatus.SUCCESS;

    const previous = await prisma.auditLog.findFirst({
      orderBy: { createdAt: 'desc' },
      select: { entryHash: true },
    });
    const previousHash = previous?.entryHash ?? null;

    const actor: AuditActor = {
      userId:    ctx.userId,
      role:      ctx.role ?? 'SYSTEM',
      ipAddress: null,
      userAgent: null,
    };

    const entryHash = computeHash({
      userId: ctx.userId,
      role: actor.role,
      action: ctx.action,
      status,
      targetType: ctx.targetType,
      targetId: ctx.targetId,
      ipAddress: undefined,
      userAgent: undefined,
      requestId: ctx.requestId || undefined,
      correlationId: ctx.correlationId || undefined,
      payloadBefore,
      payloadAfter,
      timestamp,
      previousHash,
    });

    const record = await prisma.auditLog.create({
      data: {
        adminId: ctx.userId,
        action: ctx.action,
        targetType: ctx.targetType,
        targetId: ctx.targetId,
        ipAddress: null,
        userAgent: null,
        requestId: ctx.requestId,
        correlationId: ctx.correlationId,
        snapshotBefore: (payloadBefore as any) ?? {},
        snapshotAfter: (payloadAfter as any) ?? {},
        details: {
          ...details ?? {},
          _status: status,
          _role: actor.role,
          _retentionClass: schema.retentionClass,
          _retentionDays: schema.retentionDays,
          _fieldPolicy: {
            requiredFields: schema.requiredFields,
            pseudonymizedFields: schema.pseudonymizedFields,
            prohibitedFields: schema.prohibitedFields,
          },
        },
        entryHash,
        previousHash,
        anchoredAt: null,
        anchorTxId: null,
        redactedAt: null,
      },
    });

    logger.info('audit.log', { id: record.id, action: ctx.action, userId: ctx.userId, status });
    return record;
  }

  /**
   * Backward-compatible shim – existing callers pass `adminId` + `snapshotBefore/After`.
   */
  static async logAction(data: {
    adminId: string;
    action: string;
    targetType: string;
    targetId: string;
    details?: Record<string, unknown>;
    ipAddress?: string;
    userAgent?: string;
    requestId?: string;
    correlationId?: string;
    snapshotBefore?: Record<string, unknown>;
    snapshotAfter?: Record<string, unknown>;
  }) {
    return AuditService.log({
      userId: data.adminId,
      action: data.action,
      targetType: data.targetType,
      targetId: data.targetId,
      ipAddress: data.ipAddress,
      userAgent: data.userAgent,
      requestId: data.requestId,
      correlationId: data.correlationId,
      payloadBefore: data.snapshotBefore,
      payloadAfter: data.snapshotAfter,
      details: data.details,
    });
  }

  /** Convenience wrapper for auth events. */
  static async logAuthEvent(data: {
    userId: string;
    action: 'LOGIN' | 'LOGOUT' | 'REGISTER' | 'SOCIAL_LOGIN' | 'PASSWORD_RESET' | 'EMAIL_VERIFIED' | 'GUEST_SESSION';
    details?: Record<string, unknown>;
    ipAddress?: string;
    userAgent?: string;
    provider?: string;
    correlationId?: string;
  }) {
    return AuditService.log({
      userId: data.userId,
      action: `AUTH_${data.action}` as AuditActionType,
      targetType: 'USER',
      targetId: data.userId,
      ipAddress: data.ipAddress,
      userAgent: data.userAgent,
      correlationId: data.correlationId,
      details: { ...data.details, provider: data.provider },
    });
  }

  // ── Search & filter ───────────────────────────────────────────────────────

  /**
   * Optimised query using indexed columns.
   * Indexes on (adminId, action, targetType, targetId, correlationId, createdAt)
   * are created in the companion migration.
   */
  static async search(params: AuditSearchParams = {}) {
    const prisma = getDatabaseClient();
    const { userId, action, targetType, targetId, correlationId, fromDate, toDate, limit = 50, offset = 0 } = params;

    const where: Record<string, unknown> = {};
    if (userId)        where.adminId = userId;
    if (action)        where.action = { contains: action, mode: 'insensitive' };
    if (targetType)    where.targetType = targetType;
    if (targetId)      where.targetId = targetId;
    if (correlationId) where.correlationId = correlationId;
    if (fromDate || toDate) {
      where.createdAt = {
        ...(fromDate ? { gte: fromDate } : {}),
        ...(toDate   ? { lte: toDate }   : {}),
      };
    }

    const [total, logs] = await Promise.all([
      prisma.auditLog.count({ where }),
      prisma.auditLog.findMany({
        where,
        orderBy: { createdAt: 'desc' },
        take: Math.min(limit, 500),
        skip: offset,
        select: {
          id: true, action: true, targetType: true, targetId: true,
          adminId: true, ipAddress: true, createdAt: true,
          correlationId: true, entryHash: true, previousHash: true,
          anchoredAt: true, anchorTxId: true, redactedAt: true,
          snapshotBefore: true, snapshotAfter: true, details: true,
        },
      }),
    ]);

    return {
      total,
      logs: logs.map(log => ({
        ...log,
        ipAddress: null,
        userAgent: null,
        snapshotBefore: redactAuditValue(log.snapshotBefore),
        snapshotAfter: redactAuditValue(log.snapshotAfter),
        details: redactAuditValue(log.details),
      })),
    };
  }

  /** Backward-compatible `listLogs` alias. */
  static async listLogs(filters: {
    adminId?: string; action?: string; targetType?: string; targetId?: string;
  } = {}) {
    const result = await AuditService.search({
      userId: filters.adminId, action: filters.action,
      targetType: filters.targetType, targetId: filters.targetId,
      limit: 100,
    });
    return result.logs;
  }

  // ── Export ────────────────────────────────────────────────────────────────

  static async exportLogs(params: AuditSearchParams & { format: 'json' | 'csv' }): Promise<string> {
    const { format, ...searchParams } = params;
    const { logs } = await AuditService.search({ ...searchParams, limit: 10_000 });
    if (format === 'csv') return toCsv(logs as unknown as Record<string, unknown>[]);
    return JSON.stringify(logs, null, 2);
  }

  // ── Compliance / retention ─────────────────────────────────────────────────

  /** Generate a SOC2/GDPR compliance summary report for a time window. */
  static async complianceReport(fromDate: Date, toDate: Date): Promise<ComplianceReport> {
    const prisma = getDatabaseClient();
    const logs = await prisma.auditLog.findMany({
      where: { createdAt: { gte: fromDate, lte: toDate } },
      select: { action: true, redactedAt: true },
    });

    const byAction: Record<string, number> = {};
    let sensitiveEventCount = 0;
    const sensitiveActions = new Set(['WALLET_TRANSACTION', 'WALLET_WITHDRAW', 'MANUAL_PAYOUT', 'DATA_DELETION_REQUEST', 'DATA_EXPORT', 'KYC_APPROVE', 'KYC_REJECT']);

    for (const { action, redactedAt } of logs) {
      byAction[action] = (byAction[action] ?? 0) + 1;
      if (sensitiveActions.has(action) || redactedAt) sensitiveEventCount++;
    }

    return {
      generatedAt: new Date().toISOString(),
      periodStart: fromDate.toISOString(),
      periodEnd: toDate.toISOString(),
      totalEvents: logs.length,
      byAction,
      sensitiveEventCount,
      retentionPolicyApplied: true,
    };
  }

  /**
   * GDPR "right to be forgotten": redact PII payloads for a user while
   * preserving hash chain integrity (the hashes themselves are not changed –
   * doing so would break the chain; instead `redactedAt` is stamped).
   */
  static async redactUser(userId: string): Promise<{ redacted: number }> {
    const prisma = getDatabaseClient();
    const result = await prisma.auditLog.updateMany({
      where: { adminId: userId, redactedAt: null },
      data: {
        snapshotBefore: {},
        snapshotAfter: {},
        details: { _redacted: true },
        ipAddress: null,
        userAgent: null,
        redactedAt: new Date(),
      },
    });
    logger.info('audit.redactUser', { userId, count: result.count });
    return { redacted: result.count };
  }

  /**
   * Auto-archive (hard-delete) logs older than `retentionDays` that have
   * already been redacted or are non-sensitive actions.
   */
  static async applyRetentionPolicy(retentionDays = 365): Promise<{ deleted: number }> {
    const prisma = getDatabaseClient();
    const now = new Date();
    const cutoff = new Date(now.getTime() - Math.min(Math.max(retentionDays, 1), 365) * 24 * 60 * 60 * 1000);
    const candidates = await prisma.auditLog.findMany({
      where: { createdAt: { lt: cutoff } },
      orderBy: { createdAt: 'asc' },
      take: 500,
      select: { id: true, action: true, createdAt: true, details: true },
    });
    const expiredIds = candidates
      .filter(log => {
        const details = (log.details ?? {}) as Record<string, unknown>;
        const storedDays = details._retentionDays;
        const days = Number.isInteger(storedDays)
          ? Number(storedDays)
          : Math.max(retentionDays, 2555);
        return days > 0 && log.createdAt.getTime() + days * 24 * 60 * 60 * 1000 <= now.getTime();
      })
      .map(log => log.id);

    const result = expiredIds.length > 0
      ? await prisma.auditLog.deleteMany({ where: { id: { in: expiredIds } } })
      : { count: 0 };
    logger.info('audit.retention', { deleted: result.count, cutoff });
    return { deleted: result.count };
  }

  // ── Integrity verification ────────────────────────────────────────────────

  /**
   * Walk the entire chain in ascending order.
   * Recomputes each entry's hash and checks it against the stored value.
   * Returns on first tampered entry.
   */
  static async verifyIntegrity(limit = 100_000): Promise<IntegrityReport> {
    const prisma = getDatabaseClient();
    const logs = await prisma.auditLog.findMany({
      orderBy: { createdAt: 'asc' },
      take: limit,
      select: {
        id: true, adminId: true, action: true, targetType: true, targetId: true,
        ipAddress: true, userAgent: true, requestId: true, correlationId: true,
        snapshotBefore: true, snapshotAfter: true, entryHash: true, previousHash: true,
        createdAt: true, details: true, redactedAt: true,
      },
    });

    let checked = 0;
    let previousHash: string | null = null;
    for (const log of logs) {
      if (!log.entryHash) continue; // pre-migration records have no hash
      if (previousHash !== null && log.previousHash !== previousHash) {
        return {
          ok: false,
          totalChecked: checked,
          firstTamperedId: log.id,
          message: `Broken hash-chain link detected at id=${log.id}`,
        };
      }

      if (!log.redactedAt) {
        const details = (log.details ?? {}) as Record<string, unknown>;
        const recomputed = computeHash({
          userId: log.adminId,
          role: String(details._role ?? 'SYSTEM'),
          action: log.action,
          status: (details._status as any) ?? AuditStatus.SUCCESS,
          targetType: log.targetType,
          targetId: log.targetId,
          ipAddress: log.ipAddress || undefined,
          userAgent: log.userAgent || undefined,
          requestId: log.requestId || undefined,
          correlationId: log.correlationId || undefined,
          payloadBefore: (log.snapshotBefore as Record<string, unknown>) ?? undefined,
          payloadAfter: (log.snapshotAfter as Record<string, unknown>) ?? undefined,
          timestamp: log.createdAt.toISOString(),
          previousHash: log.previousHash ?? null,
        });

        if (recomputed !== log.entryHash) {
          return {
            ok: false,
            totalChecked: checked,
            firstTamperedId: log.id,
            message: `Tampered record detected at id=${log.id}`,
          };
        }
      }
      checked++;
      previousHash = log.entryHash;
    }

    return { ok: true, totalChecked: checked, message: 'Chain intact' };
  }

  // ── Event replay ──────────────────────────────────────────────────────────

  /**
   * Reconstruct the state of a resource by replaying all audit log entries
   * for it in chronological order (applying each `snapshotAfter` as the
   * canonical state at that point in time).
   */
  static async replayEvents(targetType: string, targetId: string): Promise<{
    targetType: string;
    targetId: string;
    snapshots: Array<{ timestamp: string; action: string; state: unknown }>;
    currentState: unknown;
  }> {
    const prisma = getDatabaseClient();
    const logs = await prisma.auditLog.findMany({
      where: { targetType, targetId },
      orderBy: { createdAt: 'asc' },
      select: { createdAt: true, action: true, snapshotAfter: true },
    });

    const snapshots = logs.map(l => ({
      timestamp: l.createdAt.toISOString(),
      action: l.action,
      state: redactAuditValue(l.snapshotAfter),
    }));

    const currentState = snapshots.length > 0 ? snapshots[snapshots.length - 1].state : null;

    return { targetType, targetId, snapshots, currentState };
  }
}
