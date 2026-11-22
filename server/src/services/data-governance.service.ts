export type PurgeStatus = 'NOT_RUN' | 'RUNNING' | 'SUCCEEDED' | 'FAILED' | 'SKIPPED';
export type QualitySeverity = 'INFO' | 'WARNING' | 'CRITICAL';

export interface RetentionPolicy {
  dataset: string;
  owner: string;
  eligibilityRule: string;
  expiresAfterDays: number;
  exceptions?: Array<{ reason: string; expiresAt: number }>;
  purgeResultCount: number;
  lastSuccessfulWatermark: number | null;
  status: PurgeStatus;
  retryable: boolean;
  failureReason?: string;
}

export interface DatasetContract {
  name: string;
  owner: string;
  schema: Record<string, 'string' | 'number' | 'boolean' | 'date' | 'object'>;
  approvedPurpose: string;
  accessTier: 'PUBLIC' | 'INTERNAL' | 'CONFIDENTIAL' | 'RESTRICTED';
  retention: string;
  deIdentificationRule: string;
  approvedFields?: string[];
  sensitiveFields?: string[];
  approvedBy?: string;
}

export interface DataAccessGrant {
  dataset: string;
  principal: string;
  grantedBy: string;
  expiresAt: number;
  revokedAt?: number;
}

export interface QualityThreshold {
  eventType: string;
  minVolume: number;
  maxNullRate: number;
  minValue?: number;
  maxValue?: number;
  maxDuplicatesRate: number;
  maxAgeMs: number;
  severity: QualitySeverity;
  numericField?: string;
  expectedSegmentFields?: string[];
}

export interface QualityCheckResult {
  eventType: string;
  passed: boolean;
  severity: QualitySeverity;
  degraded: boolean;
  quarantined: boolean;
  page: boolean;
  failures: string[];
  checkedAt: number;
}

export interface PurgeResult {
  status: Extract<PurgeStatus, 'SUCCEEDED' | 'FAILED' | 'SKIPPED'>;
  dryRun: boolean;
  deletedCount: number;
  watermark: number | null;
  retriable: boolean;
  failureReason?: string;
}

const retentionPolicies = new Map<string, RetentionPolicy>();
const datasetContracts = new Map<string, DatasetContract>();
const accessGrants = new Map<string, DataAccessGrant>();
const qualityThresholds = new Map<string, QualityThreshold>();
const purgeWatermarks = new Map<string, number>();
const purgeCursors = new Map<string, number>();
const quarantinedEvents = new Set<string>();

const sensitiveFieldNames =
  /(?:^|[_-])(email|password|passwordhash|secret|privatekey|walletaddress|userid|user_id|phone|ipaddress|token)(?:$|[_-])/i;

export function registerRetentionPolicy(policy: Omit<RetentionPolicy, 'purgeResultCount' | 'lastSuccessfulWatermark' | 'status' | 'retryable'>): RetentionPolicy {
  const normalized: RetentionPolicy = {
    ...policy,
    exceptions: policy.exceptions ?? [],
    purgeResultCount: 0,
    lastSuccessfulWatermark: purgeWatermarks.get(policy.dataset) ?? null,
    status: 'NOT_RUN',
    retryable: true,
  };
  retentionPolicies.set(policy.dataset, normalized);
  return normalized;
}

export function getRetentionPolicy(dataset: string): RetentionPolicy | undefined {
  return retentionPolicies.get(dataset);
}

export function listRetentionPolicies(): RetentionPolicy[] {
  return Array.from(retentionPolicies.values());
}

export function runRetentionPurge(
  dataset: string,
  eligibleRows: unknown[],
  options: { dryRun?: boolean; limit?: number; now?: number; fail?: boolean } = {}
): PurgeResult {
  const policy = retentionPolicies.get(dataset);
  if (!policy) return { status: 'FAILED', dryRun: !!options.dryRun, deletedCount: 0, watermark: null, retriable: true, failureReason: 'Retention policy not found' };

  const now = options.now ?? Date.now();
  const activeException = (policy.exceptions ?? []).find((exception) => exception.expiresAt > now);
  if (activeException) {
    policy.status = 'SKIPPED';
    policy.retryable = true;
    policy.failureReason = `Scoped retention exception active until ${new Date(activeException.expiresAt).toISOString()}`;
    return { status: 'SKIPPED', dryRun: !!options.dryRun, deletedCount: 0, watermark: policy.lastSuccessfulWatermark, retriable: true, failureReason: policy.failureReason };
  }

  if (options.fail) {
    policy.status = 'FAILED';
    policy.retryable = true;
    policy.failureReason = 'Purge execution failed';
    return { status: 'FAILED', dryRun: !!options.dryRun, deletedCount: 0, watermark: policy.lastSuccessfulWatermark, retriable: true, failureReason: policy.failureReason };
  }

  const limit = Math.max(0, Math.min(options.limit ?? eligibleRows.length, eligibleRows.length));
  if (options.dryRun) {
    return { status: 'SUCCEEDED', dryRun: true, deletedCount: limit, watermark: policy.lastSuccessfulWatermark, retriable: false };
  }

  policy.status = 'RUNNING';
  const cursor = purgeCursors.get(dataset) ?? 0;
  const deletedCount = Math.max(0, Math.min(limit, eligibleRows.length - cursor));
  purgeCursors.set(dataset, cursor + deletedCount);
  policy.purgeResultCount = deletedCount;
  policy.lastSuccessfulWatermark = now;
  policy.status = 'SUCCEEDED';
  policy.failureReason = undefined;
  purgeWatermarks.set(dataset, now);
  return { status: 'SUCCEEDED', dryRun: false, deletedCount, watermark: now, retriable: false };
}

