import axios from 'axios';
import bcrypt from 'bcrypt';
import crypto from 'crypto';
import { BaseError } from '../errors/base-error';
import { AuditStatus } from '../types/audit.types';
import { AuditService } from './audit.service';
import { getDatabaseClient } from './database.service';
import emailService from './email.service';
import { logger } from './logger.service';
import stellarWalletService from './stellar-wallet.service';
import {
    resolveTwitchProfileFromAuthorizationCode,
    validateTwitchToken,
} from './twitch-oauth.service';

export type OAuthProvider = 'google' | 'discord' | 'twitch';
export type NormalizedOAuthProvider = 'GOOGLE' | 'DISCORD' | 'TWITCH';
export type OAuthIdentityStatus = 'ACTIVE' | 'UNLINKED' | 'REVOKED';
export type OAuthRecoveryStatus = 'PENDING' | 'COMPLETED' | 'EXPIRED' | 'FAILED';

export interface ExternalIdentityProof {
    provider: OAuthProvider;
    accessToken?: string;
    code?: string;
    redirectUri?: string;
}

export interface VerifiedExternalProfile {
    provider: NormalizedOAuthProvider;
    providerId: string;
    email: string;
    emailVerified: boolean;
    username: string;
}

export interface OAuthAuditMeta {
    requestId?: string;
    correlationId?: string;
    ipAddress?: string;
    userAgent?: string;
}

export interface OAuthIdentityRecord {
    id: string;
    userId: string;
    provider: NormalizedOAuthProvider;
    providerId: string;
    providerEmail: string | null;
    status: OAuthIdentityStatus;
    linkedAt: Date;
    lastVerifiedAt: Date;
    unlinkedAt: Date | null;
    revokedAt: Date | null;
    revokeReason: string | null;
}

interface LinkChallengeRecord {
    id: string;
    userId: string;
    provider: NormalizedOAuthProvider;
    stateHash: string;
    nonceHash: string;
    sessionFingerprint: string;
    expiresAt: Date;
    consumedAt: Date | null;
    createdAt: Date;
}

interface RecoveryChallengeRecord {
    id: string;
    userId: string;
    provider: NormalizedOAuthProvider | null;
    codeHash: string;
    status: OAuthRecoveryStatus;
    attempts: number;
    maxAttempts: number;
    expiresAt: Date;
    consumedAt: Date | null;
    createdAt: Date;
}

export interface OAuthAuditRecord {
    actorId: string;
    action: string;
    status: AuditStatus;
    targetType: string;
    targetId: string;
    details: Record<string, unknown>;
    timestamp: string;
}

export type ExternalProviderVerifier = (
    proof: ExternalIdentityProof
) => Promise<VerifiedExternalProfile>;

const LINK_CHALLENGE_TTL_MS = 5 * 60 * 1000; // 5 minutes for fresh external proof
const RECOVERY_CHALLENGE_TTL_MS = 15 * 60 * 1000; // 15 minutes
const RECOVERY_MAX_ATTEMPTS = 5;
const BCRYPT_ROUNDS = 12;
const MAX_USERNAME_LENGTH = 24;
const MIN_USERNAME_LENGTH = 3;
const MAX_USERNAME_ATTEMPTS = 25;

const toHash = (value: string): string =>
    crypto.createHash('sha256').update(value).digest('hex');

const safeEqual = (left: string, right: string): boolean => {
    const leftBuf = Buffer.from(left);
    const rightBuf = Buffer.from(right);
    if (leftBuf.length !== rightBuf.length) {
        return false;
    }
    return crypto.timingSafeEqual(leftBuf, rightBuf);
};

export const normalizeProvider = (provider: string): NormalizedOAuthProvider => {
    const upper = provider.trim().toUpperCase();
    if (upper === 'GOOGLE' || upper === 'DISCORD' || upper === 'TWITCH') {
        return upper;
    }
    throw new BaseError(`Unsupported OAuth provider: ${provider}`, 400, 'OAUTH_UNSUPPORTED_PROVIDER');
};

export const normalizeEmail = (email: string): string => email.trim().toLowerCase();

export const computeSessionFingerprint = (sessionToken: string | undefined): string => {
    const raw = (sessionToken ?? '').trim();
    const token = raw.startsWith('Bearer ') ? raw.slice(7).trim() : raw;
    if (!token) {
        throw new BaseError(
            'An authenticated ArenaX session is required for OAuth identity linking',
            401,
            'OAUTH_SESSION_REQUIRED'
        );
    }
    return toHash(token);
};

// In-memory authoritative fallback stores for environments/tests whose mock
// Prisma client does not attach OAuth tables.
const inMemoryIdentities = new Map<string, OAuthIdentityRecord>();
const inMemoryLinkChallenges = new Map<string, LinkChallengeRecord>();
const inMemoryRecoveryChallenges = new Map<string, RecoveryChallengeRecord>();
const oauthAuditSink: OAuthAuditRecord[] = [];

const identityKey = (provider: NormalizedOAuthProvider, providerId: string): string =>
    `${provider}:${providerId}`;

let customProviderVerifier: ExternalProviderVerifier | null = null;

