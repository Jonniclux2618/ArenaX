import {
    DocumentNode,
    ExecutionResult,
    FieldNode,
    FragmentDefinitionNode,
    GraphQLError,
    GraphQLSchema,
    Kind,
    OperationDefinitionNode,
    SelectionSetNode,
    ValidationContext,
    ValidationRule,
    ValueNode,
    execute,
    parse,
    validate,
} from 'graphql';

/**
 * Documented GraphQL query depth, weighted-cost, list-multiplier, alias, and
 * batched resolver fan-out policy (#173).
 *
 * See `server/docs/GRAPHQL_QUERY_LIMITS.md` for full rationale and cost formulas.
 */
export interface GraphQLQueryLimitPolicy {
    /** Maximum selection nesting depth (excluding `__typename`). */
    maxDepth: number;
    /** Maximum total weighted complexity score for a single operation. */
    maxComplexity: number;
    /** Maximum estimated resolver fan-out across nested lists & batched resolvers. */
    maxFanOut: number;
    /** Maximum number of aliased field selections in an operation. */
    maxAliases: number;
    /** Maximum items permitted in a single batched resolver input (e.g. `_entities`). */
    maxBatchSize: number;
    /** Maximum operations permitted in a single HTTP GraphQL batch request array. */
    maxOperationsPerBatch: number;
    /** Default list multiplier when a paginated list field omits an explicit limit argument. */
    defaultListMultiplier: number;
    /** Hard cap on pagination limit arguments (`limit`, `first`, `take`). */
    maxListLimit: number;
}

export interface FieldCostRule {
    /** Intrinsic base cost for selecting this field once. */
    baseCost: number;
    /** Optional argument name controlling list multiplier (e.g. `limit`). */
    listMultiplierArg?: string;
    /** Default multiplier when field is a list/connection without an explicit arg. */
    defaultMultiplier?: number;
    /** Hard upper bound on the list argument value for this field. */
    maxLimit?: number;
    /** Child field name that carries the list edges (e.g. `edges` on `MatchConnection`). */
    connectionListField?: string;
    /** True when the field is backed by a batched DataLoader / Federation `_entities` resolver. */
    batched?: boolean;
    /** Optional argument name holding a batched array (e.g. `representations` on `_entities`). */
    batchInputArg?: string;
    /** Base cost for a single batched round-trip (amortized across parent items). */
    batchBaseCost?: number;
    /** Per-item marginal cost inside a batched resolver call. */
    batchPerItemCost?: number;
    /** Return type name used to look up nested field cost rules. */
    returnType?: string;
}

export type GraphQLGuardErrorCode =
    | 'GRAPHQL_QUERY_DEPTH_EXCEEDED'
    | 'GRAPHQL_QUERY_COMPLEXITY_EXCEEDED'
    | 'GRAPHQL_RESOLVER_FANOUT_EXCEEDED'
    | 'GRAPHQL_ALIAS_LIMIT_EXCEEDED'
    | 'GRAPHQL_BATCH_LIMIT_EXCEEDED'
    | 'GRAPHQL_LIST_LIMIT_EXCEEDED'
    | 'GRAPHQL_FRAGMENT_CYCLE_DETECTED';

export interface QueryLimitViolation {
    code: GraphQLGuardErrorCode;
    limitType: 'depth' | 'complexity' | 'fanOut' | 'aliases' | 'batchSize' | 'listLimit' | 'fragmentCycle';
    message: string;
    actual: number;
    limit: number;
    field?: string;
}

export interface QueryAnalysisResult {
    allowed: boolean;
    operationName: string | null;
    depth: number;
    complexity: number;
    estimatedFanOut: number;
    aliasCount: number;
    batchedFieldCount: number;
    violations: QueryLimitViolation[];
}

const parsePositiveInt = (raw: string | undefined, fallback: number): number => {
    if (!raw) return fallback;
    const parsed = Number.parseInt(raw, 10);
    return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
};

