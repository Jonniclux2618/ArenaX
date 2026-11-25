import { Request, Response } from 'express';
import { MatchService } from '../services/match.service';
import { AuditService } from '../services/audit.service';
import paymentMonitorWorker from '../services/payment-monitor.worker';
import prisma from '../services/database.service';
import { createAdminService, AdminService } from '../services/admin.service';
import { confirmationService } from '../services/confirmation.service';
import { MaintenanceService } from '../services/maintenance.service';
import { isBaseError } from '../errors/base-error';
import {
    workspaceContextService,
    type WorkspaceAuditMeta,
} from '../services/workspace-context.service';

const matchService = new MatchService();
const adminService: AdminService = createAdminService();

const getAuditMeta = (req: Request): WorkspaceAuditMeta => ({
    requestId: req.auditContext?.requestId ?? req.requestId,
    correlationId: req.correlationId,
    ipAddress: req.auditContext?.ipAddress ?? req.ip,
    userAgent: req.auditContext?.userAgent ?? req.header('user-agent'),
});

const respondWithError = (res: Response, error: unknown, fallbackStatus = 500): void => {
    if (isBaseError(error)) {
        res.status(error.statusCode).json({
            error: error.message,
            code: error.code,
            details: error.details,
        });
        return;
    }
    res.status(fallbackStatus).json({ error: (error as Error).message });
};

export const listDisputes = async (req: Request, res: Response): Promise<void> => {
    try {
        const auditMeta = getAuditMeta(req);
        let allowedDisputeIds: string[] | null = null;
        if (req.workspaceContext) {
            const verified = await workspaceContextService.assertAuthorized(
                req.workspaceContext,
                'disputes:read',
                auditMeta
            );
            allowedDisputeIds = await workspaceContextService.listWorkspaceResourceIds(
                verified,
                'DISPUTE'
            );
        }

        const disputes = await matchService.listOpenDisputes();
        const scopedDisputes =
            allowedDisputeIds !== null
                ? disputes.filter((d: { id: string }) => allowedDisputeIds!.includes(d.id))
                : disputes;

        res.status(200).json(scopedDisputes);
    } catch (error) {
        respondWithError(res, error, 500);
    }
};

export const resolveDispute = async (req: Request, res: Response): Promise<void> => {
    try {
        const { id } = req.params;
        const { status, resolution, winnerOverrideId } = req.body;
        const auditMeta = getAuditMeta(req);

        if (req.workspaceContext) {
            const verified = await workspaceContextService.assertAuthorized(
                req.workspaceContext,
                'disputes:write',
                auditMeta
            );
            await workspaceContextService.assertResourceInWorkspace(
                verified,
                'DISPUTE',
                id,
                auditMeta
            );
        }

        const result = await matchService.resolveDispute(id, req.user!.id, {
            status,
            resolution,
            winnerOverrideId
        });

        // Detailed Audit
        await AuditService.logAction({
            adminId: req.user!.id,
            action: 'RESOLVE_DISPUTE',
            targetType: 'DISPUTE',
            targetId: id,
            details: { status, resolution, workspaceId: req.workspaceContext?.workspaceId ?? null },
            requestId: req.auditContext?.requestId,
            ipAddress: req.auditContext?.ipAddress,
            userAgent: req.auditContext?.userAgent
        });

        res.status(200).json(result);
    } catch (error) {
        respondWithError(res, error, 400);
    }
};

export const listAuditLogs = async (req: Request, res: Response): Promise<void> => {
    try {
        const auditMeta = getAuditMeta(req);
        if (req.workspaceContext) {
            await workspaceContextService.assertAuthorized(
                req.workspaceContext,
                'audit:read',
                auditMeta
            );
        }
        const logs = await AuditService.listLogs(req.query as any);
        const scopedLogs =
            req.workspaceContext?.workspaceId
                ? logs.filter((entry: any) => {
                      const details = entry?.details as Record<string, unknown> | null | undefined;
                      return details?.workspaceId === req.workspaceContext?.workspaceId;
                  })
                : logs;
        res.status(200).json(scopedLogs);
    } catch (error) {
        respondWithError(res, error, 500);
    }
};

