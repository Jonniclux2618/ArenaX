/**
 * Centralised environment configuration.
 *
 * All process.env reads for application config should go through the `env`
 * singleton exported from this module. Direct process.env access in service
 * files is a code smell — add the variable here instead.
 *
 * Loading order (handled by server.ts before this module is imported):
 *   .env.<NODE_ENV>.local   (machine-local overrides, git-ignored)
 *   .env.<NODE_ENV>         (environment-specific defaults, committed)
 *   .env.local              (cross-environment local overrides, git-ignored)
 *   .env                    (base defaults, committed)
 *
 * NODE_ENV must be set before the process starts (e.g. via the shell or a
 * process manager). It is never read from a .env file.
 */

import z from 'zod';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Accept "true" / "1" / "yes" as truthy; everything else is false. */
const boolStr = z
    .string()
    .refine((value) => ['true', 'false', '1', '0', 'yes', 'no'].includes(value.toLowerCase()), 'must be true, false, 1, 0, yes, or no')
    .transform((v) => ['true', '1', 'yes'].includes(v.toLowerCase()));

/** Accept a numeric string and coerce to number. */
const intStr = (defaultVal: string) =>
    z.string()
        .regex(/^\d+$/, 'must be a non-negative integer')
        .transform((value) => Number(value))
        .refine(Number.isSafeInteger, 'must be a safe integer')
        .default(defaultVal);

const urlWithProtocols = (protocols: string[]) =>
    z.string().url().refine((value) => {
        try {
            const protocol = new URL(value).protocol.slice(0, -1);
            return protocols.includes(protocol);
        } catch {
            return false;
        }
    });

const placeholderSecrets = new Set([
    'changeme',
    'change_me',
    'default_secret_change_in_production',
    'password',
    'secret',
    'supersecretkey',
    'test'
]);

const isPlaceholderSecret = (value: string): boolean => {
    const normalized = value.trim().toLowerCase();
    return placeholderSecrets.has(normalized) ||
        /^(replace|your|example|placeholder|xxx|<)/.test(normalized);
};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

