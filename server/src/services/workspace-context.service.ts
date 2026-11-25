import { BaseError } from '../errors/base-error';
import { AuditStatus } from '../types/audit.types';
import { AuditService } from './audit.service';
import { getDatabaseClient } from './database.service';
import { logger } from './logger.service';

export type WorkspaceRole = 'OWNER' | 'ADMIN' | 'OPERATOR' | 'MODERATOR' | 'VIEWER';
export type WorkspaceMembershipStatus = 'ACTIVE' | 'SUSPENDED' | 'REVOKED';

export type WorkspacePermission =
    | 'workspace:read'
    | 'workspace:admin'
    | 'users:read'
    | 'users:write'
    | 'tournaments:read'
    | 'tournaments:write'
    | 'disputes:read'
    | 'disputes:write'
    | 'payments:read'
    | 'payments:write'
    | 'moderation:read'
    | 'moderation:write'
    | 'games:read'
    | 'games:write'
    | 'audit:read'
    | 'refunds:read'
    | 'refunds:write';

export type WorkspaceResourceType =
    | 'USER'
    | 'TOURNAMENT'
    | 'MATCH'
    | 'DISPUTE'
    | 'PAYMENT'
    | 'MODERATION_ITEM'
    | 'REFUND_REQUEST'
    | 'GAME_CONFIG'
    | 'AUDIT_LOG';

export interface WorkspaceRecord {
    id: string;
    name: string;
    slug: string;
    ownerId: string;
    isActive: boolean;
}

export interface WorkspaceMembershipRecord {
    id: string;
    workspaceId: string;
    userId: string;
    role: WorkspaceRole;
    status: WorkspaceMembershipStatus;
}

export interface WorkspaceResourceRecord {
    workspaceId: string;
    resourceType: WorkspaceResourceType;
    resourceId: string;
}

export interface WorkspaceAuditMeta {
    requestId?: string;
    correlationId?: string;
    ipAddress?: string;
    userAgent?: string;
}

/**
 * Authoritatively resolved organizer workspace context propagated through
 * administrative routes, controllers, services, and batch operations (#174).
 */
export interface WorkspaceContext {
    /** Active organizer workspace ID, or null only for unscoped platform-admin calls. */
    workspaceId: string | null;
    /** Authoritative acting user ID. */
    actorId: string;
    /** Authoritative workspace role (or 'OWNER' for platform admin). */
    role: WorkspaceRole;
    /** Authoritative membership status verified from persistent storage. */
    membershipStatus: WorkspaceMembershipStatus;
    /** Effective permissions granted by the authoritative workspace role. */
    permissions: WorkspacePermission[];
    /** True only when authoritative DB User.role === 'ADMIN' and no workspace boundary is bypassed. */
    isPlatformAdmin: boolean;
    /** Timestamp when membership and role were last verified from authoritative records. */
    verifiedAt: Date;
}

export interface SecurityAuditRecord {
    actorId: string;
    action: string;
    status: AuditStatus;
    targetType: string;
    targetId: string;
    workspaceId: string | null;
    details: Record<string, unknown>;
    timestamp: string;
}

const WORKSPACE_ROLE_PERMISSIONS: Record<WorkspaceRole, WorkspacePermission[]> = {
    OWNER: [
        'workspace:read',
        'workspace:admin',
        'users:read',
        'users:write',
        'tournaments:read',
        'tournaments:write',
        'disputes:read',
        'disputes:write',
        'payments:read',
        'payments:write',
        'moderation:read',
        'moderation:write',
        'games:read',
        'games:write',
        'audit:read',
        'refunds:read',
        'refunds:write',
    ],
    ADMIN: [
        'workspace:read',
        'workspace:admin',
        'users:read',
        'users:write',
        'tournaments:read',
        'tournaments:write',
        'disputes:read',
        'disputes:write',
        'payments:read',
        'payments:write',
        'moderation:read',
        'moderation:write',
        'games:read',
        'games:write',
        'audit:read',
        'refunds:read',
        'refunds:write',
    ],
    OPERATOR: [
        'workspace:read',
        'users:read',
        'users:write',
        'tournaments:read',
        'tournaments:write',
        'disputes:read',
        'disputes:write',
        'payments:read',
        'payments:write',
        'moderation:read',
        'moderation:write',
        'games:read',
        'audit:read',
        'refunds:read',
        'refunds:write',
    ],
    MODERATOR: [
        'workspace:read',
        'users:read',
        'disputes:read',
        'disputes:write',
        'moderation:read',
        'moderation:write',
        'audit:read',
    ],
    VIEWER: [
        'workspace:read',
        'users:read',
        'tournaments:read',
        'disputes:read',
        'payments:read',
        'moderation:read',
        'games:read',
        'audit:read',
        'refunds:read',
    ],
};

