import { Router } from 'express';
import { authRateLimiter } from '../middleware/rate-limit.middleware';
import { authenticateJWT } from '../middleware/auth.middleware';
import { issueStepUpProof } from '../middleware/step-up.middleware';
import * as authController from '../controllers/auth.controller';

const router: Router = Router();

router.post('/register', authRateLimiter, authController.register);
router.post('/login', authRateLimiter, authController.login);
router.post('/refresh', authRateLimiter, authController.refresh);
router.post('/logout', authRateLimiter, authController.logout);
router.post('/social/:provider', authRateLimiter, authController.socialAuth);
router.post('/guest', authRateLimiter, authController.guestSession);
router.post('/verify-email', authRateLimiter, authController.verifyEmailHandler);
router.post('/resend-verification-email', authRateLimiter, authController.resendVerificationEmailHandler);
router.post('/forgot-password', authRateLimiter, authController.forgotPasswordHandler);
router.post('/reset-password', authRateLimiter, authController.resetPasswordHandler);
router.post('/step-up', authRateLimiter, authenticateJWT, issueStepUpProof);

// Safe OAuth identity linking, unlinking, revocation & audited recovery (#172)
router.post('/oauth/link/challenge', authRateLimiter, authenticateJWT, authController.initiateOAuthLinkHandler);
router.post('/oauth/link', authRateLimiter, authenticateJWT, authController.completeOAuthLinkHandler);
router.get('/oauth/identities', authRateLimiter, authenticateJWT, authController.listOAuthIdentitiesHandler);
router.delete('/oauth/identities/:provider', authRateLimiter, authenticateJWT, authController.unlinkOAuthIdentityHandler);
router.post('/oauth/revoke', authRateLimiter, authenticateJWT, authController.revokeOAuthIdentityHandler);
router.post('/oauth/recovery/initiate', authRateLimiter, authController.initiateOAuthRecoveryHandler);
router.post('/oauth/recovery/complete', authRateLimiter, authController.completeOAuthRecoveryHandler);

export default router;