export const DEFAULT_GRAPHQL_LIMIT_POLICY: GraphQLQueryLimitPolicy = {
    maxDepth: parsePositiveInt(process.env.GRAPHQL_MAX_DEPTH, 6),
    maxComplexity: parsePositiveInt(process.env.GRAPHQL_MAX_COMPLEXITY, 500),
    maxFanOut: parsePositiveInt(process.env.GRAPHQL_MAX_FANOUT, 250),
    maxAliases: parsePositiveInt(process.env.GRAPHQL_MAX_ALIASES, 20),
    maxBatchSize: parsePositiveInt(process.env.GRAPHQL_MAX_BATCH_SIZE, 50),
    maxOperationsPerBatch: parsePositiveInt(process.env.GRAPHQL_MAX_HTTP_BATCH, 10),
    defaultListMultiplier: parsePositiveInt(process.env.GRAPHQL_DEFAULT_LIST_MULTIPLIER, 20),
    maxListLimit: parsePositiveInt(process.env.GRAPHQL_MAX_LIST_LIMIT, 100),
};

/**
 * Schema-aligned weighted field cost and multiplier table.
 * Keyed by `<ParentType>.<fieldName>`.
 */
export const FIELD_COST_POLICIES: Record<string, FieldCostRule> = {
    // Root Query fields
    'Query.viewer': { baseCost: 2, returnType: 'User' },
    'Query.user': { baseCost: 2, returnType: 'User' },
    'Query.tournament': { baseCost: 2, returnType: 'Tournament' },
    'Query.matches': {
        baseCost: 5,
        listMultiplierArg: 'limit',
        defaultMultiplier: 20,
        maxLimit: 100,
        connectionListField: 'edges',
        returnType: 'MatchConnection',
    },
    'Query._service': { baseCost: 5, returnType: '_Service' },
    'Query._entities': {
        baseCost: 5,
        batched: true,
        batchInputArg: 'representations',
        defaultMultiplier: 10,
        batchBaseCost: 5,
        batchPerItemCost: 2,
        returnType: '_Entity',
    },

    // Root Mutation fields
    'Mutation.joinMatchmakingQueue': { baseCost: 10 },
    'Mutation.leaveMatchmakingQueue': { baseCost: 10 },

    // Root Subscription fields
    'Subscription.viewerRatingChanged': { baseCost: 5, returnType: 'User' },
    'Subscription.matchEvents': { baseCost: 5, returnType: 'Match' },

    // MatchConnection fields
    'MatchConnection.edges': { baseCost: 1, returnType: 'Match' },
    'MatchConnection.pageInfo': { baseCost: 1, returnType: 'PageInfo' },

    // User fields
    'User.recentMatches': {
        baseCost: 5,
        listMultiplierArg: 'limit',
        defaultMultiplier: 10,
        maxLimit: 50,
        batched: true,
        batchBaseCost: 4,
        batchPerItemCost: 1,
        returnType: 'Match',
    },

    // Match fields (batched DataLoader resolution on teamA / teamB)
    'Match.teamA': {
        baseCost: 2,
        defaultMultiplier: 5,
        batched: true,
        batchBaseCost: 2,
        batchPerItemCost: 1,
        returnType: 'User',
    },
    'Match.teamB': {
        baseCost: 2,
        defaultMultiplier: 5,
        batched: true,
        batchBaseCost: 2,
        batchPerItemCost: 1,
        returnType: 'User',
    },

    // Introspection fields
    'Query.__schema': { baseCost: 25 },
    'Query.__type': { baseCost: 10 },
};

const LIST_ARG_NAMES = ['limit', 'first', 'last', 'take'] as const;

