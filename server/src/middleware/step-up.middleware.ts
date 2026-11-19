import bcrypt from 'bcrypt';
import crypto from 'crypto';
import { Request, Response, NextFunction } from 'express';
import Redis from 'ioredis';
import { getEnv } from '../config/env';
import { getDatabaseClient } from '../services/database.service';
import { logger } from '../services/logger.service';
import { HttpError } from '../utils/http-error';

const PROOF_TTL_SECONDS = 120;
const STEP_UP_ACTIONS = [
    'wallet.withdraw',
    'wallet.transfer',
    'wallet.escrow.lock',
    'wallet.escrow.release',
    'wallet.escrow.slash',
    'wallet.encryption.rotate',
    'api_key.create',
    'api_key.revoke',
    'api_key.rotate'
] as const;

export type StepUpAction = (typeof STEP_UP_ACTIONS)[number];

interface StepUpClaims {
    actorId: string;
    action: StepUpAction;
    operationHash: string;
    sessionHash: string;
    expiresAt: number;
    id: string;
}

interface StepUpOperation {
    method: string;
    path: string;
    query?: Record<string, unknown>;
    body: Record<string, unknown>;
}

let redis: Redis | undefined;

const getRedis = (): Redis => {
    const url = getEnv().REDIS_URL;
    if (!url) {
        throw new HttpError(503, 'Step-up verification is unavailable');
    }

    if (!redis) {
        redis = new Redis(url, {
            enableOfflineQueue: false,
            lazyConnect: true,
            maxRetriesPerRequest: 1
        });
        redis.on('error', (error) => {
            logger.warn('Step-up Redis client error', { error: error.message });
        });
    }
    return redis;
};

const canonicalize = (value: unknown): string => {
    if (Array.isArray(value)) {
        return `[${value.map(canonicalize).join(',')}]`;
    }
    if (value !== null && typeof value === 'object') {
        const entries = Object.entries(value as Record<string, unknown>)
            .sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0);
        return `{${entries.map(([key, item]) => `${JSON.stringify(key)}:${canonicalize(item)}`).join(',')}}`;
    }
    return JSON.stringify(value) ?? 'null';
};

export const operationFingerprint = (action: StepUpAction, operation: unknown): string =>
    crypto.createHash('sha256')
        .update(`${action}\n${canonicalize(operation)}`)
        .digest('hex');

const isAllowedTarget = (action: StepUpAction, method: string, path: string): boolean => {
    const target = path.toLowerCase();
    switch (action) {
        case 'wallet.transfer':
            return method === 'POST' && target.endsWith('/transfer');
        case 'wallet.withdraw':
            return method === 'POST' && target.endsWith('/api/wallet/withdraw');
        case 'wallet.escrow.lock':
            return method === 'POST' && target.endsWith('/escrow/lock');
        case 'wallet.escrow.release':
            return method === 'POST' && target.endsWith('/escrow/release');
        case 'wallet.escrow.slash':
            return method === 'POST' && target.endsWith('/escrow/slash');
        case 'wallet.encryption.rotate':
            return method === 'POST' &&
                (target.endsWith('/me/rotate-encryption') || target.endsWith('/rotate-encryption'));
        case 'api_key.create':
            return method === 'POST' && target.endsWith('/keys');
        case 'api_key.revoke':
            return method === 'DELETE' && /\/keys\/[0-9a-f-]+$/.test(target);
        case 'api_key.rotate':
            return method === 'POST' && /\/keys\/[0-9a-f-]+\/rotate$/.test(target);
    }
};

const requestOperation = (req: Request): StepUpOperation => {
    const body = { ...(req.body ?? {}) };
    delete body.stepUpProof;
    return {
        method: req.method.toUpperCase(),
        path: req.originalUrl.split('?')[0],
        query: req.query as Record<string, unknown>,
        body
    };
};

const sessionFingerprint = (req: Request): string => {
    const authorization = req.header('authorization') ?? '';
    const token = authorization.startsWith('Bearer ') ? authorization.slice(7) : '';
    if (!token) {
        throw new HttpError(401, 'Authentication required');
    }
    return crypto.createHash('sha256').update(token).digest('hex');
};

