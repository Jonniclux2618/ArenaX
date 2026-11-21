export type SecretRequirement = 'required' | 'optional'

export interface SecretConsumer {
  variable: string
  service: string
  purpose: string
  rotationOwner: string
  requirement: SecretRequirement
  rotationPair?: string
}

export const SECRET_INVENTORY: readonly SecretConsumer[] = [
  { variable: 'DATABASE_URL', service: 'database', purpose: 'PostgreSQL connection', rotationOwner: 'Platform', requirement: 'required' },
  { variable: 'JWT_SECRET', service: 'auth', purpose: 'HS256 access token signing', rotationOwner: 'Identity', requirement: 'required', rotationPair: 'JWT_SECRET_PREVIOUS' },
  { variable: 'JWT_PRIVATE_KEY', service: 'auth', purpose: 'RS256 signing', rotationOwner: 'Identity', requirement: 'optional' },
  { variable: 'JWT_PUBLIC_KEY', service: 'auth', purpose: 'RS256 verification', rotationOwner: 'Identity', requirement: 'optional' },
  { variable: 'ENCRYPTION_SECRET', service: 'wallet', purpose: 'Legacy wallet encryption', rotationOwner: 'Security', requirement: 'optional' },
  { variable: 'WALLET_MASTER_KEYS', service: 'wallet', purpose: 'Versioned wallet key ring', rotationOwner: 'Security', requirement: 'optional' },
  { variable: 'SENSITIVE_OPERATION_SECRET', service: 'step-up', purpose: 'Action-bound step-up proof signing', rotationOwner: 'Security', requirement: 'optional', rotationPair: 'SENSITIVE_OPERATION_SECRET_PREVIOUS' },
  { variable: 'SENTRY_DSN', service: 'telemetry', purpose: 'Error reporting', rotationOwner: 'Observability', requirement: 'optional' },
  { variable: 'EMAIL_PASSWORD', service: 'email', purpose: 'SMTP authentication', rotationOwner: 'Platform', requirement: 'optional' },
  { variable: 'GOOGLE_CLIENT_SECRET', service: 'oauth', purpose: 'Google OAuth', rotationOwner: 'Identity', requirement: 'optional' },
  { variable: 'DISCORD_CLIENT_SECRET', service: 'oauth', purpose: 'Discord OAuth', rotationOwner: 'Identity', requirement: 'optional' },
  { variable: 'TWITCH_CLIENT_SECRET', service: 'oauth', purpose: 'Twitch OAuth', rotationOwner: 'Identity', requirement: 'optional' },
  { variable: 'ADMIN_SECRET_KEY', service: 'stellar', purpose: 'Admin Stellar account signing', rotationOwner: 'Payments', requirement: 'optional' },
  { variable: 'FEE_PAYER_SECRET_KEY', service: 'stellar', purpose: 'Transaction fee payment', rotationOwner: 'Payments', requirement: 'optional' },
  { variable: 'PAYSTACK_SECRET_KEY', service: 'payments', purpose: 'Paystack API and webhook authentication', rotationOwner: 'Payments', requirement: 'optional' },
  { variable: 'FLUTTERWAVE_SECRET_KEY', service: 'payments', purpose: 'Flutterwave API authentication', rotationOwner: 'Payments', requirement: 'optional' },
  { variable: 'FLUTTERWAVE_WEBHOOK_HASH', service: 'payments', purpose: 'Flutterwave webhook verification', rotationOwner: 'Payments', requirement: 'optional' },
]

export const isSecretVariableName = (name: string): boolean =>
  /(?:SECRET|PASSWORD|PRIVATE_KEY|CREDENTIAL|DSN|SIGNING_KEY|MASTER_KEY)/i.test(name)

export const isPlaceholderSecretValue = (value: string): boolean => {
  const normalized = value.trim().toLowerCase()
  return [
    'changeme',
    'change_me',
    'default_secret_change_in_production',
    'password',
    'secret',
    'supersecretkey',
    'test',
  ].includes(normalized) || /^(replace|your|example|placeholder|xxx|<)/.test(normalized)
}

export interface SecretInventoryResult {
  valid: boolean
  unknownVariables: string[]
  missingRequired: string[]
  placeholders: string[]
  rotationWarnings: string[]
}

export function validateSecretInventory(
  environment: Record<string, string | undefined>,
  approved: readonly SecretConsumer[] = SECRET_INVENTORY,
): SecretInventoryResult {
  const byName = new Map(approved.map((entry) => [entry.variable, entry]))
  const unknownVariables = Object.keys(environment).filter(
    (name) => isSecretVariableName(name) && !byName.has(name) && !name.endsWith('_PREVIOUS') && !name.endsWith('_RETIRE_AT'),
  )
  const missingRequired = approved
    .filter((entry) => entry.requirement === 'required')
    .filter((entry) => !environment[entry.variable]?.trim())
    .map((entry) => entry.variable)
  const placeholders = approved
    .filter((entry) => environment[entry.variable] && isPlaceholderSecretValue(environment[entry.variable]!))
    .map((entry) => entry.variable)
  const rotationWarnings = approved
    .filter((entry) => entry.rotationPair)
    .filter((entry) => {
      const previous = environment[entry.rotationPair!]?.trim()
      const retireAt = `${entry.rotationPair}_RETIRE_AT`
      return Boolean(previous) && !environment[retireAt]?.trim()
    })
    .map((entry) => `${entry.rotationPair} must include a retirement timestamp`)

  return {
    valid: unknownVariables.length === 0 && missingRequired.length === 0 && placeholders.length === 0 && rotationWarnings.length === 0,
    unknownVariables,
    missingRequired,
    placeholders,
    rotationWarnings,
  }
}