function resolveValueNode(
    node: ValueNode,
    variables: Record<string, unknown>
): unknown {
    switch (node.kind) {
        case Kind.INT:
            return Number.parseInt(node.value, 10);
        case Kind.FLOAT:
            return Number.parseFloat(node.value);
        case Kind.STRING:
            return node.value;
        case Kind.BOOLEAN:
            return node.value;
        case Kind.NULL:
            return null;
        case Kind.VARIABLE:
            return variables[node.name.value];
        case Kind.LIST:
            return node.values.map((item) => resolveValueNode(item, variables));
        case Kind.OBJECT: {
            const obj: Record<string, unknown> = {};
            for (const field of node.fields) {
                obj[field.name.value] = resolveValueNode(field.value, variables);
            }
            return obj;
        }
        default:
            return undefined;
    }
}

function getFieldArgumentValue(
    field: FieldNode,
    argName: string,
    variables: Record<string, unknown>
): unknown {
    const arg = field.arguments?.find((candidate) => candidate.name.value === argName);
    if (!arg) return undefined;
    return resolveValueNode(arg.value, variables);
}

function getRootTypeName(operation: OperationDefinitionNode): string {
    switch (operation.operation) {
        case 'mutation':
            return 'Mutation';
        case 'subscription':
            return 'Subscription';
        default:
            return 'Query';
    }
}

function lookupFieldRule(parentType: string, fieldName: string): FieldCostRule | undefined {
    const direct = FIELD_COST_POLICIES[`${parentType}.${fieldName}`];
    if (direct) return direct;

    // For Federation `_Entity` union selections without an explicit inline fragment,
    // fall back to matching entity field rules across User / Match / Tournament.
    if (parentType === '_Entity') {
        for (const entityType of ['User', 'Match', 'Tournament']) {
            const candidate = FIELD_COST_POLICIES[`${entityType}.${fieldName}`];
            if (candidate) return candidate;
        }
    }
    return undefined;
}

/**
 * Statically analyzes a GraphQL Document AST before any resolver executes,
 * computing selection depth, weighted complexity (with list multipliers and
 * batched resolver amortization), estimated resolver fan-out, alias count,
 * and batch/list argument bounds.
 */
