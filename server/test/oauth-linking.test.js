import test from 'node:test'
import assert from 'node:assert/strict'
import crypto from 'node:crypto'

process.env.NODE_ENV = 'test'

const importOAuth = () => import('../dist/services/oauth-identity.service.js')
const importDatabase = () => import('../dist/services/database.service.js')

function createOAuthMockDb() {
  const users = []
  const refreshTokens = []
  const userWallets = []

  const db = {
    _state: { users, refreshTokens, userWallets },
    user: {
      findUnique: async ({ where }) =>
        users.find((u) => (where.id ? u.id === where.id : u.email === where.email)) ?? null,
      findFirst: async ({ where }) =>
        users.find(
          (u) =>
            (!where.provider || u.provider === where.provider) &&
            (!where.providerId || u.providerId === where.providerId)
        ) ?? null,
      create: async ({ data }) => {
        const user = {
          id: crypto.randomUUID(),
          email: data.email,
          username: data.username,
          passwordHash: data.passwordHash ?? '',
          role: data.role ?? 'USER',
          isVerified: Boolean(data.isVerified),
          provider: data.provider ?? 'EMAIL',
          providerId: data.providerId ?? null,
          bio: null,
          socials: null,
          createdAt: new Date(),
          updatedAt: new Date(),
        }
        users.push(user)
        return { ...user }
      },
      update: async ({ where, data }) => {
        const user = users.find((u) => u.id === where.id)
        if (!user) throw new Error('User not found')
        Object.assign(user, data, { updatedAt: new Date() })
        return { ...user }
      },
    },
    refreshToken: {
      updateMany: async ({ where, data }) => {
        let count = 0
        for (const rt of refreshTokens) {
          if (rt.userId === where.userId && rt.revokedAt === null) {
            Object.assign(rt, data)
            count += 1
          }
        }
        return { count }
      },
    },
    userWallet: {
      create: async ({ data }) => {
        userWallets.push(data)
        return data
      },
    },
    $transaction: async (cb) => cb(db),
  }

  return db
}

test('OAuth login with matching email NEVER silently links or grants access to an existing ArenaX account', async () => {
  const oauth = await importOAuth()
  const { setDatabaseClientForTesting, resetDatabaseClient } = await importDatabase()
  oauth.resetOAuthStateForTesting()

  const db = createOAuthMockDb()
  setDatabaseClientForTesting(db)

  try {
    const existing = await db.user.create({
      data: {
        email: 'victim@example.com',
        username: 'victim',
        passwordHash: '$2b$12$hashedpasswordplaceholder',
      },
    })

    await assert.rejects(
      () =>
        oauth.resolveSafeOAuthLogin({
          provider: 'GOOGLE',
          providerId: 'google-sub-999',
          email: 'victim@example.com',
          emailVerified: true,
          username: 'attacker_google',
        }),
      (err) => err.statusCode === 409 && err.code === 'OAUTH_ACCOUNT_CONFLICT'
    )

    // Verify existing account was not modified and conflict was audited
    const stored = await db.user.findUnique({ where: { id: existing.id } })
    assert.equal(stored.provider, 'EMAIL')
    assert.equal(stored.providerId, null)

    const audits = oauth.getOAuthAuditRecordsForTesting()
    assert.ok(
      audits.some(
        (a) =>
          a.action === 'AUTH_OAUTH_LINK_CONFLICT' &&
          a.details.conflictType === 'EMAIL_MATCH_UNLINKED_ACCOUNT'
      )
    )
  } finally {
    resetDatabaseClient()
    oauth.resetOAuthStateForTesting()
  }
})

