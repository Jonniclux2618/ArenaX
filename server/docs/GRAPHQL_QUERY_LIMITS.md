# GraphQL Query Depth, Complexity, and Resolver Fan-Out Policy

Tracks Issue #173.

Every operation sent to `/api/graphql` (single queries, mutations, subscriptions, Federation `_entities` lookups, and HTTP-batched arrays) is statically analyzed **before** resolver execution begins. Oversized operations are rejected during pre-execution validation with a stable `400` GraphQL error and **zero partial resolver execution**.

## Default Policy Limits

| Limit | Default | Environment Variable | Purpose |
| :--- | :---: | :--- | :--- |
| Max query depth | `6` | `GRAPHQL_MAX_DEPTH` | Prevents deeply nested / cyclic graph traversal (`matches -> edges -> teamA -> recentMatches -> ...`). `__typename` is depth-neutral. |
| Max weighted complexity | `500` | `GRAPHQL_MAX_COMPLEXITY` | Caps aggregate weighted field cost including list multipliers and batched resolver work. |
| Max estimated resolver fan-out | `250` | `GRAPHQL_MAX_FANOUT` | Caps estimated resolver invocations across nested lists and batched entity loads. |
| Max field aliases | `20` | `GRAPHQL_MAX_ALIASES` | Prevents alias-based amplification attacks within a single operation. |
| Max batched resolver size | `50` | `GRAPHQL_MAX_BATCH_SIZE` | Caps batched inputs such as Federation `_entities(representations: [...])` and per-request DataLoader batch dispatches. |
| Max HTTP batch operations | `10` | `GRAPHQL_MAX_HTTP_BATCH` | Caps operations per HTTP array payload; aggregate complexity and fan-out are summed across the batch. |
| Default list multiplier | `20` | `GRAPHQL_DEFAULT_LIST_MULTIPLIER` | Multiplier assumed when a paginated list field omits an explicit limit argument. |
| Max list limit argument | `100` | `GRAPHQL_MAX_LIST_LIMIT` | Hard cap on list pagination arguments (`limit`, `first`, `last`, `take`); values `< 1` or above the cap are rejected. |

## Weighted Cost & Multiplier Accounting

- **Scalar fields**: weight `1`, multiplied by the enclosing list/batch multiplier.
- **Object fields** (`Query.viewer`, `Query.user`, `Query.tournament`): weight `2`.
- **Mutations** (`joinMatchmakingQueue`, `leaveMatchmakingQueue`): weight `10`.
- **Paginated connections (`Query.matches(limit: N)`)**:
  - Base cost `5`, list multiplier `N` (default `20`, max `100`).
  - The multiplier propagates to `MatchConnection.edges` child selections (while scalar `pageInfo` is charged once).
  - Both literal AST integer arguments and GraphQL `$variables` are resolved before execution.
- **Batched resolvers (`Match.teamA`, `Match.teamB`, `User.recentMatches`, `Query._entities`)**:
  - Backed by per-request `BatchLoader` instances in `server/src/graphql/loaders.ts` that deduplicate keys and cap batch sizes.
  - Cost formula for batched fields: `batchBaseCost + batchPerItemCost * itemCount` (modeling one amortized backend round-trip plus linear per-item serialization/mapping work), while nested sub-selections still propagate list multipliers and contribute to `estimatedFanOut`.
  - `_entities(representations: $reps)` checks `representations.length <= maxBatchSize` and scales child field costs by `representations.length`.
- **Fragments**: Named fragments (`...Frag`) and inline fragments (`... on User`) are expanded accurately during analysis; circular fragment cycles are rejected with `GRAPHQL_FRAGMENT_CYCLE_DETECTED`.

## Stable Client Error Format

Rejected operations never invoke any resolver and return HTTP `400` with a deterministic machine-readable code in `extensions.code`:

- `GRAPHQL_QUERY_DEPTH_EXCEEDED`
- `GRAPHQL_QUERY_COMPLEXITY_EXCEEDED`
- `GRAPHQL_RESOLVER_FANOUT_EXCEEDED`
- `GRAPHQL_ALIAS_LIMIT_EXCEEDED`
- `GRAPHQL_BATCH_LIMIT_EXCEEDED`
- `GRAPHQL_LIST_LIMIT_EXCEEDED`
- `GRAPHQL_FRAGMENT_CYCLE_DETECTED`

Example response:

```json
{
  "errors": [
    {
      "message": "GraphQL query complexity 645 exceeds maximum allowed complexity of 500",
      "extensions": {
        "code": "GRAPHQL_QUERY_COMPLEXITY_EXCEEDED",
        "http": { "status": 400 },
        "policy": {
          "limitType": "complexity",
          "actual": 645,
          "limit": 500,
          "depth": 4,
          "maxDepth": 6,
          "complexity": 645,
          "maxComplexity": 500,
          "estimatedFanOut": 210,
          "maxFanOut": 250,
          "aliasCount": 0,
          "maxAliases": 20
        }
      }
    }
  ]
}
```