const envSchema = z.object({
    // ── Runtime ──────────────────────────────────────────────────────────────
    NODE_ENV: z
        .enum(['development', 'staging', 'production', 'test'])
        .default('development'),
    PORT: intStr('3001'),
    APP_VERSION: z.string().optional(),

    // ── Database ─────────────────────────────────────────────────────────────
    DATABASE_URL: urlWithProtocols(['postgres', 'postgresql']),
    DATABASE_POOL_MIN: intStr('2'),
    DATABASE_POOL_MAX: intStr('10'),
    DATABASE_POOL_TIMEOUT: intStr('30'),
    DATABASE_IDLE_TIMEOUT: intStr('600'),

    // ── Authentication ───────────────────────────────────────────────────────
    JWT_SECRET: z.string().min(32),
    ACCESS_TOKEN_TTL: z.string().default('15m'),
    REFRESH_TOKEN_TTL: z.string().default('7d'),
    /** RS256 private key (inline PEM, \n-escaped). Takes precedence over JWT_SECRET. */
    JWT_PRIVATE_KEY: z.string().optional(),
    /** RS256 public key (inline PEM, \n-escaped). */
    JWT_PUBLIC_KEY: z.string().optional(),
    /** Path to RS256 private key file. Alternative to JWT_PRIVATE_KEY. */
    JWT_PRIVATE_KEY_FILE: z.string().optional(),
    /** Path to RS256 public key file. Alternative to JWT_PUBLIC_KEY. */
    JWT_PUBLIC_KEY_FILE: z.string().optional(),

    // ── Encryption ───────────────────────────────────────────────────────────
    ENCRYPTION_SECRET: z.string().min(32).optional(),
    WALLET_MASTER_KEYS: z.string().optional(),
    WALLET_ACTIVE_MASTER_KEY_VERSION: intStr('1').optional(),
    SENSITIVE_OPERATION_SECRET: z.string().optional(),

    // ── Logging ──────────────────────────────────────────────────────────────
    LOG_LEVEL: z
        .enum(['error', 'warn', 'info', 'debug', 'verbose', 'silly'])
        .default('info'),
    LOG_DIR: z.string().optional(),
    LOG_MAX_SIZE: z.string().default('20m'),
    LOG_MAX_FILES: z.string().default('14d'),

    // ── CORS / Origins ───────────────────────────────────────────────────────
    ARENAX_ALLOWED_ORIGINS: z.string().optional(),
    FRONTEND_URL: urlWithProtocols(['http', 'https']).default('http://localhost:3000'),
    BACKEND_URL: urlWithProtocols(['http', 'https']).default('http://localhost:3001'),

    // ── Redis / Cache ────────────────────────────────────────────────────────
    REDIS_URL: urlWithProtocols(['redis', 'rediss']).optional(),
    PROFILE_CACHE_TTL_SECONDS: intStr('300'),
    /** Comma-separated `host:port` list. When set, the cache connects via
     *  ioredis Cluster mode instead of a single REDIS_URL instance. */
    REDIS_CLUSTER_NODES: z.string().optional(),

    // ── RabbitMQ ─────────────────────────────────────────────────────────────
    RABBITMQ_URL: urlWithProtocols(['amqp', 'amqps']).optional(),
    RABBITMQ_PREFETCH_COUNT: intStr('10'),
    RABBITMQ_HEARTBEAT_INTERVAL: intStr('30'),

    // ── Rate Limiting ────────────────────────────────────────────────────────
    RATE_LIMIT_TRUSTED_IPS: z.string().optional(),
    RATE_LIMIT_TRUSTED_ACCOUNTS: z.string().optional(),
    RATE_LIMIT_REDIS_KEY_PREFIX: z.string().default('rl:'),
    RATE_LIMIT_ANALYTICS_ENABLED: boolStr.default('true'),

    // ── Metrics ──────────────────────────────────────────────────────────────
    METRICS_ENABLED: boolStr.default('true'),
    METRICS_PORT: intStr('9090'),

    // ── Health Monitor ───────────────────────────────────────────────────────
    HEALTH_CHECK_INTERVAL_MS: intStr('60000'),

    // ── Telemetry (Sentry) ───────────────────────────────────────────────────
    SENTRY_DSN: z.string().url().optional(),
    SENTRY_TRACES_SAMPLE_RATE: z
        .string()
        .transform((v) => parseFloat(v))
        .default('0'),

    // ── Email ────────────────────────────────────────────────────────────────
    EMAIL_HOST: z.string().default('localhost'),
    EMAIL_PORT: intStr('587'),
    EMAIL_SECURE: boolStr.default('false'),
    EMAIL_USER: z.string().optional(),
    EMAIL_PASSWORD: z.string().optional(),
    EMAIL_FROM: z.string().email().default('noreply@arenax.gg'),

    // ── OAuth Providers ──────────────────────────────────────────────────────
    GOOGLE_CLIENT_ID: z.string().optional(),
    GOOGLE_CLIENT_SECRET: z.string().optional(),
    DISCORD_CLIENT_ID: z.string().optional(),
    DISCORD_CLIENT_SECRET: z.string().optional(),
    TWITCH_CLIENT_ID: z.string().optional(),
    TWITCH_CLIENT_SECRET: z.string().optional(),

    // ── Stellar / Blockchain ─────────────────────────────────────────────────
    STELLAR_NETWORK: z.enum(['TESTNET', 'MAINNET']).default('TESTNET'),
    HORIZON_URL: urlWithProtocols(['http', 'https'])
        .default('https://horizon-testnet.stellar.org'),
    STELLAR_HORIZON_URL: urlWithProtocols(['http', 'https']).optional(),
    SOROBAN_RPC_URL: urlWithProtocols(['http', 'https'])
        .default('https://soroban-testnet.stellar.org'),
    SOROBAN_RPC_FAILOVERS: z.string().optional(),
    ADMIN_SECRET_KEY: z.string().optional(),
    FEE_PAYER_SECRET_KEY: z.string().optional(),
    FEE_PAYER_PUBLIC_KEY: z.string().optional(),

    // ── External Payments ────────────────────────────────────────────────────
    PAYSTACK_SECRET_KEY: z.string().optional(),
    FLUTTERWAVE_SECRET_KEY: z.string().optional(),
    /**
     * Secret Flutterwave uses to sign webhooks (the `verif-hash` header).
     * When unset, FLUTTERWAVE_SECRET_KEY is used instead.
     */
    FLUTTERWAVE_WEBHOOK_HASH: z.string().optional(),

    // ── CDN / Cache ──────────────────────────────────────────────────────────
    CDN_PROVIDER: z.enum(['cloudflare', 'cloudfront', 'fastly']).optional(),
    CDN_ZONE_ID: z.string().optional(),
    CDN_API_TOKEN: z.string().optional(),
    CDN_DISTRIBUTION_ID: z.string().optional(),
    CDN_DEFAULT_TTL: z.string().default('86400'),
    CDN_STALE_WHILE_REVALIDATE: z.string().default('86400'),
    CACHE_WARMING_INTERVAL_MS: z.string().default('300000'),

    // ── API Gateway ──────────────────────────────────────────────────────────
    API_KEY_RATE_LIMIT: z.string().default('100'),

    // ── Webhooks / Notifications ─────────────────────────────────────────────
    ADMIN_WEBHOOK_URL: urlWithProtocols(['http', 'https']).optional(),
    SECURITY_WEBHOOK_URL: urlWithProtocols(['http', 'https']).optional(),
}).superRefine((values, context) => {
    const addIssue = (path: string, message: string) =>
        context.addIssue({ code: z.ZodIssueCode.custom, path: [path], message });

    const oauthPairs = [
        ['GOOGLE_CLIENT_ID', values.GOOGLE_CLIENT_ID, 'GOOGLE_CLIENT_SECRET', values.GOOGLE_CLIENT_SECRET],
        ['DISCORD_CLIENT_ID', values.DISCORD_CLIENT_ID, 'DISCORD_CLIENT_SECRET', values.DISCORD_CLIENT_SECRET],
        ['TWITCH_CLIENT_ID', values.TWITCH_CLIENT_ID, 'TWITCH_CLIENT_SECRET', values.TWITCH_CLIENT_SECRET]
    ] as const;
    for (const [idName, clientId, secretName, clientSecret] of oauthPairs) {
        if (Boolean(clientId?.trim()) !== Boolean(clientSecret?.trim())) {
            addIssue(clientSecret?.trim() ? idName : secretName, 'must be configured together; omit both to disable this provider');
        }
    }

    const inlineKeys = [
        ['JWT_PRIVATE_KEY', values.JWT_PRIVATE_KEY, 'JWT_PRIVATE_KEY_FILE', values.JWT_PRIVATE_KEY_FILE],
        ['JWT_PUBLIC_KEY', values.JWT_PUBLIC_KEY, 'JWT_PUBLIC_KEY_FILE', values.JWT_PUBLIC_KEY_FILE]
    ] as const;
    for (const [inlineName, inlineValue, fileName, fileValue] of inlineKeys) {
        if (inlineValue?.trim() && fileValue?.trim()) {
            addIssue(fileName, `configure either ${inlineName} or ${fileName}, not both`);
        }
    }
    const hasPrivateKey = Boolean(values.JWT_PRIVATE_KEY?.trim() || values.JWT_PRIVATE_KEY_FILE?.trim());
    const hasPublicKey = Boolean(values.JWT_PUBLIC_KEY?.trim() || values.JWT_PUBLIC_KEY_FILE?.trim());
    if (hasPrivateKey !== hasPublicKey) {
        addIssue(hasPrivateKey ? 'JWT_PUBLIC_KEY' : 'JWT_PRIVATE_KEY', 'both RS256 signing and verification keys are required');
    }

    if (values.NODE_ENV === 'production') {
        const secretValues: Array<[string, string | undefined]> = [
            ['JWT_SECRET', values.JWT_SECRET],
            ['JWT_PRIVATE_KEY', values.JWT_PRIVATE_KEY],
            ['JWT_PUBLIC_KEY', values.JWT_PUBLIC_KEY],
            ['ENCRYPTION_SECRET', values.ENCRYPTION_SECRET],
            ['SENSITIVE_OPERATION_SECRET', values.SENSITIVE_OPERATION_SECRET],
            ['ADMIN_SECRET_KEY', values.ADMIN_SECRET_KEY],
            ['FEE_PAYER_SECRET_KEY', values.FEE_PAYER_SECRET_KEY],
            ['PAYSTACK_SECRET_KEY', values.PAYSTACK_SECRET_KEY],
            ['FLUTTERWAVE_SECRET_KEY', values.FLUTTERWAVE_SECRET_KEY],
            ['FLUTTERWAVE_WEBHOOK_HASH', values.FLUTTERWAVE_WEBHOOK_HASH],
            ['EMAIL_PASSWORD', values.EMAIL_PASSWORD],
            ['GOOGLE_CLIENT_SECRET', values.GOOGLE_CLIENT_SECRET],
            ['DISCORD_CLIENT_SECRET', values.DISCORD_CLIENT_SECRET],
            ['TWITCH_CLIENT_SECRET', values.TWITCH_CLIENT_SECRET]
        ];
        for (const [name, value] of secretValues) {
            if (value && (value.trim().length < 32 || isPlaceholderSecret(value))) {
                addIssue(name, 'must be a unique secret of at least 32 characters; replace the placeholder');
            }
        }
        if (values.STELLAR_NETWORK !== 'MAINNET') {
            addIssue('STELLAR_NETWORK', 'must be MAINNET when NODE_ENV is production');
        }
        if (/^sk_test_/i.test(values.PAYSTACK_SECRET_KEY ?? '')) {
            addIssue('PAYSTACK_SECRET_KEY', 'test credentials are not allowed in production');
        }
        if (/^FLWSECK_TEST-/i.test(values.FLUTTERWAVE_SECRET_KEY ?? '')) {
            addIssue('FLUTTERWAVE_SECRET_KEY', 'test credentials are not allowed in production');
        }
    }

    const stellarUrls = [values.HORIZON_URL, values.STELLAR_HORIZON_URL, values.SOROBAN_RPC_URL];
    for (const value of stellarUrls) {
        if (!value) continue;
        let parsedUrl: URL;
        try {
            parsedUrl = new URL(value);
        } catch {
            continue;
        }
        const hostname = parsedUrl.hostname.toLowerCase();
        const testnetEndpoint = hostname.includes('testnet');
        const mainnetEndpoint = hostname === 'horizon.stellar.org' || hostname === 'soroban-mainnet.stellar.org';
        if (values.STELLAR_NETWORK === 'MAINNET' && testnetEndpoint) {
            addIssue('STELLAR_NETWORK', 'MAINNET cannot use a known Stellar testnet endpoint');
        }
        if (values.STELLAR_NETWORK === 'TESTNET' && mainnetEndpoint) {
            addIssue('STELLAR_NETWORK', 'TESTNET cannot use a known Stellar mainnet endpoint');
        }
        if (values.NODE_ENV === 'production' && parsedUrl.protocol !== 'https:') {
            addIssue('STELLAR_NETWORK', 'production Stellar endpoints must use HTTPS');
        }
    }
});