export function analyzeGraphQLDocument(
    document: DocumentNode,
    variables: Record<string, unknown> = {},
    operationName?: string | null,
    policyOverrides: Partial<GraphQLQueryLimitPolicy> = {}
): QueryAnalysisResult {
    const policy: GraphQLQueryLimitPolicy = {
        ...DEFAULT_GRAPHQL_LIMIT_POLICY,
        ...policyOverrides,
    };

    const fragments = new Map<string, FragmentDefinitionNode>();
    const operations: OperationDefinitionNode[] = [];

    for (const definition of document.definitions) {
        if (definition.kind === Kind.FRAGMENT_DEFINITION) {
            fragments.set(definition.name.value, definition);
        } else if (definition.kind === Kind.OPERATION_DEFINITION) {
            operations.push(definition);
        }
    }

    const selectedOperation = operationName
        ? operations.find((op) => op.name?.value === operationName)
        : operations[0];

    if (!selectedOperation) {
        return {
            allowed: true,
            operationName: operationName ?? null,
            depth: 0,
            complexity: 0,
            estimatedFanOut: 0,
            aliasCount: 0,
            batchedFieldCount: 0,
            violations: [],
        };
    }

    let maxDepth = 0;
    let totalComplexity = 0;
    let estimatedFanOut = 0;
    let aliasCount = 0;
    let batchedFieldCount = 0;
    const violations: QueryLimitViolation[] = [];

    const visitSelectionSet = (
        selectionSet: SelectionSetNode,
        parentType: string,
        currentDepth: number,
        parentMultiplier: number,
        pendingConnectionMultiplier: number | null,
        activeFragments: Set<string>
    ): void => {
        for (const selection of selectionSet.selections) {
            if (selection.kind === Kind.FRAGMENT_SPREAD) {
                const fragName = selection.name.value;
                if (activeFragments.has(fragName)) {
                    violations.push({
                        code: 'GRAPHQL_FRAGMENT_CYCLE_DETECTED',
                        limitType: 'fragmentCycle',
                        message: `Circular fragment spread detected: ${fragName}`,
                        actual: 1,
                        limit: 0,
                        field: fragName,
                    });
                    continue;
                }
                const fragment = fragments.get(fragName);
                if (fragment) {
                    const nextActive = new Set(activeFragments);
                    nextActive.add(fragName);
                    const fragType = fragment.typeCondition.name.value || parentType;
                    visitSelectionSet(
                        fragment.selectionSet,
                        fragType,
                        currentDepth,
                        parentMultiplier,
                        pendingConnectionMultiplier,
                        nextActive
                    );
                }
                continue;
            }

            if (selection.kind === Kind.INLINE_FRAGMENT) {
                const inlineType = selection.typeCondition?.name.value ?? parentType;
                visitSelectionSet(
                    selection.selectionSet,
                    inlineType,
                    currentDepth,
                    parentMultiplier,
                    pendingConnectionMultiplier,
                    activeFragments
                );
                continue;
            }

            // FieldNode
            const fieldName = selection.name.value;
            if (fieldName === '__typename') {
                continue;
            }

            if (selection.alias && selection.alias.value !== fieldName) {
                aliasCount += 1;
            }

            const fieldDepth = currentDepth + 1;
            if (fieldDepth > maxDepth) {
                maxDepth = fieldDepth;
            }

            const rule = lookupFieldRule(parentType, fieldName);
            const hasSubSelections = Boolean(selection.selectionSet && selection.selectionSet.selections.length > 0);

            // Determine list or batch multiplier from explicit argument, variables, or policy default.
            let fieldMultiplier = 1;
            let nextPendingConnectionMultiplier: number | null = null;

            if (rule?.batchInputArg) {
                const rawBatch = getFieldArgumentValue(selection, rule.batchInputArg, variables);
                if (Array.isArray(rawBatch)) {
                    if (rawBatch.length > policy.maxBatchSize) {
                        violations.push({
                            code: 'GRAPHQL_BATCH_LIMIT_EXCEEDED',
                            limitType: 'batchSize',
                            message: `Batched input "${rule.batchInputArg}" on ${parentType}.${fieldName} exceeds limit (${rawBatch.length} > ${policy.maxBatchSize})`,
                            actual: rawBatch.length,
                            limit: policy.maxBatchSize,
                            field: `${parentType}.${fieldName}`,
                        });
                    }
                    fieldMultiplier = Math.max(1, rawBatch.length);
                } else {
                    fieldMultiplier = rule.defaultMultiplier ?? 1;
                }
            } else {
                const listArgName =
                    rule?.listMultiplierArg ??
                    LIST_ARG_NAMES.find((argName) =>
                        selection.arguments?.some((a) => a.name.value === argName)
                    );

                if (listArgName) {
                    const rawLimit = getFieldArgumentValue(selection, listArgName, variables);
                    const fieldMaxLimit = Math.min(rule?.maxLimit ?? policy.maxListLimit, policy.maxListLimit);
                    if (typeof rawLimit === 'number' && Number.isFinite(rawLimit)) {
                        if (rawLimit < 1 || rawLimit > fieldMaxLimit) {
                            violations.push({
                                code: 'GRAPHQL_LIST_LIMIT_EXCEEDED',
                                limitType: 'listLimit',
                                message: `List argument "${listArgName}" on ${parentType}.${fieldName} must be between 1 and ${fieldMaxLimit} (received ${rawLimit})`,
                                actual: rawLimit,
                                limit: fieldMaxLimit,
                                field: `${parentType}.${fieldName}`,
                            });
                        }
                        fieldMultiplier = Math.max(1, Math.min(Math.floor(rawLimit), fieldMaxLimit));
                    } else {
                        fieldMultiplier = rule?.defaultMultiplier ?? policy.defaultListMultiplier;
                    }
                } else if (rule?.defaultMultiplier) {
                    fieldMultiplier = rule.defaultMultiplier;
                }
            }

            // Account for connection wrappers (e.g., `matches(limit: 50) { edges { ... } }`):
            // the multiplier applies to `edges`, not to scalar `pageInfo`.
            let childMultiplier = parentMultiplier;
            if (rule?.connectionListField) {
                nextPendingConnectionMultiplier = fieldMultiplier;
            } else if (pendingConnectionMultiplier !== null && fieldName === 'edges') {
                childMultiplier = parentMultiplier * pendingConnectionMultiplier;
            } else if (fieldMultiplier > 1) {
                childMultiplier = parentMultiplier * fieldMultiplier;
            }

            // Compute weighted cost & resolver fan-out with explicit batching accounting.
            if (rule?.batched) {
                batchedFieldCount += 1;
                const batchBase = rule.batchBaseCost ?? rule.baseCost;
                const perItem = rule.batchPerItemCost ?? 1;
                const itemCount = rule.batchInputArg ? fieldMultiplier : parentMultiplier;
                // Batched resolver: one amortized round-trip + linear per-item cost,
                // rather than unbatched full baseCost * parentMultiplier.
                totalComplexity += batchBase + perItem * itemCount;
                estimatedFanOut += itemCount * (rule.batchInputArg ? 1 : fieldMultiplier);
            } else {
                const baseCost = rule?.baseCost ?? (hasSubSelections ? 2 : 1);
                totalComplexity += baseCost * parentMultiplier;
                if (hasSubSelections || rule?.listMultiplierArg || rule?.defaultMultiplier) {
                    estimatedFanOut += parentMultiplier * (rule?.connectionListField ? 1 : fieldMultiplier);
                }
            }

            if (selection.selectionSet) {
                const nextType = rule?.returnType ?? parentType;
                visitSelectionSet(
                    selection.selectionSet,
                    nextType,
                    fieldDepth,
                    childMultiplier,
                    nextPendingConnectionMultiplier,
                    activeFragments
                );
            }
        }
    };

    visitSelectionSet(
        selectedOperation.selectionSet,
        getRootTypeName(selectedOperation),
        0,
        1,
        null,
        new Set()
    );

    if (maxDepth > policy.maxDepth) {
        violations.push({
            code: 'GRAPHQL_QUERY_DEPTH_EXCEEDED',
            limitType: 'depth',
            message: `GraphQL query depth ${maxDepth} exceeds maximum allowed depth of ${policy.maxDepth}`,
            actual: maxDepth,
            limit: policy.maxDepth,
        });
    }

    if (aliasCount > policy.maxAliases) {
        violations.push({
            code: 'GRAPHQL_ALIAS_LIMIT_EXCEEDED',
            limitType: 'aliases',
            message: `GraphQL alias count ${aliasCount} exceeds maximum allowed aliases of ${policy.maxAliases}`,
            actual: aliasCount,
            limit: policy.maxAliases,
        });
    }

    if (totalComplexity > policy.maxComplexity) {
        violations.push({
            code: 'GRAPHQL_QUERY_COMPLEXITY_EXCEEDED',
            limitType: 'complexity',
            message: `GraphQL query complexity ${totalComplexity} exceeds maximum allowed complexity of ${policy.maxComplexity}`,
            actual: totalComplexity,
            limit: policy.maxComplexity,
        });
    }

    if (estimatedFanOut > policy.maxFanOut) {
        violations.push({
            code: 'GRAPHQL_RESOLVER_FANOUT_EXCEEDED',
            limitType: 'fanOut',
            message: `GraphQL estimated resolver fan-out ${estimatedFanOut} exceeds maximum allowed fan-out of ${policy.maxFanOut}`,
            actual: estimatedFanOut,
            limit: policy.maxFanOut,
        });
    }

    return {
        allowed: violations.length === 0,
        operationName: selectedOperation.name?.value ?? operationName ?? null,
        depth: maxDepth,
        complexity: totalComplexity,
        estimatedFanOut,
        aliasCount,
        batchedFieldCount,
        violations,
    };
}