export function getWorkspaceRolePermissions(role: WorkspaceRole): WorkspacePermission[] {
    return [...(WORKSPACE_ROLE_PERMISSIONS[role] ?? [])];
}

export function workspaceRoleHasPermission(
    role: WorkspaceRole,
    permission: WorkspacePermission
): boolean {
    return (WORKSPACE_ROLE_PERMISSIONS[role] ?? []).includes(permission);
}

// In-memory authoritative store used when running against test mocks that do
// not attach Prisma workspace delegates, or explicitly seeded in tests.
const inMemoryWorkspaces = new Map<string, WorkspaceRecord>();
const inMemoryMemberships = new Map<string, WorkspaceMembershipRecord>();
const inMemoryResources = new Map<string, WorkspaceResourceRecord>();
const workspaceSecurityAudits: SecurityAuditRecord[] = [];

const membershipKey = (workspaceId: string, userId: string): string =>
    `${workspaceId}:${userId}`;

const resourceKey = (resourceType: WorkspaceResourceType, resourceId: string): string =>
    `${resourceType}:${resourceId}`;

async function recordWorkspaceAudit(
    actorId: string,
    action: string,
    status: AuditStatus,
    targetType: string,
    targetId: string,
    workspaceId: string | null,
    details: Record<string, unknown>,
    meta?: WorkspaceAuditMeta
): Promise<void> {
    const entry: SecurityAuditRecord = {
        actorId,
        action,
        status,
        targetType,
        targetId,
        workspaceId,
        details,
        timestamp: new Date().toISOString(),
    };
    workspaceSecurityAudits.push(entry);

    const prisma = getDatabaseClient() as any;
    if (prisma?.auditLog?.create && prisma?.auditLog?.findFirst) {
        try {
            await AuditService.log({
                userId: actorId,
                action,
                status,
                targetType,
                targetId,
                requestId: meta?.requestId,
                correlationId: meta?.correlationId,
                ipAddress: meta?.ipAddress,
                userAgent: meta?.userAgent,
                details: {
                    ...details,
                    workspaceId,
                },
            });
        } catch (err) {
            logger.warn('Failed to persist workspace security audit log', {
                action,
                actorId,
                workspaceId,
                error: err instanceof Error ? err.message : String(err),
            });
        }
    }
}

export class WorkspaceContextService {
    /**
     * Authoritatively look up a workspace by ID from persistent storage.
     */
    async findWorkspace(workspaceId: string): Promise<WorkspaceRecord | null> {
        const prisma = getDatabaseClient() as any;
        if (prisma?.organizerWorkspace?.findUnique) {
            const ws = await prisma.organizerWorkspace.findUnique({
                where: { id: workspaceId },
            });
            if (ws) {
                return {
                    id: ws.id,
                    name: ws.name,
                    slug: ws.slug,
                    ownerId: ws.ownerId,
                    isActive: Boolean(ws.isActive),
                };
            }
        }
        return inMemoryWorkspaces.get(workspaceId) ?? null;
    }

    /**
     * Authoritatively look up membership & role for `(workspaceId, userId)`.
     * Never trusts caller-supplied JWT role claims or request body fields.
     */
    async findMembership(
        workspaceId: string,
        userId: string
    ): Promise<WorkspaceMembershipRecord | null> {
        const prisma = getDatabaseClient() as any;
        if (prisma?.workspaceMembership?.findUnique) {
            const membership = await prisma.workspaceMembership.findUnique({
                where: {
                    workspaceId_userId: {
                        workspaceId,
                        userId,
                    },
                },
            });
            if (membership) {
                return {
                    id: membership.id,
                    workspaceId: membership.workspaceId,
                    userId: membership.userId,
                    role: membership.role as WorkspaceRole,
                    status: membership.status as WorkspaceMembershipStatus,
                };
            }
        } else if (prisma?.workspaceMembership?.findFirst) {
            const membership = await prisma.workspaceMembership.findFirst({
                where: { workspaceId, userId },
            });
            if (membership) {
                return {
                    id: membership.id,
                    workspaceId: membership.workspaceId,
                    userId: membership.userId,
                    role: membership.role as WorkspaceRole,
                    status: membership.status as WorkspaceMembershipStatus,
                };
            }
        }

        return inMemoryMemberships.get(membershipKey(workspaceId, userId)) ?? null;
    }

