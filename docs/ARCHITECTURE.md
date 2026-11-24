# Architecture

This document describes the service boundaries, route ownership, and the
canonical way to keep the route table in sync with the live registries.

## Service boundaries

- **API server** — exposes the public HTTP surface and delegates to domain
  services. It owns the routes listed under *API server* below.
- **Worker server** — processes asynchronous jobs and exposes an internal
  control surface. It owns the routes listed under *Worker server* below.
- **Rust backend** — canonical Rust HTTP registry mounted by
  `backend/src/main.rs` and handler modules under `backend/src/http/`.
- **TypeScript server** — canonical Express registry mounted by
  `server/src/routes/index.ts` and route modules under
  `server/src/routes/`.

## Route ownership and inventory

`docs/route-owner-map.json` is the canonical owner map for public routes. It
must assign every live Rust or Express route to exactly one owner. A route that
is intentionally exposed as a cross-service read projection may instead be
listed in `cross_service_read_allowlist` as an explicit, reviewed exception.

After adding or changing a route:

1. Add or update the route registration at the canonical server boundary.
2. Run `python3 scripts/check_route_inventory.py` to identify unowned or
   duplicated routes.
3. Add exactly one owner entry for the route in `docs/route-owner-map.json`.
   Use `cross_service_read_allowlist` only for a read-only cross-service
   projection, and document why in the owner entry's `type` field.
4. Run `python3 -m unittest scripts/tests/test_check_route_inventory.py`.

The CI route-inventory job fails closed for duplicate route registrations,
duplicate owner-map keys, unowned routes, and malformed owner maps.

## Route ownership

The route and owner sections in this document are **generated** from the live
route registrations at the canonical service boundary. Do not hand-edit the
generated tables; edit the route registrations instead and regenerate.

Each generated entry records:

- HTTP **method**
- **path**
- **owning server**
- whether the route is **read-only** or **mutating** (where that distinction is
  declared on the registration)
- a link to the corresponding **OpenAPI operation** for the route family

### Regeneration

Regenerate the route and owner sections from the live registries with:

```sh
docs:generate-routes
```

Output is deterministic: routes are sorted by owning server, then path, then
method, so repeated runs produce identical output.

### CI freshness check

CI runs the same generator in check mode. If the committed route and owner
sections differ from what the live registries produce, the check fails and
rejects the change as stale. Regenerate locally and commit the result before
opening a pull request.

## API server

<!-- BEGIN GENERATED ROUTES: api-server -->
<!-- END GENERATED ROUTES: api-server -->

## Worker server

<!-- BEGIN GENERATED ROUTES: worker-server -->
<!-- END GENERATED ROUTES: worker-server -->