const sign = (encodedClaims: string): string =>
    crypto.createHmac('sha256', getEnv().SENSITIVE_OPERATION_SECRET)
        .update(encodedClaims)
        .digest('base64url');

const safeEqual = (left: string, right: string): boolean => {
    const leftBytes = Buffer.from(left);
    const rightBytes = Buffer.from(right);
    return leftBytes.length === rightBytes.length && crypto.timingSafeEqual(leftBytes, rightBytes);
};

const parseProof = (proof: string): StepUpClaims => {
    const [encoded, signature, extra] = proof.split('.');
    if (!encoded || !signature || extra) {
        throw new HttpError(401, 'Invalid step-up proof');
    }
    if (!safeEqual(signature, sign(encoded))) {
        throw new HttpError(401, 'Invalid step-up proof');
    }

    try {
        return JSON.parse(Buffer.from(encoded, 'base64url').toString('utf8')) as StepUpClaims;
    } catch {
        throw new HttpError(401, 'Invalid step-up proof');
    }
};

export const issueStepUpProof = async (
    req: Request,
    res: Response,
    next: NextFunction
): Promise<void> => {
    try {
        const { action, operation, currentPassword } = req.body ?? {};
        if (!STEP_UP_ACTIONS.includes(action) || !operation || typeof operation !== 'object' ||
            Array.isArray(operation) || typeof currentPassword !== 'string' || !req.user ||
            typeof operation.method !== 'string' || typeof operation.path !== 'string' ||
            !operation.body || typeof operation.body !== 'object' ||
            !isAllowedTarget(action, operation.method.toUpperCase(), operation.path)) {
            throw new HttpError(400, 'A supported action, operation, and current password are required');
        }

        const user = await getDatabaseClient().user.findUnique({
            where: { id: req.user.id },
            select: { passwordHash: true }
        });
        if (!user?.passwordHash) {
            throw new HttpError(403, 'Set or recover an account password before authorizing sensitive actions');
        }
        if (!await bcrypt.compare(currentPassword, user.passwordHash)) {
            throw new HttpError(401, 'Re-authentication failed');
        }

        const normalizedOperation: StepUpOperation = {
            method: operation.method.toUpperCase(),
            path: operation.path,
            query: operation.query ?? {},
            body: operation.body
        };
        const claims: StepUpClaims = {
            actorId: req.user.id,
            action,
            operationHash: operationFingerprint(action, normalizedOperation),
            sessionHash: sessionFingerprint(req),
            expiresAt: Math.floor(Date.now() / 1000) + PROOF_TTL_SECONDS,
            id: crypto.randomUUID()
        };
        logger.info('Step-up proof issued', {
            actorId: req.user.id,
            action,
            expiresAt: claims.expiresAt
        });
        const encoded = Buffer.from(JSON.stringify(claims)).toString('base64url');
        res.status(200).json({
            proof: `${encoded}.${sign(encoded)}`,
            expiresAt: claims.expiresAt
        });
    } catch (error) {
        next(error);
    }
};

export const requireStepUp = (action: StepUpAction) =>
    async (req: Request, _res: Response, next: NextFunction): Promise<void> => {
        try {
            const proof = req.body?.stepUpProof;
            if (typeof proof !== 'string' || !req.user) {
                throw new HttpError(401, 'A recent step-up proof is required');
            }

            const claims = parseProof(proof);
            if (claims.actorId !== req.user.id || claims.action !== action ||
                claims.operationHash !== operationFingerprint(action, requestOperation(req)) ||
                claims.sessionHash !== sessionFingerprint(req) ||
                claims.expiresAt <= Math.floor(Date.now() / 1000)) {
                throw new HttpError(401, 'Step-up proof is expired or does not match this action');
            }

            const consumed = await getRedis().set(
                `step-up:used:${claims.id}`,
                req.user.id,
                'EX',
                Math.max(1, claims.expiresAt - Math.floor(Date.now() / 1000)),
                'NX'
            );
            if (consumed !== 'OK') {
                throw new HttpError(401, 'Step-up proof has already been used');
            }

            delete req.body.stepUpProof;
            logger.info('Step-up proof consumed', {
                actorId: req.user.id,
                action,
                requestId: req.requestId
            });
            next();
        } catch (error) {
            next(error);
        }
    };
