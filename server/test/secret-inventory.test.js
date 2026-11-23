import test from 'node:test'
import assert from 'node:assert'

test('secret inventory rejects undeclared secret-bearing variable names', async () => {
  const { findUnapprovedSecretVariables } = await import('../dist/config/env.js')
  assert.deepStrictEqual(
    findUnapprovedSecretVariables(['JWT_SECRET', 'UNOWNED_API_TOKEN', 'EMAIL_PASSWORD']),
    ['UNOWNED_API_TOKEN']
  )
})
