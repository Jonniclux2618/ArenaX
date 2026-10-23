# Upgrade Manager

Governed contract-upgrade lifecycle for ArenaX Soroban contracts, resolving [#56](https://github.com/Jonniclux2618/ArenaX/issues/56) and [#969](https://github.com/Arenax-gaming/ArenaX/issues/969).

## Architecture & Responsibilities

The ArenaX ecosystem separates time-locked governance responsibilities into clear layers:
- **`upgrade-manager/` (This Contract)**: The single supported proposal-to-execution state machine for all contract upgrades (both self and managed contracts). Enforces fixed payload identity, dual-phase validation, governor voting thresholds, on-chain delay, cancellation, and expiry windows.
- **`time-lock/`**: Generic operation queue for timelocked administrative actions and parameter adjustments across general, treasury, and emergency categories.
- **`upgrade-system/`**: Reference single-contract migration prototype; superseded by this unified cross-contract `upgrade-manager`.

## Supported Proposal-to-Execution State Machine

```
   [Propose] ───────► Proposed
                         │
                    (Validate)
                         ▼
                     Validated
                         │
                      (Vote)
                         ▼
                     Approved
                         │
             (Timelock Delay Elapsed)
                         ▼
                     Executable
                         │
                     (Execute)
                         ▼
                      Executed

  * Note: Any non-executed proposal can transition to:
    - Canceled: via `cancel_proposal` (by Proposer, Admin, or Governor)
    - Expired: if not executed within `DEFAULT_GRACE_PERIOD` (7 days after timelock_end)
```

### Lifecycle Steps:
1. **Propose** — Call `propose_upgrade` (or `propose_upgrade_with_manifest`). The candidate payload WASM hash is permanently bound at proposal creation and cannot be substituted.
2. **Validate** — Governor verifies compatibility and emits `UpgradeValidated`. Required prior to execution.
3. **Vote** — Governors vote until `approval_threshold` is reached (`ProposalStatus::Approved`).
4. **Delay** — On-chain timelock delay must elapse (`ledger.timestamp >= timelock_end`).
5. **Execute** — Call `execute_upgrade` or `execute_upgrade_guarded(proposal_id, expected_wasm_hash)`. Ensures payload integrity, enforces that the proposal is not canceled, not expired, validated, approved, and past delay.
6. **Cancel** — Proposer, admin, or governors can cancel a proposal via `cancel_proposal` before execution.
7. **Expiry** — Bounded execution grace period (`DEFAULT_GRACE_PERIOD = 7 days`) prevents stale or abandoned proposals from being executed in the distant future.
8. **Rollback** — In emergencies, `rollback_upgrade` restores the previously verified implementation.
9. **Emergency Pause** — Bounded pause duration (`MAX_PAUSE_DURATION = 7 days`) halts state mutations during incidents without causing permanent lockouts.

## Acceptance Criteria Mapping (#56)

- **Fixed Payload Identity**: Payload WASM hash is recorded in `UpgradeProposal` upon creation. `execute_upgrade_guarded` asserts identity matches expected payload, preventing substitution.
- **On-Chain Approval, Delay, Cancellation, and Expiry**:
  - Approval: `votes.len() >= approval_threshold` verified in `execute_upgrade`.
  - Delay: `ledger.timestamp >= proposal.timelock_end` strictly enforced.
  - Cancellation: `cancel_proposal` moves proposal to `Canceled` state; execution immediately reverts.
  - Expiry: Proposals expire after `timelock_end + DEFAULT_GRACE_PERIOD`; execution reverts with `ProposalExpired`.
- **Independent Monitoring**: Detailed events emitted via `arenax_events::upgrade_manager`:
  - `UpgradeProposed`, `UpgradeValidated`, `UpgradeVoted`, `UpgradeExecuted`, `UpgradeRolledBack`, `UpgradeCanceled`.
