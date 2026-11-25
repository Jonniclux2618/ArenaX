import { Request, Response } from 'express';
import refundService from '../services/refund.service';
import { isBaseError } from '../errors/base-error';
import { workspaceContextService } from '../services/workspace-context.service';

const handleRefundError = (res: Response, error: unknown, fallbackStatus: number): void => {
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

export const listRefundRequests = async (req: Request, res: Response): Promise<void> => {
    try {
        let allowedIds: string[] | null = null;
        if (req.workspaceContext) {
            const verified = await workspaceContextService.assertAuthorized(
                req.workspaceContext,
                'refunds:read',
                {
                    requestId: req.auditContext?.requestId ?? req.requestId,
                    ipAddress: req.auditContext?.ipAddress ?? req.ip,
                    userAgent: req.auditContext?.userAgent,
                }
            );
            allowedIds = await workspaceContextService.listWorkspaceResourceIds(
                verified,
                'REFUND_REQUEST'
            );
        }
        const requests = await refundService.listRequests(req.query);
        const scoped =
            allowedIds !== null
                ? requests.filter((r: { id: string }) => allowedIds!.includes(r.id))
                : requests;
        res.status(200).json(scoped);
    } catch (error) {
        handleRefundError(res, error, 500);
    }
};

export const updateRefundStatus = async (req: Request, res: Response): Promise<void> => {
    try {
        const { id } = req.params;
        const { status, operatorNotes } = req.body;
        
        if (!status) {
            res.status(400).json({ error: 'Status is required.' });
            return;
        }

        if (req.workspaceContext) {
            const auditMeta = {
                requestId: req.auditContext?.requestId ?? req.requestId,
                ipAddress: req.auditContext?.ipAddress ?? req.ip,
                userAgent: req.auditContext?.userAgent,
            };
            const verified = await workspaceContextService.assertAuthorized(
                req.workspaceContext,
                'refunds:write',
                auditMeta
            );
            await workspaceContextService.assertResourceInWorkspace(
                verified,
                'REFUND_REQUEST',
                id,
                auditMeta
            );
        }

        const updatedRequest = await refundService.updateStatus(
            id,
            req.user!.id,
            status,
            operatorNotes
        );

        res.status(200).json(updatedRequest);
    } catch (error) {
        handleRefundError(res, error, 400);
    }
};

export const createRefundRequest = async (req: Request, res: Response): Promise<void> => {
    try {
        const { paymentId, amount, reason } = req.body;
        
        if (!paymentId || !amount || !reason) {
            res.status(400).json({ error: 'paymentId, amount, and reason are required.' });
            return;
        }

        if (req.workspaceContext) {
            const auditMeta = {
                requestId: req.auditContext?.requestId ?? req.requestId,
                ipAddress: req.auditContext?.ipAddress ?? req.ip,
                userAgent: req.auditContext?.userAgent,
            };
            const verified = await workspaceContextService.assertAuthorized(
                req.workspaceContext,
                'refunds:write',
                auditMeta
            );
            await workspaceContextService.assertResourceInWorkspace(
                verified,
                'PAYMENT',
                paymentId,
                auditMeta
            );
        }

        const request = await refundService.createRequest({
            paymentId,
            amount,
            reason
        });

        if (req.workspaceContext?.workspaceId && request?.id) {
            await workspaceContextService.registerResource(
                req.workspaceContext.workspaceId,
                'REFUND_REQUEST',
                request.id
            );
        }

        res.status(201).json(request);
    } catch (error) {
        handleRefundError(res, error, 400);
    }
};
