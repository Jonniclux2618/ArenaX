import fs from 'node:fs'
import path from 'node:path'
import { hasPermission, Permission } from '../middleware/rbac.middleware'

export interface AuthorizationPolicyRow {
  operation: string
  principal: string
  resource: string
  action: string
  permission: Permission
  owner: 'public' | 'own' | 'any'
  decision: 'allow' | 'deny'
}

const policyPath = path.resolve(__dirname, '../../test/fixtures/authorization-policy.json')
const policy = JSON.parse(fs.readFileSync(policyPath, 'utf8')) as { operations: AuthorizationPolicyRow[] }

export const authorizationPolicy: readonly AuthorizationPolicyRow[] = policy.operations

export function evaluateAuthorizationPolicy(
  principal: string,
  resource: string,
  action: string,
  owner: 'public' | 'own' | 'any',
): { allowed: boolean; row?: AuthorizationPolicyRow } {
  const row = authorizationPolicy.find((candidate) =>
    candidate.principal === principal &&
    candidate.resource === resource &&
    candidate.action === action &&
    candidate.owner === owner
  )
  if (!row) return { allowed: false }
  return {
    allowed: row.decision === 'allow' && hasPermission(principal, row.permission),
    row,
  }
}