/**
 * Builds a deterministic, client-safe `GraphQLError` from a failed analysis.
 */
export function createQueryGuardError(
    analysis: QueryAnalysisResult,
    policyOverrides: Partial<GraphQLQueryLimitPolicy> = {}
): GraphQLError {
    const policy: GraphQLQueryLimitPolicy = {
        ...DEFAULT_GRAPHQL_LIMIT_POLICY,
        ...policyOverrides,
    };
    const primary = analysis.violations[0] ?? {
        code: 'GRAPHQL_QUERY_COMPLEXITY_EXCEEDED' as GraphQLGuardErrorCode,
        limitType: 'complexity' as const,
        message: 'GraphQL query rejected by complexity policy',
        actual: analysis.complexity,
        limit: policy.maxComplexity,
    };

    return new GraphQLError(primary.message, {
        extensions: {
            code: primary.code,
            http: { status: 400 },
            policy: {
                limitType: primary.limitType,
                actual: primary.actual,
                limit: primary.limit,
                depth: analysis.depth,
                maxDepth: policy.maxDepth,
                complexity: analysis.complexity,
                maxComplexity: policy.maxComplexity,
                estimatedFanOut: analysis.estimatedFanOut,
                maxFanOut: policy.maxFanOut,
                aliasCount: analysis.aliasCount,
                maxAliases: policy.maxAliases,
                violations: analysis.violations.map((v) => ({
                    code: v.code,
                    limitType: v.limitType,
                    actual: v.actual,
                    limit: v.limit,
                    field: v.field,
                })),
            },
        },
    });
}