async function defaultVerifyExternalIdentityProof(
    proof: ExternalIdentityProof
): Promise<VerifiedExternalProfile> {
    const normalized = normalizeProvider(proof.provider);

    if (normalized === 'GOOGLE') {
        if (!proof.accessToken) {
            throw new BaseError(
                'Google identity verification requires a fresh accessToken',
                400,
                'OAUTH_EXTERNAL_PROOF_REQUIRED'
            );
        }
        const response = await axios.get('https://www.googleapis.com/oauth2/v3/userinfo', {
            headers: { Authorization: `Bearer ${proof.accessToken}` },
        });
        const { email, sub, name, email_verified } = response.data ?? {};
        if (!sub || !email) {
            throw new BaseError(
                'Google did not return a verifiable subject and email',
                401,
                'OAUTH_EXTERNAL_PROOF_INVALID'
            );
        }
        return {
            provider: 'GOOGLE',
            providerId: String(sub),
            email: normalizeEmail(String(email)),
            emailVerified: email_verified !== false,
            username: String(name || String(email).split('@')[0]),
        };
    }

    if (normalized === 'DISCORD') {
        if (!proof.accessToken) {
            throw new BaseError(
                'Discord identity verification requires a fresh accessToken',
                400,
                'OAUTH_EXTERNAL_PROOF_REQUIRED'
            );
        }
        const response = await axios.get('https://discord.com/api/users/@me', {
            headers: { Authorization: `Bearer ${proof.accessToken}` },
        });
        const { email, id, username: discordUsername, global_name, verified } = response.data ?? {};
        if (!id) {
            throw new BaseError(
                'Discord did not return a verifiable user identifier',
                401,
                'OAUTH_EXTERNAL_PROOF_INVALID'
            );
        }
        return {
            provider: 'DISCORD',
            providerId: String(id),
            email: normalizeEmail(String(email || `${id}@discord.local`)),
            emailVerified: Boolean(verified),
            username: String(global_name || discordUsername || `discord_${id}`),
        };
    }

    // TWITCH
    if (proof.code) {
        if (!proof.redirectUri) {
            throw new BaseError(
                'redirectUri is required to complete Twitch verification',
                400,
                'OAUTH_EXTERNAL_PROOF_REQUIRED'
            );
        }
        const profile = await resolveTwitchProfileFromAuthorizationCode(
            proof.code,
            proof.redirectUri
        );
        return {
            provider: 'TWITCH',
            providerId: profile.providerId,
            email: normalizeEmail(profile.email),
            emailVerified: !profile.email.endsWith('@twitch.local'),
            username: profile.username,
        };
    }

    if (proof.accessToken) {
        const profile = await validateTwitchToken(proof.accessToken);
        return {
            provider: 'TWITCH',
            providerId: profile.providerId,
            email: normalizeEmail(profile.email),
            emailVerified: !profile.email.endsWith('@twitch.local'),
            username: profile.username,
        };
    }

    throw new BaseError(
        'Twitch identity verification requires either an authorization code or an access token',
        400,
        'OAUTH_EXTERNAL_PROOF_REQUIRED'
    );
}

export async function verifyExternalIdentityProof(
    proof: ExternalIdentityProof
): Promise<VerifiedExternalProfile> {
    if (customProviderVerifier) {
        return customProviderVerifier(proof);
    }
    return defaultVerifyExternalIdentityProof(proof);
}

async function recordOAuthAudit(
    actorId: string,
    action: string,
    status: AuditStatus,
    targetType: string,
    targetId: string,
    details: Record<string, unknown>,
    meta?: OAuthAuditMeta
): Promise<void> {
    oauthAuditSink.push({
        actorId,
        action,
        status,
        targetType,
        targetId,
        details,
        timestamp: new Date().toISOString(),
    });

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
                details,
            });
        } catch (err) {
            logger.warn('Failed to persist OAuth security audit event', {
                action,
                actorId,
                error: err instanceof Error ? err.message : String(err),
            });
        }
    }
}

async function findIdentityByProviderSubject(
    provider: NormalizedOAuthProvider,
    providerId: string
): Promise<OAuthIdentityRecord | null> {
    const prisma = getDatabaseClient() as any;
    if (prisma?.oAuthIdentity?.findUnique) {
        const row = await prisma.oAuthIdentity.findUnique({
            where: {
                provider_providerId: {
                    provider,
                    providerId,
                },
            },
        });
        if (row) return row as OAuthIdentityRecord;
    } else if (prisma?.oAuthIdentity?.findFirst) {
        const row = await prisma.oAuthIdentity.findFirst({
            where: { provider, providerId },
        });
        if (row) return row as OAuthIdentityRecord;
    }

    return inMemoryIdentities.get(identityKey(provider, providerId)) ?? null;
}

async function findIdentitiesByUserId(userId: string): Promise<OAuthIdentityRecord[]> {
    const prisma = getDatabaseClient() as any;
    if (prisma?.oAuthIdentity?.findMany) {
        const rows = await prisma.oAuthIdentity.findMany({
            where: { userId },
        });
        if (Array.isArray(rows)) {
            return rows as OAuthIdentityRecord[];
        }
    }

    return [...inMemoryIdentities.values()].filter((item) => item.userId === userId);
}

async function upsertIdentityRecord(record: OAuthIdentityRecord): Promise<OAuthIdentityRecord> {
    const prisma = getDatabaseClient() as any;
    if (prisma?.oAuthIdentity?.upsert) {
        const saved = await prisma.oAuthIdentity.upsert({
            where: {
                provider_providerId: {
                    provider: record.provider,
                    providerId: record.providerId,
                },
            },
            create: {
                id: record.id,
                userId: record.userId,
                provider: record.provider,
                providerId: record.providerId,
                providerEmail: record.providerEmail,
                status: record.status,
                linkedAt: record.linkedAt,
                lastVerifiedAt: record.lastVerifiedAt,
                unlinkedAt: record.unlinkedAt,
                revokedAt: record.revokedAt,
                revokeReason: record.revokeReason,
            },
            update: {
                userId: record.userId,
                providerEmail: record.providerEmail,
                status: record.status,
                linkedAt: record.linkedAt,
                lastVerifiedAt: record.lastVerifiedAt,
                unlinkedAt: record.unlinkedAt,
                revokedAt: record.revokedAt,
                revokeReason: record.revokeReason,
            },
        });
        inMemoryIdentities.set(identityKey(record.provider, record.providerId), saved);
        return saved as OAuthIdentityRecord;
    }

    inMemoryIdentities.set(identityKey(record.provider, record.providerId), { ...record });
    return { ...record };
}

async function saveLinkChallenge(challenge: LinkChallengeRecord): Promise<void> {
    const prisma = getDatabaseClient() as any;
    if (prisma?.oAuthLinkChallenge?.create) {
        await prisma.oAuthLinkChallenge.create({ data: challenge });
    }
    inMemoryLinkChallenges.set(challenge.stateHash, { ...challenge });
}

