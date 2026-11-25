import type { Request } from 'express'
import { createGraphQLLoaders, type GraphQLLoaders } from './loaders'
import { DEFAULT_GRAPHQL_LIMIT_POLICY } from './query-guard'

/**
 * Request-scoped context every GraphQL resolver receives. Keeping the
 * shape narrow forces resolvers to make their dependencies explicit
 * and keeps mocking trivial in tests.
 */
export interface GraphQLContext {
    /** Express request, exposed for IP / header inspection. */
    req: Request
    /** Authenticated user id, or null when the request is anonymous. */
    userId: string | null
    /** Role string carried on the auth token. */
    role: string | null
    /** Stable per-request id for tracing across the schema. */
    requestId: string
    /** Per-request batched DataLoaders & runtime fan-out tracker (#173). */
    loaders: GraphQLLoaders
}

/**
 * Build a context from an Express request. The auth/role lookup is
 * deliberately left to the caller — every existing controller already
 * derives these from the JWT middleware, so the graphql layer just
 * borrows the same values via `res.locals` to keep behaviour
 * identical to the REST surface.
 */
export function buildGraphQLContext(req: Request): GraphQLContext {
    const locals = (req as Request & { res?: { locals?: Record<string, unknown> } }).res?.locals ?? {}
    const userId = typeof locals.userId === 'string' ? locals.userId : null
    const role = typeof locals.role === 'string' ? locals.role : null
    const requestId =
        typeof locals.requestId === 'string'
            ? locals.requestId
            : (req.headers['x-request-id'] as string | undefined) ?? cryptoRequestId()
    const loaders = createGraphQLLoaders(
        DEFAULT_GRAPHQL_LIMIT_POLICY.maxBatchSize,
        DEFAULT_GRAPHQL_LIMIT_POLICY.maxFanOut
    )

    return { req, userId, role, requestId, loaders }
}

function cryptoRequestId(): string {
    // Tiny non-crypto fallback for environments without `crypto.randomUUID`.
    const segment = () => Math.floor(Math.random() * 0xffff).toString(16).padStart(4, '0')
    return `${segment()}${segment()}-${segment()}-${segment()}-${segment()}-${segment()}${segment()}${segment()}`
}