test('linking requires an authenticated ArenaX session plus fresh external proof and blocks replays/session mismatches', async () => {
  const oauth = await importOAuth()
  const { setDatabaseClientForTesting, resetDatabaseClient } = await importDatabase()
  oauth.resetOAuthStateForTesting()

  const db = createOAuthMockDb()
  setDatabaseClientForTesting(db)

  try {
    const userA = await db.user.create({
      data: {
        email: 'alice@example.com',
        username: 'alice',
        passwordHash: '$2b$12$hashedpasswordplaceholder',
      },
    })
    const userB = await db.user.create({
      data: {
        email: 'bob@example.com',
        username: 'bob',
        passwordHash: '$2b$12$hashedpasswordplaceholder',
      },
    })

    oauth.setOAuthProviderVerifierForTesting(async (proof) => {
      if (proof.accessToken === 'fresh-discord-token-alice') {
        return {
          provider: 'DISCORD',
          providerId: 'discord-alice-1',
          email: 'alice@example.com',
          emailVerified: true,
          username: 'alice_dc',
        }
      }
      throw Object.assign(new Error('Invalid provider token'), { statusCode: 401 })
    })

    // Missing ArenaX session token -> rejected
    await assert.rejects(
      () =>
        oauth.initiateOAuthLinkChallenge({
          userId: userA.id,
          provider: 'discord',
          sessionToken: undefined,
        }),
      (err) => err.statusCode === 401 && err.code === 'OAUTH_SESSION_REQUIRED'
    )

    // Issue challenge bound to Alice's session
    const challenge = await oauth.initiateOAuthLinkChallenge({
      userId: userA.id,
      provider: 'discord',
      sessionToken: 'Bearer session-token-alice',
    })

    // Completing from a different session token fails
    await assert.rejects(
      () =>
        oauth.completeOAuthLink({
          userId: userA.id,
          provider: 'discord',
          state: challenge.state,
          nonce: challenge.nonce,
          sessionToken: 'Bearer different-session-token',
          accessToken: 'fresh-discord-token-alice',
        }),
      (err) => err.statusCode === 403 && err.code === 'OAUTH_LINK_SESSION_MISMATCH'
    )

    // Completing with matching session + fresh external proof succeeds
    const linked = await oauth.completeOAuthLink({
      userId: userA.id,
      provider: 'discord',
      state: challenge.state,
      nonce: challenge.nonce,
      sessionToken: 'Bearer session-token-alice',
      accessToken: 'fresh-discord-token-alice',
    })
    assert.equal(linked.provider, 'DISCORD')
    assert.equal(linked.providerId, 'discord-alice-1')
    assert.equal(linked.status, 'ACTIVE')

    // Replaying the same challenge state fails
    await assert.rejects(
      () =>
        oauth.completeOAuthLink({
          userId: userA.id,
          provider: 'discord',
          state: challenge.state,
          nonce: challenge.nonce,
          sessionToken: 'Bearer session-token-alice',
          accessToken: 'fresh-discord-token-alice',
        }),
      (err) => err.statusCode === 401 && err.code === 'OAUTH_LINK_CHALLENGE_REPLAYED'
    )

    // Bob trying to link Alice's already-linked Discord identity fails with 409 and is audited
    const bobChallenge = await oauth.initiateOAuthLinkChallenge({
      userId: userB.id,
      provider: 'discord',
      sessionToken: 'Bearer session-token-bob',
    })
    await assert.rejects(
      () =>
        oauth.completeOAuthLink({
          userId: userB.id,
          provider: 'discord',
          state: bobChallenge.state,
          nonce: bobChallenge.nonce,
          sessionToken: 'Bearer session-token-bob',
          accessToken: 'fresh-discord-token-alice',
        }),
      (err) => err.statusCode === 409 && err.code === 'OAUTH_IDENTITY_CONFLICT'
    )

    const audits = oauth.getOAuthAuditRecordsForTesting()
    assert.ok(audits.some((a) => a.action === 'AUTH_OAUTH_LINKED'))
    assert.ok(
      audits.some(
        (a) =>
          a.action === 'AUTH_OAUTH_LINK_CONFLICT' &&
          a.details.conflictType === 'IDENTITY_ALREADY_LINKED_TO_OTHER_ACCOUNT'
      )
    )
  } finally {
    resetDatabaseClient()
    oauth.resetOAuthStateForTesting()
  }
})

