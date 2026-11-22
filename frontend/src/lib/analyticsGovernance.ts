"use client";

import type { AnalyticsEventName } from "@/types/analytics";
import { ALLOWED_EVENT_NAMES } from "@/types/analytics";

export interface SamplingConfig {
  /** Sample rate per event name (0–1). Defaults to 1 (100%). */
  rates: Partial<Record<AnalyticsEventName, number>>;
  /** Global fallback rate when an event has no specific entry. */
  defaultRate: number;
}

const DEFAULT_CONFIG: SamplingConfig = {
  rates: {},
  defaultRate: 1,
};

let _config: SamplingConfig = { ...DEFAULT_CONFIG };

/**
 * Configure sampling rates for analytics events.
 * A rate of 0.5 means only 50% of events of that type are sent.
 */
export function configureSampling(config: Partial<SamplingConfig>): void {
  _config = {
    ...DEFAULT_CONFIG,
    ...config,
    rates: { ...DEFAULT_CONFIG.rates, ...config.rates },
  };
}

/**
 * Check whether an event should be sampled in (sent) based on configured rates.
 * Returns true if the event passes sampling, false if it should be dropped.
 */
export function shouldSample(eventName: AnalyticsEventName): boolean {
  const rate = _config.rates[eventName] ?? _config.defaultRate;
  if (rate >= 1) return true;
  if (rate <= 0) return false;
  return Math.random() < rate;
}

/**
 * Validate that an event name is in the allowed list.
 * Returns true if valid, false otherwise.
 */
export function isValidEventName(name: string): name is AnalyticsEventName {
  return (ALLOWED_EVENT_NAMES as readonly string[]).includes(name);
}

/**
 * Validate an analytics payload has required fields and a known event name.
 * Returns an object with isValid and optional error message.
 */
export function validatePayload(payload: {
  event?: string;
  timestamp?: number;
  sessionId?: string;
}): { isValid: boolean; error?: string } {
  if (!payload.event) {
    return { isValid: false, error: "Missing event name" };
  }
  if (!isValidEventName(payload.event)) {
    return { isValid: false, error: `Unknown event: ${payload.event}` };
  }
  if (typeof payload.timestamp !== "number" || payload.timestamp <= 0) {
    return { isValid: false, error: "Invalid timestamp" };
  }
  if (!payload.sessionId) {
    return { isValid: false, error: "Missing sessionId" };
  }
  return { isValid: true };
}

export interface DataSetContract {
  name: string;
  owner: string;
  schema: Record<string, "string" | "number" | "boolean" | "date" | "object">;
  approvedPurpose: string;
  accessTier: "PUBLIC" | "INTERNAL" | "CONFIDENTIAL" | "RESTRICTED";
  retention: string;
  deIdentificationRule: string;
  approvedFields?: string[];
  approvedBy?: string;
}

const SENSITIVE_FIELD =
  /(?:^|[_-])(email|password|secret|privatekey|walletaddress|userid|phone|ipaddress|token)(?:$|[_-])/i;

export function validateDatasetContract(contract: DataSetContract): { valid: boolean; errors: string[] } {
  const errors: string[] = [];
  if (!contract.owner) errors.push("Missing dataset owner");
  if (!contract.approvedPurpose) errors.push("Missing approved purpose");
  if (!contract.retention) errors.push("Missing retention rule");
  if (!contract.deIdentificationRule) errors.push("Missing de-identification rule");
  const approved = new Set(contract.approvedFields ?? []);
  for (const field of Object.keys(contract.schema)) {
    if (SENSITIVE_FIELD.test(field) && !approved.has(field)) {
      errors.push(`Sensitive field ${field} lacks approval metadata`);
    }
  }
  if (Object.keys(contract.schema).some((field) => SENSITIVE_FIELD.test(field)) && !contract.approvedBy) {
    errors.push("Sensitive fields require approval metadata");
  }
  return { valid: errors.length === 0, errors };
}

export function assertDatasetContract(contract: DataSetContract): DataSetContract {
  const result = validateDatasetContract(contract);
  if (!result.valid) throw new Error(result.errors.join("; "));
  return contract;
}
