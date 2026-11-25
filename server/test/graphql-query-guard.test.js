import test from 'node:test'
import assert from 'node:assert/strict'
import { buildSchema, parse } from 'graphql'

const importGuard = () => import('../dist/graphql/query-guard.js')
const importLoaders = () => import('../dist/graphql/loaders.js')
const importServer = () => import('../dist/graphql/server.js')

test('depth policy rejects deeply nested operations before resolver execution with zero partial work', async () => {
  const { executeGuardedGraphQL } = await importGuard()
  const schema = buildSchema(`
    type User { id: ID! recentMatches(limit: Int = 10): [Match!]! }
    type Match { id: ID! teamA: [User!]! }
    type MatchConnection { edges: [Match!]! }
    type Query { matches(limit: Int = 20): MatchConnection! }
  `)

  let resolverInvocations = 0
  schema.getQueryType().getFields().matches.resolve = () => {
    resolverInvocations += 1
    return { edges: [] }
  }

  const deepQuery = `
    query DeepNesting {
      matches(limit: 2) {
        edges {
          id
          teamA {
            id
            recentMatches(limit: 2) {
              id
              teamA {
                id
                recentMatches(limit: 2) {
                  id
                }
              }
            }
          }
        }
      }
    }
  `

  const result = await executeGuardedGraphQL({
    schema,
    source: deepQuery,
    policy: { maxDepth: 5 },
  })

  assert.equal(resolverInvocations, 0, 'oversized depth query must invoke zero resolvers')
  assert.ok(Array.isArray(result.errors) && result.errors.length === 1)
  assert.equal(result.errors[0].extensions.code, 'GRAPHQL_QUERY_DEPTH_EXCEEDED')
  assert.equal(result.errors[0].extensions.http.status, 400)
  assert.equal(result.errors[0].extensions.policy.limitType, 'depth')
})

test('cost accounting incorporates list multipliers from literal args and variables', async () => {
  const { analyzeGraphQLDocument } = await importGuard()

  const query = parse(`
    query ListMatches($limit: Int!) {
      matches(limit: $limit) {
        edges {
          id
          gameMode
          startedAt
        }
        pageInfo {
          hasNextPage
          endCursor
        }
      }
    }
  `)

  const small = analyzeGraphQLDocument(query, { limit: 5 }, 'ListMatches')
  const large = analyzeGraphQLDocument(query, { limit: 80 }, 'ListMatches')

  assert.equal(small.allowed, true)
  assert.ok(
    large.complexity > small.complexity * 5,
    `expected large limit complexity (${large.complexity}) to scale with list multiplier over small (${small.complexity})`
  )

  const overCap = analyzeGraphQLDocument(query, { limit: 500 }, 'ListMatches')
  assert.equal(overCap.allowed, false)
  assert.ok(overCap.violations.some((v) => v.code === 'GRAPHQL_LIST_LIMIT_EXCEEDED'))
})

test('cost accounting captures batched _entities resolver fan-out and rejects oversized batches before execution', async () => {
  const { analyzeGraphQLDocument, validateGraphQLHttpPayload } = await importGuard()

  const entitiesQuery = parse(`
    query FederatedEntities($representations: [_Any!]!) {
      _entities(representations: $representations) {
        ... on User {
          id
          displayName
          rating
        }
      }
    }
  `)

  const fiveReps = Array.from({ length: 5 }, (_, i) => ({ __typename: 'User', id: `u-${i}` }))
  const twentyReps = Array.from({ length: 20 }, (_, i) => ({ __typename: 'User', id: `u-${i}` }))
  const oversizedReps = Array.from({ length: 60 }, (_, i) => ({ __typename: 'User', id: `u-${i}` }))

  const a5 = analyzeGraphQLDocument(entitiesQuery, { representations: fiveReps })
  const a20 = analyzeGraphQLDocument(entitiesQuery, { representations: twentyReps })
  const a60 = analyzeGraphQLDocument(entitiesQuery, { representations: oversizedReps })

  assert.equal(a5.allowed, true)
  assert.equal(a5.batchedFieldCount, 1)
  assert.ok(a20.complexity > a5.complexity)
  assert.equal(a60.allowed, false)
  assert.ok(a60.violations.some((v) => v.code === 'GRAPHQL_BATCH_LIMIT_EXCEEDED'))

  // HTTP batch array combining individually valid operations beyond aggregate limits must be rejected upfront
  const singlePayload = {
    query: '{ matches(limit: 10) { edges { id gameMode teamA { id displayName } } } }',
  }
  const singleVerdict = validateGraphQLHttpPayload(singlePayload, {
    maxComplexity: 500,
    maxFanOut: 250,
  })
  assert.equal(singleVerdict.ok, true, 'single operation should be within policy limits')

  const batchVerdict = validateGraphQLHttpPayload(
    [singlePayload, singlePayload, singlePayload, singlePayload, singlePayload],
    { maxComplexity: 500, maxFanOut: 250 }
  )
  assert.equal(batchVerdict.ok, false)
  assert.ok(
    batchVerdict.error.extensions.code === 'GRAPHQL_QUERY_COMPLEXITY_EXCEEDED' ||
      batchVerdict.error.extensions.code === 'GRAPHQL_RESOLVER_FANOUT_EXCEEDED'
  )
})

test('GraphQL HTTP guard middleware returns stable 400 error and blocks handler execution', async () => {
  const { createGraphQLGuardMiddleware } = await importServer()
  const middleware = createGraphQLGuardMiddleware({ maxAliases: 2 })

  let nextCalled = false
  let statusCode = null
  let responseBody = null

  const req = {
    method: 'POST',
    body: {
      query: '{ a: viewer { id } b: viewer { id } c: viewer { id } }',
    },
  }
  const res = {
    status(code) {
      statusCode = code
      return this
    },
    json(body) {
      responseBody = body
    },
  }

  middleware(req, res, () => {
    nextCalled = true
  })

  assert.equal(nextCalled, false, 'next() must not run when guard rejects an operation')
  assert.equal(statusCode, 400)
  assert.equal(responseBody.errors[0].extensions.code, 'GRAPHQL_ALIAS_LIMIT_EXCEEDED')
})

test('BatchLoader deduplicates keys in one round-trip and enforces maxBatchSize', async () => {
  const { BatchLoader, ResolverFanOutTracker } = await importLoaders()
  let batchCalls = 0

  const tracker = new ResolverFanOutTracker(10)
  const loader = new BatchLoader(
    async (keys) => {
      batchCalls += 1
      const map = new Map()
      for (const k of keys) map.set(k, { id: k })
      return map
    },
    { maxBatchSize: 3, fanOutTracker: tracker }
  )

  const [r1, r2, r3] = await Promise.all([
    loader.load('u1'),
    loader.load('u2'),
    loader.load('u1'), // duplicate
  ])
  assert.equal(batchCalls, 1)
  assert.deepEqual(r1, { id: 'u1' })
  assert.deepEqual(r2, { id: 'u2' })
  assert.deepEqual(r3, { id: 'u1' })

  const strictLoader = new BatchLoader(
    async () => new Map(),
    { maxBatchSize: 2 }
  )
  await assert.rejects(
    () => Promise.all([strictLoader.load('a'), strictLoader.load('b'), strictLoader.load('c')]),
    (err) => err.extensions?.code === 'GRAPHQL_BATCH_LIMIT_EXCEEDED'
  )
})