export function publishDatasetContract(contract: DatasetContract): DatasetContract {
  if (!contract.owner || !contract.approvedPurpose || !contract.retention || !contract.deIdentificationRule) {
    throw new Error('Dataset contract requires owner, approved purpose, retention, and de-identification rule');
  }
  const approvedFields = new Set([...(contract.approvedFields ?? []), ...(contract.sensitiveFields ?? [])]);
  const unauthorized = Object.keys(contract.schema).filter(
    (field) => sensitiveFieldNames.test(field) && !approvedFields.has(field)
  );
  if (unauthorized.length > 0) {
    throw new Error(`Unauthorized sensitive fields cannot be published: ${unauthorized.join(', ')}`);
  }
  const hasSensitiveFields = Object.keys(contract.schema).some((field) => sensitiveFieldNames.test(field));
  if (hasSensitiveFields && !contract.approvedBy) {
    throw new Error('Sensitive fields require approval metadata');
  }
  datasetContracts.set(contract.name, contract);
  return contract;
}

export function getDatasetContract(name: string): DatasetContract | undefined {
  return datasetContracts.get(name);
}

export function grantDatasetAccess(
  dataset: string,
  principal: string,
  grantedBy: string,
  ttlMs: number,
  now = Date.now()
): DataAccessGrant {
  if (!datasetContracts.has(dataset)) throw new Error('Dataset contract not found');
  if (ttlMs <= 0) throw new Error('Access grants must be time-bounded');
  const grant: DataAccessGrant = { dataset, principal, grantedBy, expiresAt: now + ttlMs };
  accessGrants.set(`${dataset}:${principal}`, grant);
  return grant;
}

export function revokeDatasetAccess(dataset: string, principal: string, now = Date.now()): boolean {
  const grant = accessGrants.get(`${dataset}:${principal}`);
  if (!grant) return false;
  grant.revokedAt = now;
  return true;
}

export function isDatasetAccessGranted(dataset: string, principal: string, now = Date.now()): boolean {
  const grant = accessGrants.get(`${dataset}:${principal}`);
  return !!grant && grant.revokedAt == null && grant.expiresAt > now;
}

export function registerQualityThreshold(threshold: QualityThreshold): QualityThreshold {
  qualityThresholds.set(threshold.eventType, threshold);
  return threshold;
}

export function evaluateDataQuality(
  eventType: string,
  rows: Record<string, unknown>[],
  now = Date.now()
): QualityCheckResult {
  const threshold = qualityThresholds.get(eventType);
  if (!threshold) {
    quarantinedEvents.add(eventType);
    return { eventType, passed: false, severity: 'CRITICAL', degraded: true, quarantined: true, page: true, failures: ['No quality threshold configured'], checkedAt: now };
  }

  const failures: string[] = [];
  if (rows.length < threshold.minVolume) failures.push(`volume below ${threshold.minVolume}`);
  let nullCount = 0;
  let duplicateCount = 0;
  const keys = new Set<string>();
  let latestTimestamp = 0;

  for (const row of rows) {
    for (const value of Object.values(row)) if (value == null || value === '') nullCount++;
    const key = JSON.stringify(row);
    if (keys.has(key)) duplicateCount++;
    keys.add(key);
    const timestamp = Number(row.timestamp ?? row.createdAt ?? 0);
    if (timestamp > latestTimestamp) latestTimestamp = timestamp;
    if (threshold.numericField) {
      const value = Number(row[threshold.numericField]);
      if (threshold.minValue != null && value < threshold.minValue) failures.push(`value below ${threshold.minValue}`);
      if (threshold.maxValue != null && value > threshold.maxValue) failures.push(`value above ${threshold.maxValue}`);
    }
    for (const segmentField of threshold.expectedSegmentFields ?? []) {
      if (row[segmentField] == null || row[segmentField] === '') {
        failures.push(`missing segment ${segmentField}`);
      }
    }
  }

  const nullRate = rows.length === 0 ? 1 : nullCount / (rows.length * Math.max(1, Object.values(rows[0]).length));
  if (nullRate > threshold.maxNullRate) failures.push(`null rate above ${threshold.maxNullRate}`);
  if (rows.length > 0 && duplicateCount / rows.length > threshold.maxDuplicatesRate) failures.push(`duplicate rate above ${threshold.maxDuplicatesRate}`);
  if (latestTimestamp > 0 && now - latestTimestamp > threshold.maxAgeMs) failures.push(`freshness exceeds ${threshold.maxAgeMs}ms`);

  const passed = failures.length === 0;
  if (passed) quarantinedEvents.delete(eventType);
  else quarantinedEvents.add(eventType);
  return {
    eventType,
    passed,
    severity: threshold.severity,
    degraded: !passed,
    quarantined: !passed,
    page: !passed && threshold.severity === 'CRITICAL',
    failures,
    checkedAt: now,
  };
}

export function isDataQualityQuarantined(eventType: string): boolean {
  return quarantinedEvents.has(eventType);
}
