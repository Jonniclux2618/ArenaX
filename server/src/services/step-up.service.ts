import crypto from 'crypto'
import { Request, Response, NextFunction } from 'express'
import { HttpError } from '../utils/http-error'

export const STEP_UP_HEADER = 'x-step-up-proof'
export const STEP_UP_TTL_MS = 5 * 60 * 1000

interface StepUpPayload {
  jti: string
  userId: string
  action: string
  resourceHash: string
  issuedAt: number
  expiresAt: number
}

export interface StepUpProofOptions {
  ttlMs?: number
  now?: () => number
  secret?: string
}

const stableStringify = (value: unknown): string =>
  JSON.stringify(value, (_key, item) => {
    if (!item || typeof item !== 'object' || Array.isArray(item)) return item
    return Object.keys(item).sort().reduce<Record<string, unknown>>((sorted, key) => {
      sorted[key] = (item as Record<string, unknown>)[key]
      return sorted
    }, {})
  })

export class StepUpService {
  private readonly ttlMs: number
  private readonly now: () => number
  private readonly secret: string
  private readonly used = new Set<string>()
  private readonly revokedSessions = new Set<string>()
  private readonly proofUsers = new Map<string, string>()

  constructor(options: StepUpProofOptions = {}) {
    this.ttlMs = options.ttlMs ?? STEP_UP_TTL_MS
    this.now = options.now ?? Date.now
    this.secret = options.secret ?? process.env.SENSITIVE_OPERATION_SECRET ?? ''
  }

  configureSecret(secret: string): void {
    // Test-only reconfiguration: production uses constructor configuration.
    Object.assign(this, { secret })
  }

  private sign(payload: StepUpPayload): string {
    return crypto
      .createHmac('sha256', this.secret)
      .update(stableStringify(payload))
      .digest('base64url')
  }

  mint(userId: string, action: string, resource: unknown): string {
    if (!this.secret) throw new Error('Step-up proof signing secret is not configured')
    const now = this.now()
    const payload: StepUpPayload = {
      jti: crypto.randomUUID(),
      userId,
      action,
      resourceHash: crypto.createHash('sha256').update(stableStringify(resource)).digest('hex'),
      issuedAt: now,
      expiresAt: now + this.ttlMs,
    }
    this.proofUsers.set(payload.jti, userId)
    return `${Buffer.from(stableStringify(payload)).toString('base64url')}.${this.sign(payload)}`
  }

  verify(proof: string, userId: string, action: string, resource: unknown): boolean {
    if (!proof || !this.secret) return false
    const parts = proof.split('.')
    if (parts.length !== 2) return false
    let payload: StepUpPayload
    try {
      payload = JSON.parse(Buffer.from(parts[0], 'base64url').toString('utf8'))
      const supplied = Buffer.from(parts[1], 'base64url')
      const expected = Buffer.from(this.sign(payload), 'base64url')
      if (supplied.length !== expected.length || !crypto.timingSafeEqual(supplied, expected)) return false
    } catch {
      return false
    }

    const now = this.now()
    if (payload.userId !== userId || payload.action !== action) return false
    if (payload.issuedAt > now || payload.expiresAt < now) return false
    if (this.revokedSessions.has(payload.jti) || this.used.has(payload.jti)) return false
    const resourceHash = crypto.createHash('sha256').update(stableStringify(resource)).digest('hex')
    if (payload.resourceHash !== resourceHash) return false

    this.used.add(payload.jti)
    return true
  }

  revoke(proofOrJti: string): void {
    const jti = proofOrJti.includes('.') ? this.extractJti(proofOrJti) : proofOrJti
    if (jti) this.revokedSessions.add(jti)
  }

  revokeSession(sessionId: string): void {
    this.revokedSessions.add(sessionId)
  }

  revokeAllForUser(userId: string): void {
    for (const [jti, proofUserId] of this.proofUsers) {
      if (proofUserId === userId) this.revokedSessions.add(jti)
    }
  }

  private extractJti(proof: string): string | undefined {
    const first = proof.split('.')[0]
    try {
      return (JSON.parse(Buffer.from(first, 'base64url').toString('utf8')) as StepUpPayload).jti
    } catch {
      return undefined
    }
  }
}

export const defaultStepUpService = new StepUpService()

export const requireStepUp = (action: string) =>
  (req: Request, _res: Response, next: NextFunction): void => {
    const user = req.user
    const proof = req.header(STEP_UP_HEADER)
    if (!user) return next(new HttpError(401, 'Authentication required'))
    if (!proof) return next(new HttpError(401, 'Step-up authentication required'))

    const resource = {
      path: req.params,
      body: req.body,
      action,
    }
    if (!defaultStepUpService.verify(proof, user.id, action, resource)) {
      return next(new HttpError(401, 'Step-up proof is invalid, stale, replayed, or bound to a different action'))
    }
    next()
  }