    /**
     * Authoritatively look up the user's current platform record to verify
     * they still exist, are not soft-deleted, and read their current DB role.
     */
    private async findAuthoritativeUser(
        userId: string
    ): Promise<{ id: string; role: string; deletedAt?: Date | null } | null> {
        const prisma = getDatabaseClient() as any;
        if (!prisma?.user?.findUnique) {
            return null;
        }
        const user = await prisma.user.findUnique({
            where: { id: userId },
            select: { id: true, role: true, deletedAt: true },
        });
        if (!user || user.deletedAt) {
            return null;
        }
        return user;
    }

    /**
     * Resolve and verify workspace context from authoritative records.
     *
     * Rules:
     * - Platform admins (`User.role === 'ADMIN'` from DB) with no `workspaceId`
     *   receive an unscoped platform-admin context unless `requireWorkspace` is true.
     * - Any request carrying a `workspaceId` (or any non-platform-admin caller)
     *   MUST have an active `OrganizerWorkspace` and an `ACTIVE` `WorkspaceMembership`
     *   record whose role satisfies `requiredPermission` (when provided).
     */
    async resolveWorkspaceContext(params: {
        actorId: string;
        workspaceId?: string | null;
        requiredPermission?: WorkspacePermission;
        requireWorkspace?: boolean;
        auditMeta?: WorkspaceAuditMeta;
    }): Promise<WorkspaceContext> {
        const {
            actorId,
            workspaceId,
            requiredPermission,
            requireWorkspace = false,
            auditMeta,
        } = params;

        if (!actorId) {
            throw new BaseError(
                'Authentication is required to establish workspace context',
                401,
                'AUTHENTICATION_REQUIRED'
            );
        }

        const authoritativeUser = await this.findAuthoritativeUser(actorId);
        const isPlatformAdmin = authoritativeUser?.role === 'ADMIN';
        const normalizedWorkspaceId = workspaceId?.trim() || null;

        if (!normalizedWorkspaceId) {
            if (requireWorkspace || !isPlatformAdmin) {
                await recordWorkspaceAudit(
                    actorId,
                    'WORKSPACE_ACCESS_DENIED',
                    AuditStatus.FAILED,
                    'WORKSPACE',
                    'MISSING',
                    null,
                    {
                        reason: 'WORKSPACE_CONTEXT_REQUIRED',
                        requiredPermission: requiredPermission ?? null,
                    },
                    auditMeta
                );
                throw new BaseError(
                    'An organizer workspace context (X-Workspace-Id) is required for this operation',
                    403,
                    'WORKSPACE_CONTEXT_REQUIRED'
                );
            }

            return {
                workspaceId: null,
                actorId,
                role: 'OWNER',
                membershipStatus: 'ACTIVE',
                permissions: getWorkspaceRolePermissions('OWNER'),
                isPlatformAdmin: true,
                verifiedAt: new Date(),
            };
        }

        const workspace = await this.findWorkspace(normalizedWorkspaceId);
        if (!workspace || !workspace.isActive) {
            await recordWorkspaceAudit(
                actorId,
                'WORKSPACE_ACCESS_DENIED',
                AuditStatus.FAILED,
                'WORKSPACE',
                normalizedWorkspaceId,
                normalizedWorkspaceId,
                { reason: 'WORKSPACE_NOT_FOUND_OR_INACTIVE' },
                auditMeta
            );
            throw new BaseError(
                'Organizer workspace not found or inactive',
                403,
                'WORKSPACE_ACCESS_DENIED',
                { workspaceId: normalizedWorkspaceId }
            );
        }

        const membership = await this.findMembership(normalizedWorkspaceId, actorId);
        if (!membership) {
            await recordWorkspaceAudit(
                actorId,
                'WORKSPACE_ACCESS_DENIED',
                AuditStatus.FAILED,
                'WORKSPACE',
                normalizedWorkspaceId,
                normalizedWorkspaceId,
                {
                    reason: 'MEMBERSHIP_NOT_FOUND',
                    requiredPermission: requiredPermission ?? null,
                },
                auditMeta
            );
            throw new BaseError(
                'Caller is not an active member of the requested organizer workspace',
                403,
                'WORKSPACE_MEMBERSHIP_REQUIRED',
                { workspaceId: normalizedWorkspaceId }
            );
        }

        if (membership.status !== 'ACTIVE') {
            await recordWorkspaceAudit(
                actorId,
                'WORKSPACE_ACCESS_DENIED',
                AuditStatus.FAILED,
                'WORKSPACE',
                normalizedWorkspaceId,
                normalizedWorkspaceId,
                {
                    reason: 'MEMBERSHIP_INACTIVE',
                    membershipStatus: membership.status,
                },
                auditMeta
            );
            throw new BaseError(
                `Workspace membership is ${membership.status.toLowerCase()}`,
                403,
                'WORKSPACE_MEMBERSHIP_INACTIVE',
                { workspaceId: normalizedWorkspaceId, status: membership.status }
            );
        }

        const permissions = getWorkspaceRolePermissions(membership.role);
        if (requiredPermission && !permissions.includes(requiredPermission)) {
            await recordWorkspaceAudit(
                actorId,
                'WORKSPACE_ACCESS_DENIED',
                AuditStatus.FAILED,
                'WORKSPACE',
                normalizedWorkspaceId,
                normalizedWorkspaceId,
                {
                    reason: 'INSUFFICIENT_WORKSPACE_ROLE',
                    role: membership.role,
                    requiredPermission,
                },
                auditMeta
            );
            throw new BaseError(
                `Workspace role ${membership.role} lacks required permission: ${requiredPermission}`,
                403,
                'WORKSPACE_ROLE_INSUFFICIENT',
                {
                    workspaceId: normalizedWorkspaceId,
                    role: membership.role,
                    requiredPermission,
                }
            );
        }

        return {
            workspaceId: normalizedWorkspaceId,
            actorId,
            role: membership.role,
            membershipStatus: membership.status,
            permissions,
            isPlatformAdmin,
            verifiedAt: new Date(),
        };
    }

