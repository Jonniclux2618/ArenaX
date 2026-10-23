# Contract role administration

`access-control` and `auth-gateway` each maintain their own administrator and
role storage. Their role identifiers are contract-local: access-control uses
the `u32` constants in its crate, while auth-gateway uses the `Role` enum.
Consumers must not compare or translate those identifiers implicitly.

Within either contract, the current administrator is the sole `Admin` and has
all non-empty roles for authorization checks. Other roles are flat grants and
do not confer permission to administer roles. Only the current administrator
can grant or revoke non-admin roles. `Admin` cannot be granted, delegated, or
revoked as an ordinary role; it changes only through `transfer_admin`, which
requires authorization from both the outgoing and incoming administrators.
Self-transfer is rejected, and no operation can remove the current sole
administrator without completing a handoff.

An account holding a non-admin access-control role may delegate that role
temporarily, with or without a usage limit. Delegation does not create a direct
role grant and stops authorizing immediately when the delegator's role is
revoked; admin delegation is prohibited.

The two contracts are not automatically synchronized: a role change in one
does not mutate the other. A downstream contract must query the contract that
owns the role it relies on for each authorization decision, rather than cache
grants locally. Access-control permission-result caches are revision-tagged,
so role, permission, and administrator changes invalidate earlier results.
Administrators should use the same trusted administrator address and perform
both contract updates in the same transaction when both role systems are in
use.