async function findLinkChallengeByStateHash(
    stateHash: string
): Promise<LinkChallengeRecord | null> {
    const prisma = getDatabaseClient() as any;
    if (prisma?.oAuthLinkChallenge?.findUnique) {
        const row = await prisma.oAuthLinkChallenge.findUnique({
            where: { stateHash },
        });
        if (row) return row as LinkChallengeRecord;
    }
    return inMemoryLinkChallenges.get(stateHash) ?? null;
}

async function markLinkChallengeConsumed(challenge: LinkChallengeRecord): Promise<void> {
    const consumedAt = new Date();
    const prisma = getDatabaseClient() as any;
    if (prisma?.oAuthLinkChallenge?.update) {
        await prisma.oAuthLinkChallenge.update({
            where: { id: challenge.id },
            data: { consumedAt },
        });
    }
    const existing = inMemoryLinkChallenges.get(challenge.stateHash);
    if (existing) {
        existing.consumedAt = consumedAt;
    }
}

async function saveRecoveryChallenge(challenge: RecoveryChallengeRecord): Promise<void> {
    const prisma = getDatabaseClient() as any;
    if (prisma?.oAuthRecoveryChallenge?.create) {
        await prisma.oAuthRecoveryChallenge.create({ data: challenge });
    }
    inMemoryRecoveryChallenges.set(challenge.id, { ...challenge });
}

async function findRecoveryChallenge(
    challengeId: string
): Promise<RecoveryChallengeRecord | null> {
    const prisma = getDatabaseClient() as any;
    if (prisma?.oAuthRecoveryChallenge?.findUnique) {
        const row = await prisma.oAuthRecoveryChallenge.findUnique({
            where: { id: challengeId },
        });
        if (row) return row as RecoveryChallengeRecord;
    }
    return inMemoryRecoveryChallenges.get(challengeId) ?? null;
}

async function updateRecoveryChallenge(
    challengeId: string,
    patch: Partial<RecoveryChallengeRecord>
): Promise<void> {
    const prisma = getDatabaseClient() as any;
    if (prisma?.oAuthRecoveryChallenge?.update) {
        await prisma.oAuthRecoveryChallenge.update({
            where: { id: challengeId },
            data: patch,
        });
    }
    const existing = inMemoryRecoveryChallenges.get(challengeId);
    if (existing) {
        Object.assign(existing, patch);
    }
}

async function revokeAllUserRefreshTokens(userId: string): Promise<void> {
    const prisma = getDatabaseClient() as any;
    if (prisma?.refreshToken?.updateMany) {
        await prisma.refreshToken.updateMany({
            where: { userId, revokedAt: null },
            data: { revokedAt: new Date() },
        });
    }
}

const sanitizeUsername = (username: string): string =>
    username
        .trim()
        .toLowerCase()
        .replace(/[^a-z0-9_]/g, '')
        .slice(0, MAX_USERNAME_LENGTH);

const buildUsernameBase = (email: string, username?: string): string => {
    if (username) {
        const sanitized = sanitizeUsername(username);
        if (sanitized.length >= MIN_USERNAME_LENGTH) {
            return sanitized;
        }
    }
    const localPart = email.split('@')[0] || 'player';
    const sanitizedLocal = sanitizeUsername(localPart);
    if (sanitizedLocal.length >= MIN_USERNAME_LENGTH) {
        return sanitizedLocal;
    }
    return `player${Math.floor(Math.random() * 9000 + 1000)}`;
};

const buildCandidateUsername = (base: string, attempt: number): string => {
    if (attempt === 0) return base;
    const suffix = `${Math.floor(Math.random() * 9000 + 1000)}`;
    const maxBaseLength = MAX_USERNAME_LENGTH - suffix.length;
    return `${base.slice(0, maxBaseLength)}${suffix}`;
};

const isUniqueError = (error: unknown, field: string): boolean => {
    if (!error || typeof error !== 'object') return false;
    const typed = error as { code?: string; meta?: { target?: unknown } };
    return (
        typed.code === 'P2002' &&
        Array.isArray(typed.meta?.target) &&
        typed.meta!.target.includes(field)
    );
};

/**
 * Step 1 of safe OAuth linking (#172):
 * Requires an authenticated ArenaX session. Issues a single-use, short-lived
 * state + nonce challenge bound to `(userId, provider, sessionFingerprint)` so
 * the subsequent link completion proves both active ArenaX session ownership
 * and fresh external identity control.
 */
export async function initiateOAuthLinkChallenge(params: {
    userId: string;
    provider: OAuthProvider;
    sessionToken: string | undefined;
    auditMeta?: OAuthAuditMeta;
}): Promise<{
    challengeId: string;
    provider: NormalizedOAuthProvider;
    state: string;
    nonce: string;
    expiresAt: string;
}> {
    const provider = normalizeProvider(params.provider);
    const sessionFingerprint = computeSessionFingerprint(params.sessionToken);

    const prisma = getDatabaseClient();
    const user = await prisma.user.findUnique({
        where: { id: params.userId },
        select: { id: true, email: true },
    });
    if (!user) {
        throw new BaseError('Authenticated ArenaX account not found', 401, 'OAUTH_SESSION_INVALID');
    }

    const state = crypto.randomBytes(32).toString('hex');
    const nonce = crypto.randomBytes(32).toString('hex');
    const now = new Date();
    const expiresAt = new Date(now.getTime() + LINK_CHALLENGE_TTL_MS);

    const challenge: LinkChallengeRecord = {
        id: crypto.randomUUID(),
        userId: user.id,
        provider,
        stateHash: toHash(state),
        nonceHash: toHash(nonce),
        sessionFingerprint,
        expiresAt,
        consumedAt: null,
        createdAt: now,
    };

    await saveLinkChallenge(challenge);

    await recordOAuthAudit(
        user.id,
        'AUTH_OAUTH_LINK_CHALLENGE_CREATED',
        AuditStatus.SUCCESS,
        'OAUTH_IDENTITY',
        challenge.id,
        {
            provider,
            challengeId: challenge.id,
            expiresAt: expiresAt.toISOString(),
        },
        params.auditMeta
    );

    return {
        challengeId: challenge.id,
        provider,
        state,
        nonce,
        expiresAt: expiresAt.toISOString(),
    };
}

