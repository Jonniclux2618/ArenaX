import test from 'node:test'
import assert from 'node:assert'
import fs from 'node:fs'

test('TypeScript enforcement uses the shared authorization fixture', async () => {
  const { authorizationPolicy, evaluateAuthorizationPolicy } = await import('../dist/config/authorization-policy.js')
  const diskPolicy = JSON.parse(fs.readFileSync(new URL('./fixtures/authorization-policy.json', import.meta.url), 'utf8'))

  assert.deepStrictEqual(authorizationPolicy.map((row) => row.operation), diskPolicy.operations.map((row) => row.operation))
  assert.strictEqual(evaluateAuthorizationPolicy('USER', 'user', 'write', 'own').allowed, false)
  assert.strictEqual(evaluateAuthorizationPolicy('ADMIN', 'user', 'delete', 'any').allowed, true)
})
