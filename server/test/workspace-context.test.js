import test from 'node:test'
import assert from 'node:assert/strict'

const importWorkspace = () => import('../dist/services/workspace-context.service.js')
const importWorkspaceMiddleware = () => import('../dist/middleware/workspace-context.middleware.js')
const importAdminService = () => import('../dist/services/admin.service.js')
const importDatabase = () => import('../dist/services/database.service.js')

function createAdminMockDb() {
  const users = [
    { id: 'org-a-owner', email: 'owner-a@example.com', username: 'ownerA', role: 'TOURNAMENT_ORGANIZER', createdAt: new Date(), deletedAt: null },
    { id: 'org-a-viewer', email: 'viewer-a@example.com', username: 'viewerA', role: 'TOURNAMENT_ORGANIZER', createdAt: new Date(), deletedAt: null },
    { id: 'org-b-owner', email: 'owner-b@example.com', username: 'ownerB', role: 'TOURNAMENT_ORGANIZER', createdAt: new Date(), deletedAt: null },
    { id: 'player-a1', email: 'a1@example.com', username: 'playerA1', role: 'USER', createdAt: new Date(), deletedAt: null },
    { id: 'player-a2', email: 'a2@example.com', username: 'playerA2', role: 'USER', createdAt: new Date(), deletedAt: null },
    { id: 'player-b1', email: 'b1@example.com', username: 'playerB1', role: 'USER', createdAt: new Date(), deletedAt: null },
  ]
  const bans = []
  const moderationItems = [
    { id: 'mod-a1', type: 'chat', reportedUserId: 'player-a1', content: 'spam', reportedAt: new Date(), status: 'PENDING' },
    { id: 'mod-b1', type: 'chat', reportedUserId: 'player-b1', content: 'abuse', reportedAt: new Date(), status: 'PENDING' },
  ]

  return {
    _state: { users, bans, moderationItems },
    user: {
      findUnique: async ({ where }) => users.find((u) => u.id === where.id) ?? null,
      findMany: async ({ where } = {}) => {
        let rows = [...users]
        if (where?.id?.in) {
          rows = rows.filter((u) => where.id.in.includes(u.id))
        }
        return rows
      },
      count: async ({ where } = {}) => {
        let rows = [...users]
        if (where?.id?.in) {
          rows = rows.filter((u) => where.id.in.includes(u.id))
        }
        return rows.length
      },
    },
    ban: {
      create: async ({ data }) => {
        const row = { id: `ban-${bans.length + 1}`, ...data, bannedAt: new Date() }
        bans.push(row)
        return row
      },
      findFirst: async ({ where }) => bans.find((b) => b.userId === where.userId && !b.unbannedAt) ?? null,
      update: async ({ where, data }) => {
        const b = bans.find((item) => item.id === where.id)
        if (b) Object.assign(b, data)
        return b
      },
    },
    moderationItem: {
      findMany: async ({ where } = {}) => {
        let rows = moderationItems.filter((m) => !where?.status || m.status === where.status)
        if (where?.id?.in) {
          rows = rows.filter((m) => where.id.in.includes(m.id))
        }
        return rows
      },
      findUnique: async ({ where }) => moderationItems.find((m) => m.id === where.id) ?? null,
      update: async ({ where, data }) => {
        const m = moderationItems.find((item) => item.id === where.id)
        if (m) Object.assign(m, data)
        return m
      },
    },
  }
}

async function seedWorkspaces() {
  const ws = await importWorkspace()
  ws.resetWorkspaceStateForTesting()

  ws.registerWorkspaceForTesting({
    id: 'ws-alpha',
    name: 'Alpha Esports',
    slug: 'alpha-esports',
    ownerId: 'org-a-owner',
    isActive: true,
  })
  ws.registerWorkspaceForTesting({
    id: 'ws-beta',
    name: 'Beta League',
    slug: 'beta-league',
    ownerId: 'org-b-owner',
    isActive: true,
  })

  ws.registerWorkspaceMembershipForTesting({
    id: 'mem-a-owner',
    workspaceId: 'ws-alpha',
    userId: 'org-a-owner',
    role: 'OWNER',
    status: 'ACTIVE',
  })
  ws.registerWorkspaceMembershipForTesting({
    id: 'mem-a-viewer',
    workspaceId: 'ws-alpha',
    userId: 'org-a-viewer',
    role: 'VIEWER',
    status: 'ACTIVE',
  })
  ws.registerWorkspaceMembershipForTesting({
    id: 'mem-b-owner',
    workspaceId: 'ws-beta',
    userId: 'org-b-owner',
    role: 'OWNER',
    status: 'ACTIVE',
  })

  ws.registerWorkspaceResourceForTesting({ workspaceId: 'ws-alpha', resourceType: 'USER', resourceId: 'player-a1' })
  ws.registerWorkspaceResourceForTesting({ workspaceId: 'ws-alpha', resourceType: 'USER', resourceId: 'player-a2' })
  ws.registerWorkspaceResourceForTesting({ workspaceId: 'ws-beta', resourceType: 'USER', resourceId: 'player-b1' })
  ws.registerWorkspaceResourceForTesting({ workspaceId: 'ws-alpha', resourceType: 'MODERATION_ITEM', resourceId: 'mod-a1' })
  ws.registerWorkspaceResourceForTesting({ workspaceId: 'ws-beta', resourceType: 'MODERATION_ITEM', resourceId: 'mod-b1' })

  return ws
}