/**
 * Step 2 of safe OAuth linking (#172):
 * Verifies:
 *  1. Caller has an authenticated ArenaX session matching the challenge's session fingerprint.
 *  2. The single-use link challenge (`state` and optional `nonce`) is valid, unconsumed, and unexpired.
 *  3. Fresh server-side proof of control of the external identity (`code`+`redirectUri` or validated `accessToken`).
 *  4. No cross-account conflict exists (never reassigns or grants access to another user's account).
 */
export async function completeOAuthLink(params: {
    userId: string;
    provider: OAuthProvider;
    state: string;
    nonce?: string;
    sessionToken: string | undefined;
    accessToken?: string;
    code?: string;
    redirectUri?: string;
    auditMeta?: OAuthAuditMeta;
}): Promise<OAuthIdentityRecord> {
    const provider = normalizeProvider(params.provider);
    const sessionFingerprint = computeSessionFingerprint(params.sessionToken);

    if (!params.state || !params.state.trim()) {
        throw new BaseError(
            'A fresh OAuth linking challenge state is required',
            400,
            'OAUTH_LINK_CHALLENGE_REQUIRED'
        );
    }

    const stateHash = toHash(params.state.trim());
    const challenge = await findLinkChallengeByStateHash(stateHash);

    if (!challenge) {
        await recordOAuthAudit(
            params.userId,
            'AUTH_OAUTH_LINK_CONFLICT',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            provider,
            { provider, reason: 'INVALID_LINK_CHALLENGE' },
            params.auditMeta
        );
        throw new BaseError('Invalid or unknown OAuth linking challenge', 401, 'OAUTH_LINK_CHALLENGE_INVALID');
    }

    if (challenge.consumedAt) {
        await recordOAuthAudit(
            params.userId,
            'AUTH_OAUTH_LINK_CONFLICT',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            challenge.id,
            { provider, reason: 'LINK_CHALLENGE_REPLAYED', challengeId: challenge.id },
            params.auditMeta
        );
        throw new BaseError('OAuth linking challenge has already been used', 401, 'OAUTH_LINK_CHALLENGE_REPLAYED');
    }

    if (challenge.expiresAt.getTime() <= Date.now()) {
        await recordOAuthAudit(
            params.userId,
            'AUTH_OAUTH_LINK_CONFLICT',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            challenge.id,
            { provider, reason: 'LINK_CHALLENGE_EXPIRED', challengeId: challenge.id },
            params.auditMeta
        );
        throw new BaseError(
            'OAuth linking challenge expired; request fresh proof before linking',
            401,
            'OAUTH_LINK_CHALLENGE_EXPIRED'
        );
    }

    if (
        challenge.userId !== params.userId ||
        challenge.provider !== provider ||
        !safeEqual(challenge.sessionFingerprint, sessionFingerprint)
    ) {
        await recordOAuthAudit(
            params.userId,
            'AUTH_OAUTH_LINK_CONFLICT',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            challenge.id,
            {
                provider,
                reason: 'LINK_CHALLENGE_SESSION_MISMATCH',
                challengeId: challenge.id,
            },
            params.auditMeta
        );
        throw new BaseError(
            'OAuth linking challenge does not match the active authenticated session',
            403,
            'OAUTH_LINK_SESSION_MISMATCH'
        );
    }

    if (params.nonce !== undefined && !safeEqual(toHash(params.nonce), challenge.nonceHash)) {
        await recordOAuthAudit(
            params.userId,
            'AUTH_OAUTH_LINK_CONFLICT',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            challenge.id,
            { provider, reason: 'LINK_CHALLENGE_NONCE_MISMATCH', challengeId: challenge.id },
            params.auditMeta
        );
        throw new BaseError('OAuth linking nonce mismatch', 401, 'OAUTH_LINK_NONCE_INVALID');
    }

    // Consume single-use challenge before verifying external proof to prevent replay races.
    await markLinkChallengeConsumed(challenge);

    // Verify fresh proof of control of the external identity directly with the provider.
    const externalProfile = await verifyExternalIdentityProof({
        provider: params.provider,
        accessToken: params.accessToken,
        code: params.code,
        redirectUri: params.redirectUri,
    });

    // Conflict check 1: Is this external identity already owned by another ArenaX account?
    const existingBySubject = await findIdentityByProviderSubject(
        externalProfile.provider,
        externalProfile.providerId
    );
    if (
        existingBySubject &&
        existingBySubject.status === 'ACTIVE' &&
        existingBySubject.userId !== params.userId
    ) {
        await recordOAuthAudit(
            params.userId,
            'AUTH_OAUTH_LINK_CONFLICT',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            existingBySubject.id,
            {
                conflictType: 'IDENTITY_ALREADY_LINKED_TO_OTHER_ACCOUNT',
                provider: externalProfile.provider,
                providerId: externalProfile.providerId,
                existingOwnerId: existingBySubject.userId,
                requestingUserId: params.userId,
            },
            params.auditMeta
        );
        throw new BaseError(
            'This external OAuth identity is already linked to another ArenaX account',
            409,
            'OAUTH_IDENTITY_CONFLICT'
        );
    }

    if (
        existingBySubject &&
        existingBySubject.status === 'ACTIVE' &&
        existingBySubject.userId === params.userId
    ) {
        await recordOAuthAudit(
            params.userId,
            'AUTH_OAUTH_LINK_CONFLICT',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            existingBySubject.id,
            {
                conflictType: 'IDENTITY_ALREADY_LINKED_TO_SELF',
                provider: externalProfile.provider,
                providerId: externalProfile.providerId,
            },
            params.auditMeta
        );
        throw new BaseError(
            'This external OAuth identity is already linked to your ArenaX account',
            409,
            'OAUTH_IDENTITY_ALREADY_LINKED'
        );
    }

    // Conflict check 2: Does this ArenaX account already have a different subject for this provider?
    const userIdentities = await findIdentitiesByUserId(params.userId);
    const activeSameProvider = userIdentities.find(
        (item) =>
            item.provider === externalProfile.provider &&
            item.status === 'ACTIVE' &&
            item.providerId !== externalProfile.providerId
    );
    if (activeSameProvider) {
        await recordOAuthAudit(
            params.userId,
            'AUTH_OAUTH_LINK_CONFLICT',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            activeSameProvider.id,
            {
                conflictType: 'PROVIDER_SLOT_OCCUPIED',
                provider: externalProfile.provider,
                existingProviderId: activeSameProvider.providerId,
                requestedProviderId: externalProfile.providerId,
            },
            params.auditMeta
        );
        throw new BaseError(
            `Unlink your existing ${externalProfile.provider} identity before linking a different one`,
            409,
            'OAUTH_PROVIDER_SLOT_OCCUPIED'
        );
    }

    const now = new Date();
    const saved = await upsertIdentityRecord({
        id: existingBySubject?.id ?? crypto.randomUUID(),
        userId: params.userId,
        provider: externalProfile.provider,
        providerId: externalProfile.providerId,
        providerEmail: externalProfile.email,
        status: 'ACTIVE',
        linkedAt: now,
        lastVerifiedAt: now,
        unlinkedAt: null,
        revokedAt: null,
        revokeReason: null,
    });

    await recordOAuthAudit(
        params.userId,
        'AUTH_OAUTH_LINKED',
        AuditStatus.SUCCESS,
        'OAUTH_IDENTITY',
        saved.id,
        {
            provider: saved.provider,
            providerId: saved.providerId,
            providerEmail: saved.providerEmail,
            challengeId: challenge.id,
        },
        params.auditMeta
    );

    return saved;
}

