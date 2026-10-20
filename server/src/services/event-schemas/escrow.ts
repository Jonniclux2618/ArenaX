import { xdr, scValToNative } from '@stellar/stellar-sdk';
import { EventSchema } from '../event-parser.service.js';

function parseStruct(data: xdr.ScVal, requiredFields: string[] = []): Record<string, unknown> {
    const parsed = scValToNative(data);
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
        throw new Error('Invalid escrow event payload: expected a struct');
    }

    const missingFields = requiredFields.filter(
        (field) => !Object.prototype.hasOwnProperty.call(parsed, field) || parsed[field] == null,
    );
    if (missingFields.length > 0) {
        throw new Error(`Invalid escrow event payload: missing required fields ${missingFields.join(', ')}`);
    }

    return parsed as Record<string, unknown>;
}

const parseDeposit = (data: xdr.ScVal) => parseStruct(data, ['match_id', 'player', 'amount', 'asset']);
const parseRelease = (data: xdr.ScVal) => parseStruct(data, ['match_id', 'winner', 'amount', 'asset']);
const parseRefund = (data: xdr.ScVal) => parseStruct(data, ['match_id', 'player_a', 'player_b', 'amount', 'asset']);
const parseSlash = (data: xdr.ScVal) => parseStruct(data, ['match_id', 'subject', 'amount', 'asset']);
const parseEmergency = (data: xdr.ScVal) => parseStruct(data, ['match_id', 'admin', 'amount', 'asset']);

export const escrowInitV0: EventSchema = { namespace: 'ArenaXEscrow', version: 'v0', eventType: 'INIT', parse: parseStruct };
export const escrowInitV1: EventSchema = { namespace: 'ArenaXEscrow', version: 'v1', eventType: 'INIT', parse: parseStruct };
export const escrowMatchSetV0: EventSchema = { namespace: 'ArenaXEscrow', version: 'v0', eventType: 'MATCH_SET', parse: parseStruct };
export const escrowMatchSetV1: EventSchema = { namespace: 'ArenaXEscrow', version: 'v1', eventType: 'MATCH_SET', parse: parseStruct };
export const escrowIdSetV0: EventSchema = { namespace: 'ArenaXEscrow', version: 'v0', eventType: 'ID_SET', parse: parseStruct };
export const escrowIdSetV1: EventSchema = { namespace: 'ArenaXEscrow', version: 'v1', eventType: 'ID_SET', parse: parseStruct };
export const escrowTreasuryV0: EventSchema = { namespace: 'ArenaXEscrow', version: 'v0', eventType: 'TREASURY', parse: parseStruct };
export const escrowTreasuryV1: EventSchema = { namespace: 'ArenaXEscrow', version: 'v1', eventType: 'TREASURY', parse: parseStruct };
export const escrowDepositV0: EventSchema = { namespace: 'ArenaXEscrow', version: 'v0', eventType: 'DEPOSIT', parse: parseDeposit };
export const escrowDepositV1: EventSchema = { namespace: 'ArenaXEscrow', version: 'v1', eventType: 'DEPOSIT', parse: parseDeposit };
export const escrowLockedV0: EventSchema = { namespace: 'ArenaXEscrow', version: 'v0', eventType: 'LOCKED', parse: parseStruct };
export const escrowLockedV1: EventSchema = { namespace: 'ArenaXEscrow', version: 'v1', eventType: 'LOCKED', parse: parseStruct };
export const escrowReleasedV0: EventSchema = { namespace: 'ArenaXEscrow', version: 'v0', eventType: 'RELEASED', parse: parseRelease };
export const escrowReleasedV1: EventSchema = { namespace: 'ArenaXEscrow', version: 'v1', eventType: 'RELEASED', parse: parseRelease };
export const escrowRefundedV0: EventSchema = { namespace: 'ArenaXEscrow', version: 'v0', eventType: 'REFUNDED', parse: parseRefund };
export const escrowRefundedV1: EventSchema = { namespace: 'ArenaXEscrow', version: 'v1', eventType: 'REFUNDED', parse: parseRefund };
export const escrowSlashedV0: EventSchema = { namespace: 'ArenaXEscrow', version: 'v0', eventType: 'SLASHED', parse: parseSlash };
export const escrowSlashedV1: EventSchema = { namespace: 'ArenaXEscrow', version: 'v1', eventType: 'SLASHED', parse: parseSlash };
export const escrowEmergencyV0: EventSchema = { namespace: 'ArenaXEscrow', version: 'v0', eventType: 'EMERGENCY', parse: parseEmergency };
export const escrowEmergencyV1: EventSchema = { namespace: 'ArenaXEscrow', version: 'v1', eventType: 'EMERGENCY', parse: parseEmergency };
