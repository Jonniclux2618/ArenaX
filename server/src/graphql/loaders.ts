import { GraphQLError } from 'graphql';
import { getDatabaseClient } from '../services/database.service';

export interface BatchLoaderOptions {
    maxBatchSize?: number;
    fanOutTracker?: ResolverFanOutTracker;
}

/**
 * Tracks cumulative batched and unbatched resolver invocations within a
 * single GraphQL request as defense-in-depth behind static query validation.
 */
export class ResolverFanOutTracker {
    private count = 0;

    constructor(public readonly maxFanOut: number) {}

    record(units = 1): void {
        this.count += units;
        if (this.count > this.maxFanOut) {
            throw new GraphQLError(
                `GraphQL resolver fan-out limit exceeded (${this.count} > ${this.maxFanOut})`,
                {
                    extensions: {
                        code: 'GRAPHQL_RESOLVER_FANOUT_EXCEEDED',
                        http: { status: 400 },
                        policy: {
                            limitType: 'fanOut',
                            actual: this.count,
                            limit: this.maxFanOut,
                        },
                    },
                }
            );
        }
    }

    getCount(): number {
        return this.count;
    }
}

interface PendingEntry<K, V> {
    key: K;
    resolve: (value: V | null) => void;
    reject: (error: unknown) => void;
}

/**
 * Zero-dependency per-request DataLoader-style batcher.
 * Deduplicates keys scheduled in the same microtask tick and dispatches
 * them in a single batched backend call while enforcing `maxBatchSize`.
 */
export class BatchLoader<K extends string, V> {
    private readonly cache = new Map<K, Promise<V | null>>();
    private queue: PendingEntry<K, V>[] = [];
    private scheduled = false;
    private readonly maxBatchSize: number;
    private readonly fanOutTracker?: ResolverFanOutTracker;

    constructor(
        private readonly batchFn: (keys: readonly K[]) => Promise<Map<K, V>>,
        options: BatchLoaderOptions = {}
    ) {
        this.maxBatchSize = options.maxBatchSize ?? 50;
        this.fanOutTracker = options.fanOutTracker;
    }

    load(key: K): Promise<V | null> {
        const cached = this.cache.get(key);
        if (cached) {
            return cached;
        }

        const promise = new Promise<V | null>((resolve, reject) => {
            this.queue.push({ key, resolve, reject });
            if (!this.scheduled) {
                this.scheduled = true;
                queueMicrotask(() => {
                    void this.dispatch();
                });
            }
        });

        this.cache.set(key, promise);
        return promise;
    }

    private async dispatch(): Promise<void> {
        const batch = this.queue;
        this.queue = [];
        this.scheduled = false;

        if (batch.length === 0) {
            return;
        }

        if (batch.length > this.maxBatchSize) {
            const error = new GraphQLError(
                `GraphQL batched resolver size exceeded (${batch.length} > ${this.maxBatchSize})`,
                {
                    extensions: {
                        code: 'GRAPHQL_BATCH_LIMIT_EXCEEDED',
                        http: { status: 400 },
                        policy: {
                            limitType: 'batchSize',
                            actual: batch.length,
                            limit: this.maxBatchSize,
                        },
                    },
                }
            );
            for (const entry of batch) {
                entry.reject(error);
            }
            return;
        }

        try {
            // A batched call accounts for 1 backend round-trip plus amortized item fan-out.
            this.fanOutTracker?.record(batch.length);
            const keys = batch.map((item) => item.key);
            const results = await this.batchFn(keys);
            for (const entry of batch) {
                entry.resolve(results.get(entry.key) ?? null);
            }
        } catch (error) {
            for (const entry of batch) {
                entry.reject(error);
            }
        }
    }
}

export interface LoadedUser {
    id: string;
    displayName: string;
    rating: number;
    createdAt: Date;
}

export interface LoadedMatch {
    id: string;
    gameMode: string;
    teamA: LoadedUser[];
    teamB: LoadedUser[];
    startedAt: Date;
}

export interface LoadedTournament {
    id: string;
    name: string;
    startsAt: Date;
    endsAt: Date | null;
    participantCount: number;
}

export interface GraphQLLoaders {
    fanOutTracker: ResolverFanOutTracker;
    userById: BatchLoader<string, LoadedUser>;
    matchById: BatchLoader<string, LoadedMatch>;
    tournamentById: BatchLoader<string, LoadedTournament>;
}

export function createGraphQLLoaders(
    maxBatchSize = 50,
    maxFanOut = 250
): GraphQLLoaders {
    const fanOutTracker = new ResolverFanOutTracker(maxFanOut);

    const userById = new BatchLoader<string, LoadedUser>(
        async (ids) => {
            const prisma = getDatabaseClient() as any;
            const rows: any[] = prisma?.user?.findMany
                ? await prisma.user.findMany({
                      where: { id: { in: [...ids] } },
                      select: { id: true, username: true, createdAt: true },
                  })
                : [];
            const map = new Map<string, LoadedUser>();
            for (const row of rows) {
                map.set(row.id, {
                    id: row.id,
                    displayName: row.username,
                    rating: 1000,
                    createdAt: row.createdAt,
                });
            }
            return map;
        },
        { maxBatchSize, fanOutTracker }
    );

    const matchById = new BatchLoader<string, LoadedMatch>(
        async (ids) => {
            const prisma = getDatabaseClient() as any;
            const rows: any[] = prisma?.match?.findMany
                ? await prisma.match.findMany({
                      where: { id: { in: [...ids] } },
                  })
                : [];
            const map = new Map<string, LoadedMatch>();
            for (const row of rows) {
                map.set(row.id, {
                    id: row.id,
                    gameMode: row.metadata?.gameMode || 'unknown',
                    teamA: [],
                    teamB: [],
                    startedAt: row.createdAt,
                });
            }
            return map;
        },
        { maxBatchSize, fanOutTracker }
    );

    const tournamentById = new BatchLoader<string, LoadedTournament>(
        async (ids) => {
            const prisma = getDatabaseClient() as any;
            const rows: any[] = prisma?.tournament?.findMany
                ? await prisma.tournament.findMany({
                      where: { id: { in: [...ids] } },
                      select: {
                          id: true,
                          name: true,
                          startDate: true,
                          endDate: true,
                          _count: { select: { participants: true } },
                      },
                  })
                : [];
            const map = new Map<string, LoadedTournament>();
            for (const row of rows) {
                map.set(row.id, {
                    id: row.id,
                    name: row.name,
                    startsAt: row.startDate,
                    endsAt: row.endDate,
                    participantCount: row._count?.participants ?? 0,
                });
            }
            return map;
        },
        { maxBatchSize, fanOutTracker }
    );

    return {
        fanOutTracker,
        userById,
        matchById,
        tournamentById,
    };
}