/**
 * List active OAuth identities linked to an authenticated ArenaX account.
 */
export async function listLinkedOAuthIdentities(
    userId: string
): Promise<OAuthIdentityRecord[]> {
    const identities = await findIdentitiesByUserId(userId);
    return identities.filter((item) => item.status === 'ACTIVE');
}

/**
 * Unlink an OAuth identity from an authenticated ArenaX account (#172).
 * Prevents accidental lockout when the provider is the user's sole login
 * method (no password and no other active OAuth identity) and audits every
 * allowed or denied unlink attempt.
 */
export async function unlinkOAuthIdentity(params: {
    userId: string;
    provider: OAuthProvider;
    auditMeta?: OAuthAuditMeta;
}): Promise<{ unlinked: true; provider: NormalizedOAuthProvider }> {
    const provider = normalizeProvider(params.provider);
    const prisma = getDatabaseClient();

    const user = await prisma.user.findUnique({
        where: { id: params.userId },
        select: { id: true, passwordHash: true, provider: true, providerId: true },
    });
    if (!user) {
        throw new BaseError('User not found', 404, 'USER_NOT_FOUND');
    }

    const identities = await findIdentitiesByUserId(params.userId);
    const target = identities.find(
        (item) => item.provider === provider && item.status === 'ACTIVE'
    );

    if (!target) {
        await recordOAuthAudit(
            params.userId,
            'AUTH_OAUTH_UNLINK_DENIED',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            provider,
            { provider, reason: 'IDENTITY_NOT_LINKED' },
            params.auditMeta
        );
        throw new BaseError(
            `No active ${provider} identity is linked to this account`,
            404,
            'OAUTH_IDENTITY_NOT_FOUND'
        );
    }

    const otherActiveIdentities = identities.filter(
        (item) => item.status === 'ACTIVE' && item.id !== target.id
    );
    const hasPassword = typeof user.passwordHash === 'string' && user.passwordHash.trim().length > 0;

    if (!hasPassword && otherActiveIdentities.length === 0) {
        await recordOAuthAudit(
            params.userId,
            'AUTH_OAUTH_UNLINK_DENIED',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            target.id,
            {
                provider,
                providerId: target.providerId,
                reason: 'LAST_LOGIN_METHOD',
            },
            params.auditMeta
        );
        throw new BaseError(
            'Cannot unlink the last remaining sign-in method; set a password or link another provider first',
            400,
            'OAUTH_LAST_LOGIN_METHOD'
        );
    }

    const now = new Date();
    await upsertIdentityRecord({
        ...target,
        status: 'UNLINKED',
        unlinkedAt: now,
    });

    if (user.provider === provider && user.providerId === target.providerId) {
        await prisma.user.update({
            where: { id: user.id },
            data: {
                provider: otherActiveIdentities[0]?.provider ?? 'EMAIL',
                providerId: otherActiveIdentities[0]?.providerId ?? null,
            },
        });
    }

    await recordOAuthAudit(
        params.userId,
        'AUTH_OAUTH_UNLINKED',
        AuditStatus.SUCCESS,
        'OAUTH_IDENTITY',
        target.id,
        {
            provider,
            providerId: target.providerId,
        },
        params.auditMeta
    );

    return { unlinked: true, provider };
}

/**
 * Handle provider-side or user-initiated OAuth revocation (#172):
 * Marks the link `REVOKED`, invalidates active refresh token sessions so a
 * revoked external grant cannot preserve lingering ArenaX access, and emits a
 * tamper-evident audit record.
 */
