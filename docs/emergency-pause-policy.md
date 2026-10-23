# Emergency Pause Policy (Without Permanent Lockout)

This document formalizes the emergency pause policy and user-fund protection invariants for ArenaX contracts, resolving [#57](https://github.com/Jonniclux2618/ArenaX/issues/57).

## 1. Objectives

- **Bounded Response**: Provide fast-response containment of active exploits, smart contract vulnerabilities, or oracle report anomalies without requiring full governance rounds.
- **Narrow Pause Authority**: Restrict pause capabilities to dedicated `Guardian` roles separate from full protocol admin rights where possible.
- **Anti-Lockout Guarantee**: Ensure that emergency stops cannot indefinitely lock user funds or permanently freeze protocol state.
- **Guaranteed User Fund Exit**: Guarantee that user capital can be withdrawn (`emergency_withdraw` / refunds) even when risk-bearing operations (new deposits, prize settlements) are frozen.

---

## 2. Roles and Permissions

| Role | Responsibilities | Permissions |
|------|------------------|-------------|
| **Guardian** | Rapid responder for security incidents | Can trigger `emergency_pause` with a bounded duration; cannot withdraw funds or alter protocol logic. |
| **Admin** | Protocol governor & recovery coordinator | Can configure guardians, unpause early after post-incident review, and manage contracts. |
| **Resolver / Oracle** | Game outcome resolution | Resolves matches and disputes; blocked from issuing winner payouts while emergency pause is active. |
| **Users / Depositors** | Players & tournament participants | Always retain access to `emergency_withdraw` to exit deposited principal during active pause. |

---

## 3. Scope of Affected Operations

### Risk-Bearing Operations (Blocked During Pause)
- **Escrow Creation (`create_escrow`)**: Prevents initializing new vulnerable pools during an ongoing incident.
- **Fund Ingress (`deposit`)**: Halts user deposits to prevent users from sending funds into a compromised contract.
- **Prize Settlement (`resolve(winner)`)**: Halts automated or resolver-driven prize release to prevent exploiters or corrupted oracle data from draining tournament pools.
- **Governance Executions (`execute_upgrade`)**: Upgrade manager halts execution of pending proposals while paused.

### Preserved Operations (Always Available)
- **User Principal Exit (`emergency_withdraw`)**: Any participant can unilaterally reclaim their deposited amount from an active or disputed escrow.
- **Resolver Emergency Refunds (`resolve(None)`)**: Resolvers or administrators can mass-refund participants to return funds to users.
- **State Read Queries**: All getters and inspection entrypoints remain fully operational.

---

## 4. Bounded Duration and Expiry

To eliminate the danger of permanent lockout:
1. Every pause invocation requires a `duration_seconds` argument, capped at `MAX_PAUSE_DURATION` (7 days / 604,800 seconds).
2. The contract evaluates active pause dynamically against the current ledger timestamp:
   ```rust
   if state.paused && env.ledger().timestamp() < state.expires_at {
       // paused
   } else {
       // active (automatically expired)
   }
   ```
3. Once the duration expires, the contract resumes normal operation automatically without requiring admin transaction execution.

---

## 5. Reviewed Recovery Path

- Early resumption prior to natural expiration requires authenticated admin approval (`unpause(admin)`).
- Resumption triggers public on-chain event logging (`UNPAUSED`) to provide transparency to off-chain indexers and participants.
