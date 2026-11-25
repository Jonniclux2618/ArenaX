import { NextFunction, Request, Response } from 'express';
import { BaseError } from '../errors/base-error';
import {
    workspaceContextService,
    type WorkspaceAuditMeta,
    type WorkspacePermission,
} from '../services/workspace-context.service';

export interface WorkspacePropagationOptions {
    /** Require an explicit organizer workspace ID even for platform admins. */
    requireWorkspace?: boolean;
    /** Optional permission checked from authoritative workspace membership records. */
    requiredPermission?: WorkspacePermission;
}

const extractSingleString = (value: unknown): string | null => {
    if (typeof value === 'string' && value.trim().length > 0) {
        return value.trim();
    }
    if (Array.isArray(value) && typeof value[0] === 'string' && value[0].trim().length > 0) {
        return value[0].trim();
    }
    return null;
};

const getAuditMeta = (req: Request): WorkspaceAuditMeta => ({
    requestId: req.auditContext?.requestId ?? req.requestId,
    correlationId: req.correlationId,
    ipAddress: req.auditContext?.ipAddress ?? req.ip,
    userAgent: req.auditContext?.userAgent ?? req.header('user-agent'),
});

/**
 * Extracts and authoritatively verifies the caller's organizer workspace
 * context from persistent membership records, preventing header/param/body
 * workspace smuggling and attaching `req.workspaceContext` (#174).
 */
export const propagateWorkspaceContext =
    (options: WorkspacePropagationOptions = {}) =>
    async (req: Request, _res: Response, next: NextFunction): Promise<void> => {
        try {
            if (!req.user?.id) {
                return next(new BaseError('Unauthorized', 401, 'AUTHENTICATION_REQUIRED'));
            }

            const headerWorkspaceId = extractSingleString(req.headers['x-workspace-id']);
            const paramWorkspaceId = extractSingleString(req.params?.workspaceId);
            const queryWorkspaceId = extractSingleString(req.query?.workspaceId);
            const bodyWorkspaceId =
                req.body && typeof req.body === 'object'
                    ? extractSingleString((req.body as Record<string, unknown>).workspaceId)
                    : null;

            const candidates = [
                headerWorkspaceId,
                paramWorkspaceId,
                queryWorkspaceId,
                bodyWorkspaceId,
            ].filter((id): id is string => id !== null);

            const distinct = new Set(candidates);
            if (distinct.size > 1) {
                return next(
                    new BaseError(
                        'Conflicting workspace identifiers across request header, path, query, or body',
                        400,
                        'WORKSPACE_CONTEXT_CONFLICT',
                        { workspaceIds: [...distinct] }
                    )
                );
            }

            const requestedWorkspaceId = candidates[0] ?? null;

            const context = await workspaceContextService.resolveWorkspaceContext({
                actorId: req.user.id,
                workspaceId: requestedWorkspaceId,
                requiredPermission: options.requiredPermission,
                requireWorkspace: options.requireWorkspace,
                auditMeta: getAuditMeta(req),
            });

            req.workspaceContext = context;
            if (req.auditContext) {
                req.auditContext.workspaceId = context.workspaceId ?? undefined;
            }

            return next();
        } catch (error) {
            return next(error);
        }
    };

/**
 * Enforces authoritative workspace membership and role permission for a
 * specific administrative read or mutation endpoint (#174).
 */
export const requireWorkspacePermission =
    (permission: WorkspacePermission) =>
    async (req: Request, _res: Response, next: NextFunction): Promise<void> => {
        try {
            if (!req.user?.id) {
                return next(new BaseError('Unauthorized', 401, 'AUTHENTICATION_REQUIRED'));
            }

            const verified = await workspaceContextService.assertAuthorized(
                req.workspaceContext,
                permission,
                getAuditMeta(req)
            );
            req.workspaceContext = verified;
            return next();
        } catch (error) {
            return next(error);
        }
    };
