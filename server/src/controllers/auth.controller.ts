import { Request, Response, NextFunction } from 'express';
import { z } from 'zod';
import {
    loginUser,
    logoutUser,
    refreshSession,
    registerUser,
    authenticateSocial,
    createGuestSession,
    verifyEmail,
    resendVerificationEmail,
    forgotPassword,
    resetPassword
} from '../services/auth.service';
import {
    completeOAuthLink,
    completeOAuthRecovery,
    handleProviderRevocation,
    initiateOAuthLinkChallenge,
    initiateOAuthRecovery,
    listLinkedOAuthIdentities,
    unlinkOAuthIdentity,
    type OAuthAuditMeta
} from '../services/oauth-identity.service';
import { HttpError } from '../utils/http-error';

const parseBody = <T>(schema: z.ZodSchema<T>, body: unknown): T => {
    const parsed = schema.safeParse(body);

    if (!parsed.success) {
        throw new HttpError(400, parsed.error.issues[0]?.message || 'Invalid request');
    }

    return parsed.data;
};

const registerSchema = z.object({
    email: z.string().email(),
    password: z.string().min(8).max(128),
    username: z.string().min(3).max(24).optional()
});

const loginSchema = z.object({
    email: z.string().email(),
    password: z.string().min(1)
});

const refreshSchema = z.object({
    refreshToken: z.string().min(1)
});

// Twitch supports the Authorization Code flow (`code` + `redirectUri`), which
// exchanges the code server-side; `accessToken` remains for google/discord and
// for existing Twitch clients (#1160). Which combination is valid depends on
// the provider, so that check lives in the service where the error message can
// name the provider.
const socialAuthSchema = z.object({
    provider: z.enum(['google', 'discord', 'twitch']),
    accessToken: z.string().min(1).optional(),
    code: z.string().min(1).optional(),
    redirectUri: z.string().url().optional()
});

const verifyEmailSchema = z.object({
    token: z.string().min(1)
});

const resendVerificationEmailSchema = z.object({
    email: z.string().email()
});

const forgotPasswordSchema = z.object({
    email: z.string().email()
});

const resetPasswordSchema = z.object({
    token: z.string().min(1),
    newPassword: z.string().min(8).max(128)
});

const oauthProviderSchema = z.enum(['google', 'discord', 'twitch']);

const oauthLinkChallengeSchema = z.object({
    provider: oauthProviderSchema
});

const oauthLinkCompleteSchema = z.object({
    provider: oauthProviderSchema,
    state: z.string().min(1),
    nonce: z.string().min(1).optional(),
    accessToken: z.string().min(1).optional(),
    code: z.string().min(1).optional(),
    redirectUri: z.string().url().optional()
});

const oauthRevokeSchema = z.object({
    provider: oauthProviderSchema,
    providerId: z.string().min(1),
    reason: z.string().max(256).optional()
});

const oauthRecoveryInitiateSchema = z.object({
    email: z.string().email(),
    provider: oauthProviderSchema.optional()
});

const oauthRecoveryCompleteSchema = z.object({
    challengeId: z.string().min(1),
    recoveryCode: z.string().min(1),
    newPassword: z.string().min(8).max(128).optional(),
    externalProof: z
        .object({
            provider: oauthProviderSchema,
            accessToken: z.string().min(1).optional(),
            code: z.string().min(1).optional(),
            redirectUri: z.string().url().optional()
        })
        .optional()
});

const getAuthAuditMeta = (req: Request): OAuthAuditMeta => ({
    requestId: req.auditContext?.requestId ?? req.requestId,
    correlationId: req.correlationId,
    ipAddress: req.auditContext?.ipAddress ?? req.ip,
    userAgent: req.auditContext?.userAgent ?? req.header('user-agent')
});

export const register = async (req: Request, res: Response, next: NextFunction) => {
    try {
        const payload = parseBody(registerSchema, req.body);
        const result = await registerUser(payload);
        res.status(201).json(result);
    } catch (error) {
        next(error);
    }
};

export const login = async (req: Request, res: Response, next: NextFunction) => {
    try {
        const payload = parseBody(loginSchema, req.body);
        const result = await loginUser(payload);
        res.status(200).json(result);
    } catch (error) {
        next(error);
    }
};

export const refresh = async (req: Request, res: Response, next: NextFunction) => {
    try {
        const payload = parseBody(refreshSchema, req.body);
        const result = await refreshSession(payload);
        res.status(200).json(result);
    } catch (error) {
        next(error);
    }
};

export const logout = async (req: Request, res: Response, next: NextFunction) => {
    try {
        const payload = parseBody(refreshSchema, req.body);
        await logoutUser(payload);
        res.status(204).send();
    } catch (error) {
        next(error);
    }
};

export const socialAuth = async (req: Request, res: Response, next: NextFunction) => {
    try {
        const { provider } = req.params;
        const payload = parseBody(socialAuthSchema, { ...req.body, provider });
        const result = await authenticateSocial(payload, getAuthAuditMeta(req));
        res.status(200).json(result);
    } catch (error) {
        next(error);
    }
};