/**
 * Throws a stable `GraphQLError` if the operation violates any depth,
 * complexity, list multiplier, alias, or batched fan-out bound.
 */
export function assertGraphQLOperationWithinLimits(
    document: DocumentNode,
    variables: Record<string, unknown> = {},
    operationName?: string | null,
    policyOverrides: Partial<GraphQLQueryLimitPolicy> = {}
): QueryAnalysisResult {
    const analysis = analyzeGraphQLDocument(
        document,
        variables,
        operationName,
        policyOverrides
    );
    if (!analysis.allowed) {
        throw createQueryGuardError(analysis, policyOverrides);
    }
    return analysis;
}

/**
 * Standard GraphQL `ValidationRule` for use with `graphql`'s `validate()`
 * and `graphql-yoga`'s `onValidate` hook so rejected queries terminate before
 * execution starts — guaranteeing zero partial resolver work.
 */
export function createQueryGuardValidationRule(
    variables: Record<string, unknown> = {},
    operationName?: string | null,
    policyOverrides: Partial<GraphQLQueryLimitPolicy> = {}
): ValidationRule {
    return (context: ValidationContext) => {
        const analysis = analyzeGraphQLDocument(
            context.getDocument(),
            variables,
            operationName,
            policyOverrides
        );
        if (!analysis.allowed) {
            context.reportError(createQueryGuardError(analysis, policyOverrides));
        }
        return {};
    };
}

export interface GraphQLOperationPayload {
    query?: string;
    variables?: Record<string, unknown>;
    operationName?: string | null;
}

/**
 * Validates either a single GraphQL HTTP payload or an HTTP batch array of
 * operations BEFORE any execution begins, summing aggregate complexity and
 * fan-out across the batch so attackers cannot bypass limits via batching.
 */