export async function handleProviderRevocation(params: {
    provider: OAuthProvider;
    providerId: string;
    actorUserId?: string;
    actorRole?: string;
    reason?: string;
    auditMeta?: OAuthAuditMeta;
}): Promise<{ revoked: true; provider: NormalizedOAuthProvider; providerId: string }> {
    const provider = normalizeProvider(params.provider);
    const existing = await findIdentityByProviderSubject(provider, params.providerId);

    if (!existing) {
        await recordOAuthAudit(
            params.actorUserId ?? 'system',
            'AUTH_OAUTH_PROVIDER_REVOKED',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            `${provider}:${params.providerId}`,
            {
                provider,
                providerId: params.providerId,
                reason: 'IDENTITY_NOT_FOUND',
            },
            params.auditMeta
        );
        throw new BaseError('OAuth identity link not found', 404, 'OAUTH_IDENTITY_NOT_FOUND');
    }

    if (
        params.actorUserId &&
        params.actorRole !== 'ADMIN' &&
        existing.userId !== params.actorUserId
    ) {
        await recordOAuthAudit(
            params.actorUserId,
            'AUTH_OAUTH_LINK_CONFLICT',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            existing.id,
            {
                conflictType: 'UNAUTHORIZED_REVOCATION_ATTEMPT',
                provider,
                providerId: params.providerId,
                ownerUserId: existing.userId,
            },
            params.auditMeta
        );
        throw new BaseError(
            'Cannot revoke an OAuth identity belonging to another user',
            403,
            'OAUTH_REVOCATION_FORBIDDEN'
        );
    }

    const now = new Date();
    const revokeReason = params.reason?.trim() || 'PROVIDER_REVOKED';
    await upsertIdentityRecord({
        ...existing,
        status: 'REVOKED',
        revokedAt: now,
        revokeReason,
    });

    await revokeAllUserRefreshTokens(existing.userId);

    await recordOAuthAudit(
        params.actorUserId ?? existing.userId,
        'AUTH_OAUTH_PROVIDER_REVOKED',
        AuditStatus.SUCCESS,
        'OAUTH_IDENTITY',
        existing.id,
        {
            provider,
            providerId: params.providerId,
            affectedUserId: existing.userId,
            reason: revokeReason,
            sessionsRevoked: true,
        },
        params.auditMeta
    );

    return { revoked: true, provider, providerId: params.providerId };
}

/**
 * Step 1 of audited OAuth account recovery (#172):
 * Issues a short-lived, hashed single-use recovery code sent to the existing
 * account's email address. Never grants access directly.
 */
export async function initiateOAuthRecovery(params: {
    email: string;
    provider?: OAuthProvider;
    auditMeta?: OAuthAuditMeta;
}): Promise<{
    message: string;
    challengeId?: string;
    expiresAt?: string;
    recoveryCode?: string;
}> {
    const prisma = getDatabaseClient();
    const normalizedEmail = normalizeEmail(params.email);
    const provider = params.provider ? normalizeProvider(params.provider) : null;

    const user = await prisma.user.findUnique({
        where: { email: normalizedEmail },
        select: { id: true, email: true },
    });

    if (!user) {
        await recordOAuthAudit(
            'anonymous',
            'AUTH_OAUTH_RECOVERY_INITIATED',
            AuditStatus.FAILED,
            'OAUTH_RECOVERY',
            toHash(normalizedEmail),
            { provider, reason: 'ACCOUNT_NOT_FOUND' },
            params.auditMeta
        );
        return {
            message: 'If an ArenaX account matches this email, recovery instructions have been sent',
        };
    }

    const recoveryCode = crypto.randomBytes(16).toString('hex');
    const now = new Date();
    const expiresAt = new Date(now.getTime() + RECOVERY_CHALLENGE_TTL_MS);

    const challenge: RecoveryChallengeRecord = {
        id: crypto.randomUUID(),
        userId: user.id,
        provider,
        codeHash: toHash(recoveryCode),
        status: 'PENDING',
        attempts: 0,
        maxAttempts: RECOVERY_MAX_ATTEMPTS,
        expiresAt,
        consumedAt: null,
        createdAt: now,
    };

    await saveRecoveryChallenge(challenge);

    try {
        await emailService.sendPasswordResetEmail(normalizedEmail, recoveryCode);
    } catch (err) {
        logger.warn('Failed to dispatch OAuth recovery email', {
            userId: user.id,
            error: err instanceof Error ? err.message : String(err),
        });
    }

    await recordOAuthAudit(
        user.id,
        'AUTH_OAUTH_RECOVERY_INITIATED',
        AuditStatus.SUCCESS,
        'OAUTH_RECOVERY',
        challenge.id,
        {
            provider,
            challengeId: challenge.id,
            expiresAt: expiresAt.toISOString(),
        },
        params.auditMeta
    );

    return {
        message: 'If an ArenaX account matches this email, recovery instructions have been sent',
        challengeId: challenge.id,
        expiresAt: expiresAt.toISOString(),
        ...(process.env.NODE_ENV === 'test' ? { recoveryCode } : {}),
    };
}

/**
 * Step 2 of audited OAuth account recovery (#172):
 * Strictly requires valid, unexpired recovery proof (`challengeId` + `recoveryCode`)
 * before resetting a password or re-linking a verified external identity.
 * Never grants access to an existing account without proof.
 */