export const replayPayment = async (req: Request, res: Response): Promise<void> => {
    try {
        const { id } = req.params;
        const auditMeta = getAuditMeta(req);

        if (req.workspaceContext) {
            const verified = await workspaceContextService.assertAuthorized(
                req.workspaceContext,
                'payments:write',
                auditMeta
            );
            await workspaceContextService.assertResourceInWorkspace(
                verified,
                'PAYMENT',
                id,
                auditMeta
            );
        }

        const payment = await prisma.payment.findUnique({ where: { id } });

        if (!payment) {
            res.status(404).json({ error: 'Payment not found' });
            return;
        }

        // Reset retry metadata and set to PENDING
        await prisma.payment.update({
            where: { id },
            data: {
                status: 'PENDING',
                retryCount: 0,
                nextRetryAt: null,
                lastError: null,
                updatedAt: new Date()
            }
        });

        // Log the action with detailed context
        await AuditService.logAction({
            adminId: req.user!.id,
            action: 'REPLAY_PAYMENT',
            targetType: 'PAYMENT',
            targetId: id,
            details: {
                previousStatus: payment.status,
                workspaceId: req.workspaceContext?.workspaceId ?? null
            },
            requestId: req.auditContext?.requestId,
            ipAddress: req.auditContext?.ipAddress,
            userAgent: req.auditContext?.userAgent
        });

        res.status(200).json({ message: 'Payment replay triggered successfully.' });
    } catch (error) {
        respondWithError(res, error, 500);
    }
};

export const getAdminStatus = (req: Request, res: Response): void => {
    res.status(200).json({
        message: 'Admin access granted',
        user: req.user,
        workspaceContext: req.workspaceContext
            ? {
                  workspaceId: req.workspaceContext.workspaceId,
                  role: req.workspaceContext.role,
                  isPlatformAdmin: req.workspaceContext.isPlatformAdmin,
              }
            : null,
    });
};

export const listUsers = async (req: Request, res: Response): Promise<void> => {
    try {
        const filters = {
            role: req.query.role as string | undefined,
            status: req.query.status as any,
            search: req.query.search as string | undefined,
            limit: req.query.limit ? parseInt(req.query.limit as string, 10) : undefined,
            offset: req.query.offset ? parseInt(req.query.offset as string, 10) : undefined,
        };

        const result = await adminService.getUserList(
            filters as any,
            req.workspaceContext,
            getAuditMeta(req)
        );

        res.status(200).json(result);
    } catch (error) {
        respondWithError(res, error, 500);
    }
};

export const banUnbanUser = async (req: Request, res: Response): Promise<void> => {
    try {
        const { id } = req.params;
        const { action, reason, duration } = req.body as { action: 'ban' | 'unban'; reason?: string; duration?: number };
        const auditMeta = getAuditMeta(req);

        if (action === 'ban') {
            if (!reason) {
                res.status(400).json({ error: 'reason is required to ban a user' });
                return;
            }

            await adminService.banUser(
                { userId: id, reason, duration },
                req.workspaceContext,
                auditMeta
            );

            await AuditService.logAction({
                adminId: req.user!.id,
                action: 'BAN_USER',
                targetType: 'USER',
                targetId: id,
                details: {
                    reason,
                    duration,
                    workspaceId: req.workspaceContext?.workspaceId ?? null
                },
                requestId: req.auditContext?.requestId,
                ipAddress: req.auditContext?.ipAddress,
                userAgent: req.auditContext?.userAgent
            });

            res.status(200).json({ message: 'User banned' });
            return;
        }

        if (action === 'unban') {
            await adminService.unbanUser(id, req.workspaceContext, auditMeta);

            await AuditService.logAction({
                adminId: req.user!.id,
                action: 'UNBAN_USER',
                targetType: 'USER',
                targetId: id,
                details: { workspaceId: req.workspaceContext?.workspaceId ?? null },
                requestId: req.auditContext?.requestId,
                ipAddress: req.auditContext?.ipAddress,
                userAgent: req.auditContext?.userAgent
            });

            res.status(200).json({ message: 'User unbanned' });
            return;
        }

        res.status(400).json({ error: 'invalid action' });
    } catch (error) {
        respondWithError(res, error, 400);
    }
};