// ---------------------------------------------------------------------------
// Derived helpers (computed once, not re-read from process.env)
// ---------------------------------------------------------------------------

export type Env = z.infer<typeof envSchema>;

export interface DerivedEnv extends Env {
    /** True only in production. */
    isProduction: boolean;
    /** True in staging or production. */
    isProductionLike: boolean;
    /** True only in development. */
    isDevelopment: boolean;
    /** True only in test. */
    isTest: boolean;
    /** True only in staging. */
    isStaging: boolean;
    /** Effective Horizon URL — STELLAR_HORIZON_URL takes precedence over HORIZON_URL. */
    effectiveHorizonUrl: string;
    /** Stellar network passphrase string. */
    stellarNetworkPassphrase: string;
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/**
 * Parse and validate process.env, then attach derived helpers.
 * Exits the process with code 1 on validation failure so misconfigured
 * deployments fail loudly at startup rather than silently misbehaving.
 */
export const validateEnv = (): DerivedEnv => {
    const result = envSchema.safeParse(process.env);

    if (!result.success) {
        // Use console.error here — logger is not yet initialised at this point.
        console.error(
            '[env] Environment validation failed:\n',
            JSON.stringify(result.error.format(), null, 2)
        );
        process.exit(1);
    }

    const parsed = result.data;

    const derived: DerivedEnv = {
        ...parsed,
        isProduction: parsed.NODE_ENV === 'production',
        isProductionLike:
            parsed.NODE_ENV === 'production' || parsed.NODE_ENV === 'staging',
        isDevelopment: parsed.NODE_ENV === 'development',
        isTest: parsed.NODE_ENV === 'test',
        isStaging: parsed.NODE_ENV === 'staging',
        effectiveHorizonUrl:
            parsed.STELLAR_HORIZON_URL ?? parsed.HORIZON_URL,
        stellarNetworkPassphrase:
            parsed.STELLAR_NETWORK === 'MAINNET'
                ? 'Public Global Stellar Network ; September 2015'
                : 'Test SDF Network ; September 2015',
    };

    return derived;
};

// ---------------------------------------------------------------------------
// Singleton — call validateEnv() once in server.ts; import `env` everywhere
// else.
// ---------------------------------------------------------------------------

let _env: DerivedEnv | null = null;

/**
 * Return the validated environment singleton.
 * Throws if `validateEnv()` has not been called yet (i.e. before server.ts
 * runs). This makes import-order bugs visible immediately.
 */
export const getEnv = (): DerivedEnv => {
    if (!_env) {
        throw new Error(
            '[env] getEnv() called before validateEnv(). ' +
            'Ensure server.ts calls validateEnv() at startup.'
        );
    }
    return _env;
};

/**
 * Initialise the singleton. Called once by server.ts after dotenv has loaded
 * the correct .env file.
 */
export const initEnv = (): DerivedEnv => {
    _env = validateEnv();
    return _env;
};

/** Reset the singleton — for use in tests only. */
export const resetEnv = (): void => {
    _env = null;
};