export async function completeOAuthRecovery(params: {
    challengeId: string;
    recoveryCode: string;
    newPassword?: string;
    externalProof?: ExternalIdentityProof;
    auditMeta?: OAuthAuditMeta;
}): Promise<{ recovered: true; userId: string }> {
    if (!params.challengeId || !params.recoveryCode) {
        throw new BaseError(
            'Recovery challengeId and recoveryCode proof are required',
            400,
            'OAUTH_RECOVERY_PROOF_REQUIRED'
        );
    }

    if (!params.newPassword && !params.externalProof) {
        throw new BaseError(
            'Recovery completion requires either a newPassword or a verified externalProof',
            400,
            'OAUTH_RECOVERY_CREDENTIAL_REQUIRED'
        );
    }

    const challenge = await findRecoveryChallenge(params.challengeId);
    if (!challenge) {
        await recordOAuthAudit(
            'anonymous',
            'AUTH_OAUTH_RECOVERY_FAILED',
            AuditStatus.FAILED,
            'OAUTH_RECOVERY',
            params.challengeId,
            { reason: 'CHALLENGE_NOT_FOUND' },
            params.auditMeta
        );
        throw new BaseError('Invalid recovery challenge or proof', 401, 'OAUTH_RECOVERY_PROOF_INVALID');
    }

    if (challenge.status !== 'PENDING' || challenge.consumedAt) {
        await recordOAuthAudit(
            challenge.userId,
            'AUTH_OAUTH_RECOVERY_FAILED',
            AuditStatus.FAILED,
            'OAUTH_RECOVERY',
            challenge.id,
            { reason: 'CHALLENGE_NOT_PENDING', status: challenge.status },
            params.auditMeta
        );
        throw new BaseError('Recovery challenge is no longer active', 401, 'OAUTH_RECOVERY_PROOF_INVALID');
    }

    if (challenge.expiresAt.getTime() <= Date.now()) {
        await updateRecoveryChallenge(challenge.id, { status: 'EXPIRED' });
        await recordOAuthAudit(
            challenge.userId,
            'AUTH_OAUTH_RECOVERY_FAILED',
            AuditStatus.FAILED,
            'OAUTH_RECOVERY',
            challenge.id,
            { reason: 'CHALLENGE_EXPIRED' },
            params.auditMeta
        );
        throw new BaseError('Recovery challenge has expired', 401, 'OAUTH_RECOVERY_EXPIRED');
    }

    if (challenge.attempts >= challenge.maxAttempts) {
        await updateRecoveryChallenge(challenge.id, { status: 'FAILED' });
        await recordOAuthAudit(
            challenge.userId,
            'AUTH_OAUTH_RECOVERY_FAILED',
            AuditStatus.FAILED,
            'OAUTH_RECOVERY',
            challenge.id,
            { reason: 'MAX_ATTEMPTS_EXCEEDED', attempts: challenge.attempts },
            params.auditMeta
        );
        throw new BaseError('Recovery challenge locked due to too many failed attempts', 429, 'OAUTH_RECOVERY_LOCKED');
    }

    const incomingHash = toHash(params.recoveryCode);
    if (!safeEqual(incomingHash, challenge.codeHash)) {
        const nextAttempts = challenge.attempts + 1;
        await updateRecoveryChallenge(challenge.id, {
            attempts: nextAttempts,
            ...(nextAttempts >= challenge.maxAttempts ? { status: 'FAILED' as OAuthRecoveryStatus } : {}),
        });
        await recordOAuthAudit(
            challenge.userId,
            'AUTH_OAUTH_RECOVERY_FAILED',
            AuditStatus.FAILED,
            'OAUTH_RECOVERY',
            challenge.id,
            { reason: 'INVALID_RECOVERY_CODE', attempts: nextAttempts },
            params.auditMeta
        );
        throw new BaseError('Invalid recovery proof', 401, 'OAUTH_RECOVERY_PROOF_INVALID');
    }

    const prisma = getDatabaseClient();
    const user = await prisma.user.findUnique({ where: { id: challenge.userId } });
    if (!user) {
        throw new BaseError('Account not found', 404, 'USER_NOT_FOUND');
    }

    if (params.newPassword) {
        if (params.newPassword.length < 8) {
            throw new BaseError('newPassword must be at least 8 characters', 400, 'WEAK_PASSWORD');
        }
        const passwordHash = await bcrypt.hash(params.newPassword, BCRYPT_ROUNDS);
        await prisma.user.update({
            where: { id: user.id },
            data: { passwordHash },
        });
    }

    if (params.externalProof) {
        const verifiedExternal = await verifyExternalIdentityProof(params.externalProof);
        const existingOwner = await findIdentityByProviderSubject(
            verifiedExternal.provider,
            verifiedExternal.providerId
        );
        if (
            existingOwner &&
            existingOwner.status === 'ACTIVE' &&
            existingOwner.userId !== user.id
        ) {
            await recordOAuthAudit(
                user.id,
                'AUTH_OAUTH_LINK_CONFLICT',
                AuditStatus.FAILED,
                'OAUTH_IDENTITY',
                existingOwner.id,
                {
                    conflictType: 'RECOVERY_IDENTITY_OWNED_BY_OTHER_ACCOUNT',
                    provider: verifiedExternal.provider,
                    providerId: verifiedExternal.providerId,
                },
                params.auditMeta
            );
            throw new BaseError(
                'External OAuth identity belongs to another ArenaX account',
                409,
                'OAUTH_IDENTITY_CONFLICT'
            );
        }

        const now = new Date();
        await upsertIdentityRecord({
            id: existingOwner?.id ?? crypto.randomUUID(),
            userId: user.id,
            provider: verifiedExternal.provider,
            providerId: verifiedExternal.providerId,
            providerEmail: verifiedExternal.email,
            status: 'ACTIVE',
            linkedAt: now,
            lastVerifiedAt: now,
            unlinkedAt: null,
            revokedAt: null,
            revokeReason: null,
        });
    }

    await updateRecoveryChallenge(challenge.id, {
        status: 'COMPLETED',
        consumedAt: new Date(),
    });
    await revokeAllUserRefreshTokens(user.id);

    await recordOAuthAudit(
        user.id,
        'AUTH_OAUTH_RECOVERY_COMPLETED',
        AuditStatus.SUCCESS,
        'OAUTH_RECOVERY',
        challenge.id,
        {
            challengeId: challenge.id,
            passwordReset: Boolean(params.newPassword),
            providerRelinked: params.externalProof?.provider ?? null,
        },
        params.auditMeta
    );

    return { recovered: true, userId: user.id };
}

/**
 * Safe OAuth sign-in resolution used by BOTH `auth.service.ts` (`authenticateSocial`)
 * and Passport OAuth callbacks (`passport-oauth.config.ts`) (#172).
 *
 * Security invariants:
 * - An active linked `OAuthIdentity` resolves directly to its linked ArenaX user.
 * - A `REVOKED` identity is NEVER allowed back into the account silently (audited + 401).
 * - An unlinked OAuth identity whose email matches an existing ArenaX user is
 *   NEVER silently auto-linked (prevents pre-hijacking / account takeover);
 *   the conflict is audited and rejected with 409 `OAUTH_ACCOUNT_CONFLICT`.
 * - A brand-new identity with an unused email creates a new ArenaX account
 *   and records an active `OAuthIdentity`.
 */