export const bulkUserOperation = async (req: Request, res: Response): Promise<void> => {
    try {
        const { action, userIds, reason, duration } = req.body as { action: 'ban' | 'unban'; userIds: string[]; reason?: string; duration?: number };
        const auditMeta = getAuditMeta(req);

        if (!Array.isArray(userIds) || userIds.length === 0) {
            res.status(400).json({ error: 'userIds must be a non-empty array' });
            return;
        }

        // Safety: cap max bulk size and require explicit confirm for large operations
        const MAX_BULK = 200
        if (userIds.length > MAX_BULK) {
            res.status(400).json({ error: `bulk operations limited to ${MAX_BULK} users` })
            return
        }

        // For batches over a smaller threshold require a confirmation token
        const CONFIRM_THRESHOLD = 25
        if (userIds.length >= CONFIRM_THRESHOLD) {
            const confirmToken = req.headers['x-confirm-token'] as string || req.body.confirmToken
            const valid = confirmToken && confirmationService.validate(req.user!.id, confirmToken, `bulk:${action}`, { userIds, reason, duration })
            if (!valid) {
                res.status(400).json({ error: 'confirmation required for large bulk operations. Request a token first via POST /api/admin/confirmations' })
                return
            }
        }

        const results = await adminService.executeBulkUserOperation(
            { action, userIds, reason, duration },
            req.workspaceContext,
            auditMeta
        );

        await AuditService.logAction({
            adminId: req.user!.id,
            action: 'BULK_USER_OPERATION',
            targetType: 'USER_BULK',
            targetId: `bulk-${Date.now()}`,
            details: {
                action,
                userIds,
                reason,
                duration,
                results,
                workspaceId: req.workspaceContext?.workspaceId ?? null
            },
            requestId: req.auditContext?.requestId,
            ipAddress: req.auditContext?.ipAddress,
            userAgent: req.auditContext?.userAgent
        });

        res.status(200).json({ results });
    } catch (error) {
        respondWithError(res, error, 500);
    }
};

export const getGameConfig = async (req: Request, res: Response): Promise<void> => {
    try {
        if (req.workspaceContext) {
            await workspaceContextService.assertAuthorized(
                req.workspaceContext,
                'games:read',
                getAuditMeta(req)
            );
        }
        const cfg = await adminService.getGameConfig();
        res.status(200).json(cfg);
    } catch (error) {
        respondWithError(res, error, 500);
    }
};

export const updateGameConfig = async (req: Request, res: Response): Promise<void> => {
    try {
        if (req.workspaceContext) {
            await workspaceContextService.assertAuthorized(
                req.workspaceContext,
                'games:write',
                getAuditMeta(req)
            );
        }
        const updated = await adminService.updateGameConfig(req.body as any);

        await AuditService.logAction({
            adminId: req.user!.id,
            action: 'UPDATE_GAME_CONFIG',
            targetType: 'GAME_CONFIG',
            targetId: 'GAME_CONFIG',
            details: { updated, workspaceId: req.workspaceContext?.workspaceId ?? null },
            requestId: req.auditContext?.requestId,
            ipAddress: req.auditContext?.ipAddress,
            userAgent: req.auditContext?.userAgent
        });

        res.status(200).json(updated);
    } catch (error) {
        respondWithError(res, error, 400);
    }
};

export const getModerationQueue = async (req: Request, res: Response): Promise<void> => {
    try {
        const queue = await adminService.getModerationQueue(
            req.workspaceContext,
            getAuditMeta(req)
        );
        res.status(200).json(queue);
    } catch (error) {
        respondWithError(res, error, 500);
    }
};