export function validateGraphQLHttpPayload(
    payload: GraphQLOperationPayload | GraphQLOperationPayload[],
    policyOverrides: Partial<GraphQLQueryLimitPolicy> = {}
): { ok: true; analyses: QueryAnalysisResult[] } | { ok: false; error: GraphQLError } {
    const policy: GraphQLQueryLimitPolicy = {
        ...DEFAULT_GRAPHQL_LIMIT_POLICY,
        ...policyOverrides,
    };

    const items = Array.isArray(payload) ? payload : [payload];
    if (items.length > policy.maxOperationsPerBatch) {
        return {
            ok: false,
            error: new GraphQLError(
                `GraphQL HTTP batch size ${items.length} exceeds maximum allowed operations of ${policy.maxOperationsPerBatch}`,
                {
                    extensions: {
                        code: 'GRAPHQL_BATCH_LIMIT_EXCEEDED',
                        http: { status: 400 },
                        policy: {
                            limitType: 'batchSize',
                            actual: items.length,
                            limit: policy.maxOperationsPerBatch,
                        },
                    },
                }
            ),
        };
    }

    const analyses: QueryAnalysisResult[] = [];
    let combinedComplexity = 0;
    let combinedFanOut = 0;

    for (const item of items) {
        if (!item || typeof item.query !== 'string' || !item.query.trim()) {
            continue;
        }
        let document: DocumentNode;
        try {
            document = parse(item.query);
        } catch {
            // Syntax errors are left to standard GraphQL parsing error formatting.
            continue;
        }
        const analysis = analyzeGraphQLDocument(
            document,
            item.variables ?? {},
            item.operationName ?? null,
            policy
        );
        if (!analysis.allowed) {
            return {
                ok: false,
                error: createQueryGuardError(analysis, policy),
            };
        }
        analyses.push(analysis);
        combinedComplexity += analysis.complexity;
        combinedFanOut += analysis.estimatedFanOut;
    }

    if (items.length > 1 && combinedComplexity > policy.maxComplexity) {
        return {
            ok: false,
            error: new GraphQLError(
                `Batched GraphQL complexity ${combinedComplexity} exceeds maximum allowed complexity of ${policy.maxComplexity}`,
                {
                    extensions: {
                        code: 'GRAPHQL_QUERY_COMPLEXITY_EXCEEDED',
                        http: { status: 400 },
                        policy: {
                            limitType: 'complexity',
                            actual: combinedComplexity,
                            limit: policy.maxComplexity,
                        },
                    },
                }
            ),
        };
    }

    if (items.length > 1 && combinedFanOut > policy.maxFanOut) {
        return {
            ok: false,
            error: new GraphQLError(
                `Batched GraphQL estimated fan-out ${combinedFanOut} exceeds maximum allowed fan-out of ${policy.maxFanOut}`,
                {
                    extensions: {
                        code: 'GRAPHQL_RESOLVER_FANOUT_EXCEEDED',
                        http: { status: 400 },
                        policy: {
                            limitType: 'fanOut',
                            actual: combinedFanOut,
                            limit: policy.maxFanOut,
                        },
                    },
                }
            ),
        };
    }

    return { ok: true, analyses };
}

/**
 * Executes a GraphQL query against a schema ONLY after passing both standard
 * GraphQL validation and the depth/complexity/fan-out guard. When rejected,
 * returns `{ errors: [GraphQLError] }` without invoking any resolver.
 */
export async function executeGuardedGraphQL(params: {
    schema: GraphQLSchema;
    source: string;
    variableValues?: Record<string, unknown>;
    operationName?: string | null;
    contextValue?: unknown;
    policy?: Partial<GraphQLQueryLimitPolicy>;
}): Promise<ExecutionResult> {
    const document = parse(params.source);
    const variables = params.variableValues ?? {};

    const analysis = analyzeGraphQLDocument(
        document,
        variables,
        params.operationName ?? null,
        params.policy
    );
    if (!analysis.allowed) {
        return {
            errors: [createQueryGuardError(analysis, params.policy)],
        };
    }

    const validationErrors = validate(params.schema, document);
    if (validationErrors.length > 0) {
        return { errors: validationErrors };
    }

    return execute({
        schema: params.schema,
        document,
        variableValues: variables,
        operationName: params.operationName ?? undefined,
        contextValue: params.contextValue,
    });
}
