# Executable Authorization Policy

`security/authorization-policy.json` is the shared source for privileged
backend path roles and representative TypeScript role-permission decisions.
Each operation names its principal, resource, action, and enforcement mapping.
The Rust authentication middleware loads the backend mappings at runtime;
Rust and TypeScript tests consume the same file and check both positive and
negative decisions. Resource ownership and object-level checks remain in their
owning handlers and services; the shared file does not create duplicate route
registration.

When changing a protected operation, update this matrix and its service
enforcement together. Policies deny by default; do not infer ownership from a
request-supplied user id.