test('unlinking, provider revocation, and recovery are audited and never grant access without proof', async () => {
  const oauth = await importOAuth()
  const { setDatabaseClientForTesting, resetDatabaseClient } = await importDatabase()
  oauth.resetOAuthStateForTesting()

  const db = createOAuthMockDb()
  setDatabaseClientForTesting(db)

  try {
    // OAuth-only user (no password)
    const oauthOnlyUser = await db.user.create({
      data: {
        email: 'oauthonly@example.com',
        username: 'oauthonly',
        passwordHash: '',
      },
    })
    db._state.refreshTokens.push({
      id: 'rt-1',
      userId: oauthOnlyUser.id,
      revokedAt: null,
    })

    oauth.setOAuthProviderVerifierForTesting(async () => ({
      provider: 'TWITCH',
      providerId: 'twitch-42',
      email: 'oauthonly@example.com',
      emailVerified: true,
      username: 'oauthonly_tw',
    }))

    const ch = await oauth.initiateOAuthLinkChallenge({
      userId: oauthOnlyUser.id,
      provider: 'twitch',
      sessionToken: 'Bearer sess-1',
    })
    await oauth.completeOAuthLink({
      userId: oauthOnlyUser.id,
      provider: 'twitch',
      state: ch.state,
      nonce: ch.nonce,
      sessionToken: 'Bearer sess-1',
      accessToken: 'fresh-twitch-token',
    })

    // Refuses to unlink last sign-in method (no password & no other provider)
    await assert.rejects(
      () =>
        oauth.unlinkOAuthIdentity({
          userId: oauthOnlyUser.id,
          provider: 'twitch',
        }),
      (err) => err.statusCode === 400 && err.code === 'OAUTH_LAST_LOGIN_METHOD'
    )

    // Provider revocation marks identity REVOKED, revokes refresh tokens, and blocks silent login
    const revoked = await oauth.handleProviderRevocation({
      provider: 'twitch',
      providerId: 'twitch-42',
      actorUserId: oauthOnlyUser.id,
      reason: 'USER_REVOKED_AT_PROVIDER',
    })
    assert.equal(revoked.revoked, true)
    assert.ok(db._state.refreshTokens[0].revokedAt instanceof Date)

    await assert.rejects(
      () =>
        oauth.resolveSafeOAuthLogin({
          provider: 'TWITCH',
          providerId: 'twitch-42',
          email: 'oauthonly@example.com',
          emailVerified: true,
          username: 'oauthonly_tw',
        }),
      (err) => err.statusCode === 401 && err.code === 'OAUTH_IDENTITY_REVOKED'
    )

    // Audited recovery: wrong code MUST fail and never grant access
    const recovery = await oauth.initiateOAuthRecovery({
      email: 'oauthonly@example.com',
      provider: 'twitch',
    })
    assert.ok(recovery.challengeId)
    assert.ok(recovery.recoveryCode)

    await assert.rejects(
      () =>
        oauth.completeOAuthRecovery({
          challengeId: recovery.challengeId,
          recoveryCode: 'wrong-proof-code',
          newPassword: 'newStrongPassword123',
        }),
      (err) => err.statusCode === 401 && err.code === 'OAUTH_RECOVERY_PROOF_INVALID'
    )

    // Valid recovery proof completes recovery, sets password, and audits completion
    const completed = await oauth.completeOAuthRecovery({
      challengeId: recovery.challengeId,
      recoveryCode: recovery.recoveryCode,
      newPassword: 'newStrongPassword123',
    })
    assert.equal(completed.recovered, true)
    assert.equal(completed.userId, oauthOnlyUser.id)

    const updatedUser = await db.user.findUnique({ where: { id: oauthOnlyUser.id } })
    assert.ok(updatedUser.passwordHash && updatedUser.passwordHash.length > 20)

    // Now that the account has a password, linking & unlinking a provider succeeds and is audited
    const ch2 = await oauth.initiateOAuthLinkChallenge({
      userId: oauthOnlyUser.id,
      provider: 'twitch',
      sessionToken: 'Bearer sess-2',
    })
    await oauth.completeOAuthLink({
      userId: oauthOnlyUser.id,
      provider: 'twitch',
      state: ch2.state,
      nonce: ch2.nonce,
      sessionToken: 'Bearer sess-2',
      accessToken: 'fresh-twitch-token',
    })
    const unlinkResult = await oauth.unlinkOAuthIdentity({
      userId: oauthOnlyUser.id,
      provider: 'twitch',
    })
    assert.equal(unlinkResult.unlinked, true)

    const audits = oauth.getOAuthAuditRecordsForTesting()
    assert.ok(audits.some((a) => a.action === 'AUTH_OAUTH_UNLINK_DENIED'))
    assert.ok(audits.some((a) => a.action === 'AUTH_OAUTH_UNLINKED'))
    assert.ok(audits.some((a) => a.action === 'AUTH_OAUTH_PROVIDER_REVOKED'))
    assert.ok(audits.some((a) => a.action === 'AUTH_OAUTH_RECOVERY_INITIATED'))
    assert.ok(audits.some((a) => a.action === 'AUTH_OAUTH_RECOVERY_FAILED'))
    assert.ok(audits.some((a) => a.action === 'AUTH_OAUTH_RECOVERY_COMPLETED'))
  } finally {
    resetDatabaseClient()
    oauth.resetOAuthStateForTesting()
  }
})
