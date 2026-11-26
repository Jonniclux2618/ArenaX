import type { Express, RequestHandler } from 'express';
import { GraphQLSchema, parse } from 'graphql';
import { buildSubgraphSchema } from '@apollo/subgraph';
import { typeDefs } from './schema';
import { resolvers } from './resolvers';
import { buildGraphQLContext, type GraphQLContext } from './context';
import {
    DEFAULT_GRAPHQL_LIMIT_POLICY,
    createQueryGuardValidationRule,
    validateGraphQLHttpPayload,
    type GraphQLOperationPayload,
    type GraphQLQueryLimitPolicy,
} from './query-guard';

// Built once at module load: a real Apollo Federation v2 subgraph schema
// (adds `_service { sdl }` and `_entities(representations: [_Any!]!)` on
// top of the application schema). A future gateway/router composes this
// together with other subgraphs into a supergraph — see
// server/docs/GRAPHQL_FEDERATION.md.
const federatedSchema = buildSubgraphSchema({ typeDefs: parse(typeDefs), resolvers: resolvers as any });

export interface GraphQLRegistration {
    path: string
    handler: RequestHandler
}

export interface GraphQLExecutor {
    mount(app: Express): GraphQLRegistration
    stop(): Promise<void>
}

/**
 * Pre-execution Express guard for `/api/graphql` (#173).
 * Inspects single and batched GraphQL payloads before Yoga invokes any
 * resolver, returning a stable 400 GraphQL error with zero partial work.
 */
export function createGraphQLGuardMiddleware(
    policyOverrides: Partial<GraphQLQueryLimitPolicy> = {}
): RequestHandler {
    return (req, res, next) => {
        if (req.method === 'POST' && req.body) {
            const verdict = validateGraphQLHttpPayload(
                req.body as GraphQLOperationPayload | GraphQLOperationPayload[],
                policyOverrides
            );
            if (!verdict.ok) {
                res.status(400).json({
                    errors: [
                        {
                            message: verdict.error.message,
                            extensions: verdict.error.extensions,
                        },
                    ],
                });
                return;
            }
        } else if (req.method === 'GET' && typeof req.query?.query === 'string') {
            let parsedVariables: Record<string, unknown> = {};
            if (typeof req.query.variables === 'string') {
                try {
                    parsedVariables = JSON.parse(req.query.variables) as Record<string, unknown>;
                } catch {
                    // Let Yoga report malformed variables JSON.
                }
            }
            const verdict = validateGraphQLHttpPayload(
                {
                    query: req.query.query,
                    variables: parsedVariables,
                    operationName:
                        typeof req.query.operationName === 'string'
                            ? req.query.operationName
                            : null,
                },
                policyOverrides
            );
            if (!verdict.ok) {
                res.status(400).json({
                    errors: [
                        {
                            message: verdict.error.message,
                            extensions: verdict.error.extensions,
                        },
                    ],
                });
                return;
            }
        }
        next();
    };
}

function createYogaQueryGuardPlugin(
    policyOverrides: Partial<GraphQLQueryLimitPolicy> = {}
) {
    return {
        onValidate({
            params,
            addValidationRule,
        }: {
            params: { variables?: Record<string, unknown>; operationName?: string };
            addValidationRule: (rule: ReturnType<typeof createQueryGuardValidationRule>) => void;
        }) {
            addValidationRule(
                createQueryGuardValidationRule(
                    params?.variables ?? {},
                    params?.operationName ?? null,
                    policyOverrides
                )
            );
        },
    };
}

class YogaExecutor implements GraphQLExecutor {
    private yoga: any = null;

    mount(app: Express): GraphQLRegistration {
        const path = '/api/graphql';
        const guardMiddleware = createGraphQLGuardMiddleware();

        // Lazy async init — the first request triggers the dynamic import
        // so CommonJS environments can load this module without choking on
        // graphql-yoga's ESM-only distribution.
        const lazyHandler: RequestHandler = (req, res, next) => {
            guardMiddleware(req, res, () => {
                if (this.yoga) {
                    this.yoga(req, res, next);
                    return;
                }
                import('graphql-yoga').then(({ createYoga }: any) => {
                    this.yoga = createYoga({
                        schema: federatedSchema,
                        context: async ({ request }: { request: any }) =>
                            buildGraphQLContext(request as unknown as import('express').Request),
                        plugins: [createYogaQueryGuardPlugin()],
                        graphiql: process.env.NODE_ENV !== 'production',
                        graphqlEndpoint: path,
                        cors: false,
                        logging: false,
                    });
                    this.yoga(req, res, next);
                }).catch(() => {
                    res.status(503).json({
                        errors: [{ message: 'GraphQL executor not available', extensions: { code: 'GRAPHQL_UNAVAILABLE' } }],
                    });
                });
            });
        };

        app.use(path, lazyHandler);
        return { path, handler: lazyHandler };
    }

    async stop(): Promise<void> {
        // yoga doesn't require explicit cleanup for subscriptions in memory
    }
}

let activeExecutor: GraphQLExecutor = process.env.GRAPHQL_DISABLED === 'true'
    ? new (class UnconfiguredExecutor implements GraphQLExecutor {
        mount(app: Express): GraphQLRegistration {
            const path = '/api/graphql';
            const handler: RequestHandler = (req, res) => {
                const context = buildGraphQLContext(req);
                res.status(503).json({
                    errors: [{
                        message: 'GraphQL executor disabled by config',
                        extensions: { code: 'GRAPHQL_DISABLED', requestId: context.requestId },
                    }],
                });
            };
            app.post(path, handler);
            app.get(path, handler);
            return { path, handler };
        }
        async stop(): Promise<void> { /* no-op */ }
    })()
    : new YogaExecutor();

export function getGraphQLExecutor(): GraphQLExecutor {
    return activeExecutor;
}

export function setGraphQLExecutor(executor: GraphQLExecutor): void {
    activeExecutor = executor;
}

export function getGraphQLSchemaSDL(): string {
    return typeDefs;
}

export function getGraphQLSchema(): GraphQLSchema {
    return federatedSchema;
}

export function getGraphQLQueryLimitPolicy(): GraphQLQueryLimitPolicy {
    return { ...DEFAULT_GRAPHQL_LIMIT_POLICY };
}

export type { GraphQLContext };