export const reviewContent = async (req: Request, res: Response): Promise<void> => {
    try {
        const { contentId, action } = req.body as { contentId: string; action: 'approve' | 'reject' | 'remove' };

        if (!contentId || !action) {
            res.status(400).json({ error: 'contentId and action are required' });
            return;
        }

        await adminService.reviewContent(
            contentId,
            action,
            req.workspaceContext,
            getAuditMeta(req)
        );

        await AuditService.logAction({
            adminId: req.user!.id,
            action: 'REVIEW_CONTENT',
            targetType: 'CONTENT',
            targetId: contentId,
            details: { action, workspaceId: req.workspaceContext?.workspaceId ?? null },
            requestId: req.auditContext?.requestId,
            ipAddress: req.auditContext?.ipAddress,
            userAgent: req.auditContext?.userAgent
        });

        res.status(200).json({ message: 'review processed' });
    } catch (error) {
        respondWithError(res, error, 400);
    }
};

export const getSystemHealth = async (_req: Request, res: Response): Promise<void> => {
    try {
        const health = await adminService.getSystemHealth();
        res.status(200).json(health);
    } catch (error) {
        res.status(500).json({ error: (error as Error).message });
    }
};

export const scheduleMaintenance = async (req: Request, res: Response): Promise<void> => {
    try {
        const { startTime, endTime, message } = req.body;
        if (!startTime || !endTime) {
            res.status(400).json({ error: 'startTime and endTime are required' });
            return;
        }
        MaintenanceService.getInstance().scheduleMaintenance(new Date(startTime), new Date(endTime), message);
        
        await AuditService.logAction({
            adminId: req.user!.id,
            action: 'SCHEDULE_MAINTENANCE',
            targetType: 'SYSTEM',
            targetId: 'MAINTENANCE',
            details: { startTime, endTime, message },
            requestId: req.auditContext?.requestId,
            ipAddress: req.auditContext?.ipAddress,
            userAgent: req.auditContext?.userAgent
        });

        res.status(200).json({ message: 'Maintenance scheduled successfully' });
    } catch (error) {
        res.status(500).json({ error: (error as Error).message });
    }
};

export const enableMaintenanceImmediately = async (req: Request, res: Response): Promise<void> => {
    try {
        const { endTime, message } = req.body;
        if (!endTime) {
            res.status(400).json({ error: 'endTime is required' });
            return;
        }
        MaintenanceService.getInstance().enableMaintenanceImmediately(new Date(endTime), message);

        await AuditService.logAction({
            adminId: req.user!.id,
            action: 'ENABLE_MAINTENANCE_IMMEDIATELY',
            targetType: 'SYSTEM',
            targetId: 'MAINTENANCE',
            details: { endTime, message },
            requestId: req.auditContext?.requestId,
            ipAddress: req.auditContext?.ipAddress,
            userAgent: req.auditContext?.userAgent
        });

        res.status(200).json({ message: 'Maintenance enabled immediately' });
    } catch (error) {
        res.status(500).json({ error: (error as Error).message });
    }
};

export const disableMaintenance = async (req: Request, res: Response): Promise<void> => {
    try {
        MaintenanceService.getInstance().disableMaintenance();

        await AuditService.logAction({
            adminId: req.user!.id,
            action: 'DISABLE_MAINTENANCE',
            targetType: 'SYSTEM',
            targetId: 'MAINTENANCE',
            details: {},
            requestId: req.auditContext?.requestId,
            ipAddress: req.auditContext?.ipAddress,
            userAgent: req.auditContext?.userAgent
        });

        res.status(200).json({ message: 'Maintenance disabled successfully' });
    } catch (error) {
        res.status(500).json({ error: (error as Error).message });
    }
};

export const getMaintenanceStatus = (req: Request, res: Response): void => {
    res.status(200).json(MaintenanceService.getInstance().getStatus());
};