    /**
     * Re-verify authoritative membership and role for a specific permission
     * before executing any read or mutation.
     */
    async assertAuthorized(
        ctx: WorkspaceContext | undefined,
        requiredPermission: WorkspacePermission,
        auditMeta?: WorkspaceAuditMeta
    ): Promise<WorkspaceContext> {
        if (!ctx) {
            throw new BaseError(
                'Workspace authorization context is missing',
                403,
                'WORKSPACE_CONTEXT_REQUIRED'
            );
        }

        return this.resolveWorkspaceContext({
            actorId: ctx.actorId,
            workspaceId: ctx.workspaceId,
            requiredPermission,
            requireWorkspace: ctx.workspaceId !== null,
            auditMeta,
        });
    }

    /**
     * Resolve the authoritative workspace that owns `(resourceType, resourceId)`.
     */
    async getResourceWorkspaceId(
        resourceType: WorkspaceResourceType,
        resourceId: string
    ): Promise<string | null> {
        const prisma = getDatabaseClient() as any;

        // 1. Authoritative WorkspaceResource index table
        if (prisma?.workspaceResource?.findUnique) {
            const record = await prisma.workspaceResource.findUnique({
                where: {
                    resourceType_resourceId: {
                        resourceType,
                        resourceId,
                    },
                },
            });
            if (record?.workspaceId) {
                return record.workspaceId;
            }
        } else if (prisma?.workspaceResource?.findFirst) {
            const record = await prisma.workspaceResource.findFirst({
                where: { resourceType, resourceId },
            });
            if (record?.workspaceId) {
                return record.workspaceId;
            }
        }

        // 2. In-memory authoritative registry (used in tests / local registration)
        const inMem = inMemoryResources.get(resourceKey(resourceType, resourceId));
        if (inMem) {
            return inMem.workspaceId;
        }

        // 3. Domain-table ownership fallback for tournaments/payments/refunds
        if (resourceType === 'TOURNAMENT' && prisma?.tournament?.findUnique) {
            const tournament = await prisma.tournament.findUnique({
                where: { id: resourceId },
                select: { organizerId: true, metadata: true },
            });
            const metaWorkspaceId = (tournament?.metadata as Record<string, unknown> | null)?.workspaceId;
            if (typeof metaWorkspaceId === 'string' && metaWorkspaceId) {
                return metaWorkspaceId;
            }
            if (tournament?.organizerId) {
                const ownerWs = await this.findOwnerWorkspaceId(tournament.organizerId);
                if (ownerWs) return ownerWs;
            }
        }

        if (resourceType === 'PAYMENT' && prisma?.payment?.findUnique) {
            const payment = await prisma.payment.findUnique({
                where: { id: resourceId },
                include: { project: { select: { ownerId: true } } },
            });
            if (payment?.project?.ownerId) {
                const ownerWs = await this.findOwnerWorkspaceId(payment.project.ownerId);
                if (ownerWs) return ownerWs;
            }
        }

        if (resourceType === 'REFUND_REQUEST' && prisma?.refundRequest?.findUnique) {
            const refund = await prisma.refundRequest.findUnique({
                where: { id: resourceId },
                include: { payment: { include: { project: { select: { ownerId: true } } } } },
            });
            if (refund?.payment?.project?.ownerId) {
                const ownerWs = await this.findOwnerWorkspaceId(refund.payment.project.ownerId);
                if (ownerWs) return ownerWs;
            }
        }

        return null;
    }

