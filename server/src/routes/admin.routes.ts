import { Router } from 'express';
import {
    getAdminStatus,
    listDisputes,
    resolveDispute,
    listAuditLogs,
    replayPayment,
    listUsers,
    banUnbanUser,
    bulkUserOperation,
    getGameConfig,
    updateGameConfig,
    getModerationQueue,
    reviewContent,
    getSystemHealth,
    scheduleMaintenance,
    enableMaintenanceImmediately,
    disableMaintenance,
    getMaintenanceStatus
} from '../controllers/admin.controller';
import {
    searchAuditLogs,
    exportAuditLogs,
    verifyAuditIntegrity,
    replayAuditEvents,
    auditComplianceReport,
    redactUserAuditData,
    anchorAuditLogs,
} from '../controllers/audit.controller';
import { confirmationService } from '../services/confirmation.service';
import {
    listKycReviews,
    getKycReview,
    processKycReview
} from '../controllers/kyc.controller';
import { 
    listRefundRequests, 
    updateRefundStatus, 
    createRefundRequest 
} from '../controllers/refund.controller';
import { authenticateJWT, restrictTo, restrictToScope } from '../middleware/auth.middleware';
import requireConfirmationForBulk from '../middleware/confirm.middleware';
import { adminRateLimiter } from '../middleware/rate-limit.middleware';
import {
    propagateWorkspaceContext,
    requireWorkspacePermission
} from '../middleware/workspace-context.middleware';

const router: Router = Router();

router.use(adminRateLimiter, authenticateJWT, propagateWorkspaceContext());

router.get('/status', requireWorkspacePermission('workspace:read'), getAdminStatus);
// Request a confirmation token for destructive/bulk actions
router.post('/confirmations', requireWorkspacePermission('users:write'), (req, res) => {
    const { action, payload, ttlMs } = req.body as any
    if (!action) return res.status(400).json({ error: 'action is required' })
    const info = confirmationService.generate(req.user!.id, action, payload, ttlMs)
    res.status(200).json({ token: info.token, expiresAt: info.expiresAt })
})
// User management
router.get('/users', requireWorkspacePermission('users:read'), listUsers);
router.post('/users/bulk', requireWorkspacePermission('users:write'), requireConfirmationForBulk(10), bulkUserOperation);
router.post('/users/:id/ban', requireWorkspacePermission('users:write'), banUnbanUser);

// Game configuration
router.get('/games/config', requireWorkspacePermission('games:read'), getGameConfig);
router.put('/games/config', requireWorkspacePermission('games:write'), updateGameConfig);

// Moderation
router.get('/moderation/queue', requireWorkspacePermission('moderation:read'), getModerationQueue);
router.post('/moderation/review', requireWorkspacePermission('moderation:write'), reviewContent);

// System
router.get('/system/health', restrictTo('ADMIN'), getSystemHealth);
router.get('/disputes', requireWorkspacePermission('disputes:read'), listDisputes);
router.post('/disputes/:id/resolve', requireWorkspacePermission('disputes:write'), resolveDispute);

// ── Audit Trail ──────────────────────────────────────────────────────────────
router.get('/audit-logs', requireWorkspacePermission('audit:read'), listAuditLogs);               // backward-compat
router.get('/audit/search', restrictTo('ADMIN'), searchAuditLogs);           // advanced search
router.get('/audit/export', restrictTo('ADMIN'), exportAuditLogs);           // CSV / JSON export
router.get('/audit/verify', restrictTo('ADMIN'), verifyAuditIntegrity);      // chain integrity check
router.get('/audit/replay', restrictTo('ADMIN'), replayAuditEvents);         // event replay
router.get('/audit/compliance-report', restrictTo('ADMIN'), auditComplianceReport);
router.post('/audit/anchor', restrictTo('ADMIN'), anchorAuditLogs);          // blockchain anchoring
router.post('/audit/redact/:userId', restrictTo('ADMIN'), redactUserAuditData); // GDPR right-to-forget

router.post('/payments/:id/replay', requireWorkspacePermission('payments:write'), replayPayment);

// KYC Management (platform-level)
router.get('/kyc', restrictTo('ADMIN'), listKycReviews);
router.get('/kyc/:id', restrictTo('ADMIN'), getKycReview);
router.post('/kyc/:id/process', restrictTo('ADMIN'), processKycReview);

// Refund Management
router.get('/refunds', requireWorkspacePermission('refunds:read'), listRefundRequests);
router.patch('/refunds/:id/status', requireWorkspacePermission('refunds:write'), updateRefundStatus);
router.post('/refunds', requireWorkspacePermission('refunds:write'), createRefundRequest);

// Maintenance Management (platform-level)
router.post('/maintenance/schedule', restrictTo('ADMIN'), restrictToScope('SYSTEM:WRITE'), scheduleMaintenance);
router.post('/maintenance/enable', restrictTo('ADMIN'), restrictToScope('SYSTEM:WRITE'), enableMaintenanceImmediately);
router.post('/maintenance/disable', restrictTo('ADMIN'), restrictToScope('SYSTEM:WRITE'), disableMaintenance);
router.get('/maintenance/status', restrictTo('ADMIN'), getMaintenanceStatus);

export default router;
