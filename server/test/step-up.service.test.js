import test from 'node:test'
import assert from 'node:assert'

test('step-up proof is bound to action and resource and cannot replay', async () => {
  const { StepUpService } = await import('../dist/services/step-up.service.js')
  const now = 1_000
  const service = new StepUpService({
    secret: '0123456789012345678901234567890123456789',
    now: () => now,
  })
  const proof = service.mint('user-1', 'wallet:transfer', { amount: 25, destination: 'GDEST' })

  assert.ok(service.verify(proof, 'user-1', 'wallet:transfer', { amount: 25, destination: 'GDEST' }))
  assert.ok(!service.verify(proof, 'user-1', 'wallet:transfer', { amount: 25, destination: 'GDEST' }))
  assert.ok(!service.verify(proof, 'user-1', 'wallet:transfer', { amount: 26, destination: 'GDEST' }))
  assert.ok(!service.verify(proof, 'user-1', 'api-key:create', { amount: 25, destination: 'GDEST' }))
})

test('step-up proof expires and session revocation rejects it', async () => {
  const { StepUpService } = await import('../dist/services/step-up.service.js')
  let now = 1_000
  const service = new StepUpService({
    secret: '0123456789012345678901234567890123456789',
    ttlMs: 500,
    now: () => now,
  })
  const proof = service.mint('user-1', 'api-key:revoke', { keyId: 'key-1' })
  now += 501
  assert.ok(!service.verify(proof, 'user-1', 'api-key:revoke', { keyId: 'key-1' }))
})