    private async findOwnerWorkspaceId(ownerId: string): Promise<string | null> {
        const prisma = getDatabaseClient() as any;
        if (prisma?.organizerWorkspace?.findFirst) {
            const ws = await prisma.organizerWorkspace.findFirst({
                where: { ownerId, isActive: true },
                select: { id: true },
            });
            if (ws?.id) return ws.id;
        }
        for (const ws of inMemoryWorkspaces.values()) {
            if (ws.ownerId === ownerId && ws.isActive) {
                return ws.id;
            }
        }
        return null;
    }

    /**
     * Assert that a single resource belongs to the caller's active workspace.
     * Unscoped platform admins (`ctx.workspaceId === null && ctx.isPlatformAdmin`)
     * bypass single-tenant workspace filtering; any workspace-scoped context is
     * strictly isolated to its own workspace.
     */
    async assertResourceInWorkspace(
        ctx: WorkspaceContext | undefined,
        resourceType: WorkspaceResourceType,
        resourceId: string,
        auditMeta?: WorkspaceAuditMeta
    ): Promise<void> {
        if (!ctx) {
            throw new BaseError(
                'Workspace context is required for resource access',
                403,
                'WORKSPACE_CONTEXT_REQUIRED'
            );
        }

        if (ctx.workspaceId === null && ctx.isPlatformAdmin) {
            return;
        }

        if (!ctx.workspaceId) {
            throw new BaseError(
                'Workspace context is required for resource access',
                403,
                'WORKSPACE_CONTEXT_REQUIRED'
            );
        }

        const ownerWorkspaceId = await this.getResourceWorkspaceId(resourceType, resourceId);
        if (!ownerWorkspaceId || ownerWorkspaceId !== ctx.workspaceId) {
            await recordWorkspaceAudit(
                ctx.actorId,
                'WORKSPACE_CROSS_TENANT_BLOCKED',
                AuditStatus.FAILED,
                resourceType,
                resourceId,
                ctx.workspaceId,
                {
                    requestedWorkspaceId: ctx.workspaceId,
                    ownerWorkspaceId: ownerWorkspaceId ?? 'UNASSIGNED',
                    resourceType,
                    resourceId,
                },
                auditMeta
            );
            throw new BaseError(
                `Cross-workspace access denied for ${resourceType} resource`,
                403,
                'CROSS_WORKSPACE_ACCESS_DENIED',
                {
                    workspaceId: ctx.workspaceId,
                    resourceType,
                    resourceId,
                }
            );
        }
    }

