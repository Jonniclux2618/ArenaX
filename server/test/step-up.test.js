import test from 'node:test'
import assert from 'node:assert'

test('step-up fingerprints bind the exact action and operation details', async () => {
  const { operationFingerprint } = await import('../dist/middleware/step-up.middleware.js')
  const action = 'wallet.transfer'
  const operation = { method: 'POST', path: '/api/v1/wallet/transfer', body: { amount: '25', toUserId: 'user-a' } }
  const fingerprint = operationFingerprint(action, operation)

  assert.strictEqual(
    fingerprint,
    operationFingerprint(action, { body: { toUserId: 'user-a', amount: '25' }, path: operation.path, method: operation.method })
  )
  assert.notStrictEqual(fingerprint, operationFingerprint('api_key.create', operation))
  assert.notStrictEqual(
    fingerprint,
    operationFingerprint(action, { ...operation, body: { ...operation.body, amount: '26' } })
  )
  assert.notStrictEqual(
    fingerprint,
    operationFingerprint(action, { ...operation, body: { ...operation.body, toUserId: 'user-b' } })
  )
})
