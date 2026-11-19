import { Router } from 'express';
import rateLimit from 'express-rate-limit';
import {
    createRecoveryChallenge,
    exportMyWallet,
    getLedger,
    getMyWallet,
    getTransactions,
    internalTransfer,
    lockEscrow,
    releaseEscrow,
    rotateWalletEncryption,
    slashEscrow
} from '../controllers/wallet.controller';
import { authenticateJWT, restrictTo } from '../middleware/auth.middleware';
import { paymentRateLimiter, publicRateLimiter } from '../middleware/rate-limit.middleware';
import { requireStepUp } from '../middleware/step-up.middleware';

const router: Router = Router();

const walletRecoveryLimiter = rateLimit({
    windowMs: 15 * 60 * 1000,
    max: process.env.NODE_ENV === 'test' ? 1000 : 5,
    standardHeaders: true,
    legacyHeaders: false,
    message: {
        error: {
            code: 'WALLET_RECOVERY_RATE_LIMIT',
            message: 'Too many wallet recovery attempts. Please try again later.'
        }
    }
});

router.use(authenticateJWT);

router.get('/me', publicRateLimiter, getMyWallet);
router.post('/me/recovery/challenges', walletRecoveryLimiter, createRecoveryChallenge);
router.post('/me/recovery/export', walletRecoveryLimiter, exportMyWallet);
router.post('/me/rotate-encryption', paymentRateLimiter, requireStepUp('wallet.encryption.rotate'), rotateWalletEncryption);
router.post('/rotate-encryption', restrictTo('ADMIN'), paymentRateLimiter, requireStepUp('wallet.encryption.rotate'), rotateWalletEncryption);

router.get('/ledger', publicRateLimiter, getLedger);
router.get('/transactions', publicRateLimiter, getTransactions);
router.post('/escrow/lock', paymentRateLimiter, requireStepUp('wallet.escrow.lock'), lockEscrow);
router.post('/escrow/release', paymentRateLimiter, requireStepUp('wallet.escrow.release'), releaseEscrow);
router.post('/escrow/slash', paymentRateLimiter, requireStepUp('wallet.escrow.slash'), slashEscrow);
router.post('/transfer', paymentRateLimiter, requireStepUp('wallet.transfer'), internalTransfer);

export default router;