test('authoritative membership and role are checked on every read and mutation', async () => {
  const ws = await seedWorkspaces()
  const db = createAdminMockDb()
  const { setDatabaseClientForTesting, resetDatabaseClient } = await importDatabase()
  const { createAdminService } = await importAdminService()
  setDatabaseClientForTesting(db)

  try {
    const adminService = createAdminService()

    // VIEWER can read users within their own workspace
    const viewerCtx = await ws.workspaceContextService.resolveWorkspaceContext({
      actorId: 'org-a-viewer',
      workspaceId: 'ws-alpha',
      requiredPermission: 'users:read',
    })
    const list = await adminService.getUserList({}, viewerCtx)
    assert.deepEqual(
      list.users.map((u) => u.id).sort(),
      ['player-a1', 'player-a2']
    )

    // VIEWER cannot mutate (ban) even a user inside their own workspace
    await assert.rejects(
      () => adminService.banUser({ userId: 'player-a1', reason: 'rule violation' }, viewerCtx),
      (err) => err.statusCode === 403 && err.code === 'WORKSPACE_ROLE_INSUFFICIENT'
    )

    // Revoking membership in authoritative store immediately blocks subsequent reads
    ws.registerWorkspaceMembershipForTesting({
      id: 'mem-a-viewer',
      workspaceId: 'ws-alpha',
      userId: 'org-a-viewer',
      role: 'VIEWER',
      status: 'REVOKED',
    })
    await assert.rejects(
      () => adminService.getUserList({}, viewerCtx),
      (err) => err.statusCode === 403 && err.code === 'WORKSPACE_MEMBERSHIP_INACTIVE'
    )
  } finally {
    resetDatabaseClient()
    ws.resetWorkspaceStateForTesting()
  }
})

test('cross-workspace identifiers cannot read or mutate another organizer data, including batch operations', async () => {
  const ws = await seedWorkspaces()
  const db = createAdminMockDb()
  const { setDatabaseClientForTesting, resetDatabaseClient } = await importDatabase()
  const { createAdminService } = await importAdminService()
  setDatabaseClientForTesting(db)

  try {
    const adminService = createAdminService()
    const alphaOwnerCtx = await ws.workspaceContextService.resolveWorkspaceContext({
      actorId: 'org-a-owner',
      workspaceId: 'ws-alpha',
    })

    // Scoped moderation read only exposes ws-alpha items, never ws-beta
    const queue = await adminService.getModerationQueue(alphaOwnerCtx)
    assert.deepEqual(queue.map((item) => item.id), ['mod-a1'])

    // Single cross-workspace moderation mutation is blocked with 403
    await assert.rejects(
      () => adminService.reviewContent('mod-b1', 'remove', alphaOwnerCtx),
      (err) => err.statusCode === 403 && err.code === 'CROSS_WORKSPACE_ACCESS_DENIED'
    )

    // Batch operation mixing ws-alpha and ws-beta IDs is rejected atomically with ZERO partial bans
    await assert.rejects(
      () =>
        adminService.executeBulkUserOperation(
          {
            action: 'ban',
            userIds: ['player-a1', 'player-b1', 'player-a2'],
            reason: 'coordinated cheating',
          },
          alphaOwnerCtx
        ),
      (err) => err.statusCode === 403 && err.code === 'CROSS_WORKSPACE_ACCESS_DENIED'
    )
    assert.equal(db._state.bans.length, 0, 'no partial mutations may execute when a batch contains a cross-workspace ID')

    // Valid same-workspace batch succeeds cleanly
    const results = await adminService.executeBulkUserOperation(
      {
        action: 'ban',
        userIds: ['player-a1', 'player-a2'],
        reason: 'verified infraction',
      },
      alphaOwnerCtx
    )
    assert.deepEqual(results, { 'player-a1': true, 'player-a2': true })
    assert.equal(db._state.bans.length, 2)

    const audits = ws.getWorkspaceSecurityAuditsForTesting()
    assert.ok(audits.some((a) => a.action === 'WORKSPACE_CROSS_TENANT_BLOCKED'))
    assert.ok(audits.some((a) => a.action === 'WORKSPACE_BATCH_CROSS_TENANT_BLOCKED'))
  } finally {
    resetDatabaseClient()
    ws.resetWorkspaceStateForTesting()
  }
})

test('workspace middleware rejects conflicting workspace IDs across header and body', async () => {
  const ws = await seedWorkspaces()
  const { propagateWorkspaceContext } = await importWorkspaceMiddleware()

  const middleware = propagateWorkspaceContext()
  let capturedError = null
  const req = {
    user: { id: 'org-a-owner', role: 'TOURNAMENT_ORGANIZER' },
    headers: { 'x-workspace-id': 'ws-alpha' },
    params: {},
    query: {},
    body: { workspaceId: 'ws-beta' },
    header: () => '',
  }

  await middleware(req, {}, (err) => {
    capturedError = err ?? null
  })

  assert.ok(capturedError)
  assert.equal(capturedError.statusCode, 400)
  assert.equal(capturedError.code, 'WORKSPACE_CONTEXT_CONFLICT')
  ws.resetWorkspaceStateForTesting()
})