    /**
     * Atomic pre-flight check for batch operations: verifies EVERY identifier
     * belongs to the active workspace BEFORE any single read or mutation runs.
     * If even one identifier belongs to another workspace, rejects the entire
     * batch with 403 and emits a security audit record.
     */
    async assertBatchResourcesInWorkspace(
        ctx: WorkspaceContext | undefined,
        resourceType: WorkspaceResourceType,
        resourceIds: string[],
        auditMeta?: WorkspaceAuditMeta
    ): Promise<void> {
        if (!ctx) {
            throw new BaseError(
                'Workspace context is required for batch operations',
                403,
                'WORKSPACE_CONTEXT_REQUIRED'
            );
        }

        if (ctx.workspaceId === null && ctx.isPlatformAdmin) {
            return;
        }

        if (!ctx.workspaceId) {
            throw new BaseError(
                'Workspace context is required for batch operations',
                403,
                'WORKSPACE_CONTEXT_REQUIRED'
            );
        }

        const uniqueIds = [...new Set(resourceIds.map((id) => String(id).trim()).filter(Boolean))];
        const disallowedIds: string[] = [];

        for (const id of uniqueIds) {
            const ownerWorkspaceId = await this.getResourceWorkspaceId(resourceType, id);
            if (!ownerWorkspaceId || ownerWorkspaceId !== ctx.workspaceId) {
                disallowedIds.push(id);
            }
        }

        if (disallowedIds.length > 0) {
            await recordWorkspaceAudit(
                ctx.actorId,
                'WORKSPACE_BATCH_CROSS_TENANT_BLOCKED',
                AuditStatus.FAILED,
                `${resourceType}_BATCH`,
                disallowedIds[0],
                ctx.workspaceId,
                {
                    workspaceId: ctx.workspaceId,
                    resourceType,
                    totalRequested: uniqueIds.length,
                    blockedCount: disallowedIds.length,
                    blockedResourceIds: disallowedIds,
                },
                auditMeta
            );
            throw new BaseError(
                `Batch operation rejected: ${disallowedIds.length} ${resourceType} identifier(s) are outside workspace scope`,
                403,
                'CROSS_WORKSPACE_ACCESS_DENIED',
                {
                    workspaceId: ctx.workspaceId,
                    resourceType,
                    blockedCount: disallowedIds.length,
                    blockedResourceIds: disallowedIds,
                }
            );
        }
    }

    /**
     * Return all authoritative resource IDs owned by `ctx.workspaceId` for a
     * given `resourceType`, or `null` when operating as an unscoped platform admin.
     */
    async listWorkspaceResourceIds(
        ctx: WorkspaceContext | undefined,
        resourceType: WorkspaceResourceType
    ): Promise<string[] | null> {
        if (!ctx || (ctx.workspaceId === null && ctx.isPlatformAdmin)) {
            return null;
        }

        if (!ctx.workspaceId) {
            return [];
        }

        const prisma = getDatabaseClient() as any;
        const ids = new Set<string>();

        if (prisma?.workspaceResource?.findMany) {
            const rows = await prisma.workspaceResource.findMany({
                where: {
                    workspaceId: ctx.workspaceId,
                    resourceType,
                },
                select: { resourceId: true },
            });
            for (const row of rows) {
                if (row?.resourceId) ids.add(row.resourceId);
            }
        }

        for (const rec of inMemoryResources.values()) {
            if (rec.workspaceId === ctx.workspaceId && rec.resourceType === resourceType) {
                ids.add(rec.resourceId);
            }
        }

        return [...ids];
    }

    /**
     * Register a resource under an organizer workspace (persisted when Prisma
     * delegate exists, and mirrored in-memory for test environments).
     */
    async registerResource(
        workspaceId: string,
        resourceType: WorkspaceResourceType,
        resourceId: string
    ): Promise<WorkspaceResourceRecord> {
        const record: WorkspaceResourceRecord = {
            workspaceId,
            resourceType,
            resourceId,
        };
        const prisma = getDatabaseClient() as any;
        if (prisma?.workspaceResource?.upsert) {
            await prisma.workspaceResource.upsert({
                where: {
                    resourceType_resourceId: {
                        resourceType,
                        resourceId,
                    },
                },
                create: record,
                update: { workspaceId },
            });
        }
        inMemoryResources.set(resourceKey(resourceType, resourceId), record);
        return record;
    }
}

export const workspaceContextService = new WorkspaceContextService();

// ─── Test & bootstrap helpers ────────────────────────────────────────────────

export function registerWorkspaceForTesting(workspace: WorkspaceRecord): void {
    inMemoryWorkspaces.set(workspace.id, { ...workspace });
}

export function registerWorkspaceMembershipForTesting(
    membership: WorkspaceMembershipRecord
): void {
    inMemoryMemberships.set(
        membershipKey(membership.workspaceId, membership.userId),
        { ...membership }
    );
}

export function registerWorkspaceResourceForTesting(
    resource: WorkspaceResourceRecord
): void {
    inMemoryResources.set(
        resourceKey(resource.resourceType, resource.resourceId),
        { ...resource }
    );
}

export function getWorkspaceSecurityAuditsForTesting(): SecurityAuditRecord[] {
    return [...workspaceSecurityAudits];
}

export function resetWorkspaceStateForTesting(): void {
    inMemoryWorkspaces.clear();
    inMemoryMemberships.clear();
    inMemoryResources.clear();
    workspaceSecurityAudits.length = 0;
}