export const guestSession = async (req: Request, res: Response, next: NextFunction) => {
    try {
        const result = await createGuestSession();
        res.status(201).json(result);
    } catch (error) {
        next(error);
    }
};

export const verifyEmailHandler = async (req: Request, res: Response, next: NextFunction) => {
    try {
        const payload = parseBody(verifyEmailSchema, req.body);
        await verifyEmail(payload);
        res.status(200).json({ message: 'Email verified successfully' });
    } catch (error) {
        next(error);
    }
};

export const resendVerificationEmailHandler = async (req: Request, res: Response, next: NextFunction) => {
    try {
        const payload = parseBody(resendVerificationEmailSchema, req.body);
        const result = await resendVerificationEmail(payload);
        res.status(200).json(result);
    } catch (error) {
        next(error);
    }
};

export const forgotPasswordHandler = async (req: Request, res: Response, next: NextFunction) => {
    try {
        const payload = parseBody(forgotPasswordSchema, req.body);
        const result = await forgotPassword(payload);
        res.status(200).json(result);
    } catch (error) {
        next(error);
    }
};

export const resetPasswordHandler = async (req: Request, res: Response, next: NextFunction) => {
    try {
        const payload = parseBody(resetPasswordSchema, req.body);
        await resetPassword(payload);
        res.status(200).json({ message: 'Password reset successfully' });
    } catch (error) {
        next(error);
    }
};

export const initiateOAuthLinkHandler = async (
    req: Request,
    res: Response,
    next: NextFunction
) => {
    try {
        if (!req.user?.id) {
            throw new HttpError(401, 'Unauthorized');
        }
        const payload = parseBody(oauthLinkChallengeSchema, req.body);
        const result = await initiateOAuthLinkChallenge({
            userId: req.user.id,
            provider: payload.provider,
            sessionToken: req.header('authorization'),
            auditMeta: getAuthAuditMeta(req)
        });
        res.status(201).json(result);
    } catch (error) {
        next(error);
    }
};

export const completeOAuthLinkHandler = async (
    req: Request,
    res: Response,
    next: NextFunction
) => {
    try {
        if (!req.user?.id) {
            throw new HttpError(401, 'Unauthorized');
        }
        const payload = parseBody(oauthLinkCompleteSchema, req.body);
        const identity = await completeOAuthLink({
            userId: req.user.id,
            provider: payload.provider,
            state: payload.state,
            nonce: payload.nonce,
            sessionToken: req.header('authorization'),
            accessToken: payload.accessToken,
            code: payload.code,
            redirectUri: payload.redirectUri,
            auditMeta: getAuthAuditMeta(req)
        });
        res.status(200).json({ identity });
    } catch (error) {
        next(error);
    }
};

export const listOAuthIdentitiesHandler = async (
    req: Request,
    res: Response,
    next: NextFunction
) => {
    try {
        if (!req.user?.id) {
            throw new HttpError(401, 'Unauthorized');
        }
        const identities = await listLinkedOAuthIdentities(req.user.id);
        res.status(200).json({ identities });
    } catch (error) {
        next(error);
    }
};

export const unlinkOAuthIdentityHandler = async (
    req: Request,
    res: Response,
    next: NextFunction
) => {
    try {
        if (!req.user?.id) {
            throw new HttpError(401, 'Unauthorized');
        }
        const provider = parseBody(oauthProviderSchema, req.params.provider);
        const result = await unlinkOAuthIdentity({
            userId: req.user.id,
            provider,
            auditMeta: getAuthAuditMeta(req)
        });
        res.status(200).json(result);
    } catch (error) {
        next(error);
    }
};

export const revokeOAuthIdentityHandler = async (
    req: Request,
    res: Response,
    next: NextFunction
) => {
    try {
        if (!req.user?.id) {
            throw new HttpError(401, 'Unauthorized');
        }
        const payload = parseBody(oauthRevokeSchema, req.body);
        const result = await handleProviderRevocation({
            provider: payload.provider,
            providerId: payload.providerId,
            actorUserId: req.user.id,
            actorRole: req.user.role,
            reason: payload.reason,
            auditMeta: getAuthAuditMeta(req)
        });
        res.status(200).json(result);
    } catch (error) {
        next(error);
    }
};

export const initiateOAuthRecoveryHandler = async (
    req: Request,
    res: Response,
    next: NextFunction
) => {
    try {
        const payload = parseBody(oauthRecoveryInitiateSchema, req.body);
        const result = await initiateOAuthRecovery({
            email: payload.email,
            provider: payload.provider,
            auditMeta: getAuthAuditMeta(req)
        });
        res.status(200).json(result);
    } catch (error) {
        next(error);
    }
};

export const completeOAuthRecoveryHandler = async (
    req: Request,
    res: Response,
    next: NextFunction
) => {
    try {
        const payload = parseBody(oauthRecoveryCompleteSchema, req.body);
        const result = await completeOAuthRecovery({
            challengeId: payload.challengeId,
            recoveryCode: payload.recoveryCode,
            newPassword: payload.newPassword,
            externalProof: payload.externalProof,
            auditMeta: getAuthAuditMeta(req)
        });
        res.status(200).json(result);
    } catch (error) {
        next(error);
    }
};