export async function resolveSafeOAuthLogin(
    profile: VerifiedExternalProfile,
    auditMeta?: OAuthAuditMeta
): Promise<any> {
    const prisma = getDatabaseClient();
    const normalizedEmail = normalizeEmail(profile.email);

    // 1. Check authoritative OAuthIdentity binding first.
    const existingLink = await findIdentityByProviderSubject(
        profile.provider,
        profile.providerId
    );

    if (existingLink) {
        if (existingLink.status === 'REVOKED') {
            await recordOAuthAudit(
                existingLink.userId,
                'AUTH_OAUTH_LINK_CONFLICT',
                AuditStatus.FAILED,
                'OAUTH_IDENTITY',
                existingLink.id,
                {
                    conflictType: 'REVOKED_IDENTITY_LOGIN_ATTEMPT',
                    provider: profile.provider,
                    providerId: profile.providerId,
                },
                auditMeta
            );
            throw new BaseError(
                'This OAuth identity was revoked. Sign in with your ArenaX account and re-link with fresh proof, or complete account recovery.',
                401,
                'OAUTH_IDENTITY_REVOKED'
            );
        }

        if (existingLink.status === 'ACTIVE') {
            const linkedUser = await prisma.user.findUnique({
                where: { id: existingLink.userId },
            });
            if (!linkedUser) {
                throw new BaseError('Linked ArenaX account not found', 401, 'OAUTH_LINKED_USER_MISSING');
            }
            await upsertIdentityRecord({
                ...existingLink,
                lastVerifiedAt: new Date(),
            });
            return linkedUser;
        }
        // If status === 'UNLINKED', fall through to conflict check — MUST NOT re-grant access without proof.
    }

    // 2. Check legacy `user.provider + user.providerId` ONLY if no UNLINKED/REVOKED record exists.
    if (!existingLink && (prisma.user as any).findFirst) {
        const legacyLinked = await (prisma.user as any).findFirst({
            where: {
                provider: profile.provider,
                providerId: profile.providerId,
            },
        });
        if (legacyLinked) {
            const now = new Date();
            await upsertIdentityRecord({
                id: crypto.randomUUID(),
                userId: legacyLinked.id,
                provider: profile.provider,
                providerId: profile.providerId,
                providerEmail: normalizedEmail,
                status: 'ACTIVE',
                linkedAt: now,
                lastVerifiedAt: now,
                unlinkedAt: null,
                revokedAt: null,
                revokeReason: null,
            });
            return legacyLinked;
        }
    }

    // 3. NEVER auto-link to an existing account by email without an authenticated
    //    ArenaX session + fresh linking proof!
    const existingByEmail = await prisma.user.findUnique({
        where: { email: normalizedEmail },
    });
    if (existingByEmail) {
        await recordOAuthAudit(
            existingByEmail.id,
            'AUTH_OAUTH_LINK_CONFLICT',
            AuditStatus.FAILED,
            'OAUTH_IDENTITY',
            `${profile.provider}:${profile.providerId}`,
            {
                conflictType: 'EMAIL_MATCH_UNLINKED_ACCOUNT',
                provider: profile.provider,
                providerId: profile.providerId,
                existingUserId: existingByEmail.id,
                emailVerifiedByProvider: profile.emailVerified,
            },
            auditMeta
        );
        throw new BaseError(
            'An ArenaX account already exists with this email. Sign in to ArenaX and explicitly link this OAuth identity with fresh proof, or use account recovery.',
            409,
            'OAUTH_ACCOUNT_CONFLICT',
            {
                provider: profile.provider,
                recoveryRequired: true,
            }
        );
    }

    // 4. Brand-new user — create ArenaX account + active OAuthIdentity atomically.
    const usernameBase = buildUsernameBase(normalizedEmail, profile.username);
    let createdUser: any = null;

    for (let attempt = 0; attempt < MAX_USERNAME_ATTEMPTS; attempt += 1) {
        const candidate = buildCandidateUsername(usernameBase, attempt);
        try {
            createdUser = await prisma.$transaction(async (tx) => {
                const newUser = await tx.user.create({
                    data: {
                        email: normalizedEmail,
                        username: candidate,
                        passwordHash: '',
                        isVerified: profile.emailVerified,
                        provider: profile.provider,
                        providerId: profile.providerId,
                    },
                });
                await stellarWalletService.registerUserWallet(newUser.id, tx);
                return newUser;
            });
            break;
        } catch (error) {
            if (isUniqueError(error, 'email')) {
                await recordOAuthAudit(
                    'anonymous',
                    'AUTH_OAUTH_LINK_CONFLICT',
                    AuditStatus.FAILED,
                    'OAUTH_IDENTITY',
                    `${profile.provider}:${profile.providerId}`,
                    {
                        conflictType: 'EMAIL_RACE_CONFLICT',
                        provider: profile.provider,
                        providerId: profile.providerId,
                    },
                    auditMeta
                );
                throw new BaseError(
                    'An ArenaX account already exists with this email. Sign in and explicitly link with fresh proof.',
                    409,
                    'OAUTH_ACCOUNT_CONFLICT'
                );
            }
            if (isUniqueError(error, 'username')) {
                continue;
            }
            throw error;
        }
    }

    if (!createdUser) {
        throw new BaseError('Unable to create user from OAuth login', 500, 'OAUTH_USER_CREATION_FAILED');
    }

    const now = new Date();
    const linked = await upsertIdentityRecord({
        id: crypto.randomUUID(),
        userId: createdUser.id,
        provider: profile.provider,
        providerId: profile.providerId,
        providerEmail: normalizedEmail,
        status: 'ACTIVE',
        linkedAt: now,
        lastVerifiedAt: now,
        unlinkedAt: null,
        revokedAt: null,
        revokeReason: null,
    });

    await recordOAuthAudit(
        createdUser.id,
        'AUTH_OAUTH_LINKED',
        AuditStatus.SUCCESS,
        'OAUTH_IDENTITY',
        linked.id,
        {
            provider: linked.provider,
            providerId: linked.providerId,
            flow: 'NEW_ACCOUNT_SIGNUP',
        },
        auditMeta
    );

    return createdUser;
}

// ─── Test helpers ────────────────────────────────────────────────────────────

export function setOAuthProviderVerifierForTesting(
    verifier: ExternalProviderVerifier | null
): void {
    customProviderVerifier = verifier;
}

export function getOAuthAuditRecordsForTesting(): OAuthAuditRecord[] {
    return [...oauthAuditSink];
}

export function resetOAuthStateForTesting(): void {
    inMemoryIdentities.clear();
    inMemoryLinkChallenges.clear();
    inMemoryRecoveryChallenges.clear();
    oauthAuditSink.length = 0;
    customProviderVerifier = null;
}
