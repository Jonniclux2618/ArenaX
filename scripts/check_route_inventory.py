#!/usr/bin/env python3
"""Check Rust and TypeScript route registrations against the canonical owner map."""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OWNER_MAP = ROOT / "docs" / "route-owner-map.json"


def normalize_path(path: str) -> str:
    path = path.strip().strip("\"").strip("'")
    if not path.startswith("/"):
        path = "/" + path
    return re.sub(r"/{2,}", "/", path).rstrip("/") or "/"


def extract_typescript(root: Path) -> list[tuple[str, str, str]]:
    mounts: dict[str, str] = {}
    index = (root / "server" / "src" / "routes" / "index.ts").read_text(encoding="utf-8")
    mount_re = re.compile(r"mountVersioned\(['\"]([^'\"]+)['\"],\s*(\w+)\)")
    direct_re = re.compile(r"router\.use\(['\"]([^'\"]+)['\"],\s*(\w+)\)")
    for prefix, name in mount_re.findall(index):
        mounts[name.lower()] = prefix
    for prefix, name in direct_re.findall(index):
        mounts[name.lower()] = prefix

    found = []
    route_re = re.compile(r"router\.(get|post|put|patch|delete)\(['\"]([^'\"]+)['\"]")
    for file in sorted((root / "server" / "src" / "routes").glob("*.routes.ts")):
        stem = file.stem.lower()
        stem_alias = stem.replace(".routes", "")
        symbol_alias = stem_alias + "routes"
        prefix = mounts.get(stem, mounts.get(stem_alias, mounts.get(symbol_alias, "")))
        if prefix:
            prefix = prefix if prefix.startswith("/") else "/" + prefix
        text = file.read_text(encoding="utf-8")
        for method, path in route_re.findall(text):
            full = normalize_path("/api/v1" + prefix + ("/" + path if not path.startswith("/") else path))
            found.append((method.upper(), full, f"server/src/routes/{file.name}"))
    return found


def extract_rust(root: Path) -> list[tuple[str, str, str]]:
    found = []
    scope_stack: list[str] = []
    main = (root / "backend" / "src" / "main.rs").read_text(encoding="utf-8")

    for match in re.finditer(r"web::scope\(['\"]([^'\"]+)['\"]\)", main):
        scope_stack.append(match.group(1))

    direct_re = re.compile(r"\.route\(['\"]([^'\"]+)['\"],\s*web::(get|post|put|patch|delete)\(")
    for match in direct_re.finditer(main):
        path, method = match.group(1), match.group(2)
        prefix = ""
        start = main.rfind("web::scope", 0, match.start())
        if start >= 0:
            scope_chunk = main[start:match.start()]
            scopes = re.findall(r"web::scope\(['\"]([^'\"]+)['\"]\)", scope_chunk)
            if scopes:
                prefix += "/" + "/".join(scopes)
        full = normalize_path(prefix + path)
        found.append((method.upper(), full, "backend/src/main.rs"))

    for file in sorted((root / "backend" / "src" / "http").glob("*.rs")):
        text = file.read_text(encoding="utf-8")
        prefix_match = re.search(r"web::scope\(['\"]([^'\"]+)['\"]\)", text)
        prefix = prefix_match.group(1) if prefix_match else ""
        for match in re.finditer(r"(?:\.route|cfg\.route)\(['\"]([^'\"]*)['\"],\s*(?:web::)?(get|post|put|patch|delete)\(", text):
            path, method = match.group(1), match.group(2)
            full = normalize_path("/api/" + prefix + path)
            found.append((method.upper(), full, f"backend/src/http/{file.name}"))
    return found


def load_owner_map(path: Path) -> dict:
    text = path.read_text(encoding="utf-8")
    duplicate_keys = []

    def reject_duplicate_keys(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                duplicate_keys.append(key)
            value[key] = item
        return value

    data = json.loads(text, object_pairs_hook=reject_duplicate_keys)
    if duplicate_keys:
        raise ValueError("duplicate owner-map key(s): " + ", ".join(sorted(set(duplicate_keys))))
    owners = data.get("owners")
    allowlist = data.get("cross_service_read_allowlist", [])
    if not isinstance(owners, dict) or not isinstance(allowlist, list):
        raise ValueError("owner map must contain 'owners' and 'cross_service_read_allowlist'")
    return {"owners": owners, "allowlist": allowlist}


def owner_key(owner: object) -> str:
    if isinstance(owner, dict):
        owner = owner.get("owner")
    return str(owner or "")


def validate(registrations: list[tuple[str, str, str]], owner_map: dict) -> list[str]:
    owners = owner_map["owners"]
    allowlist = set(owner_map["allowlist"])
    owners_by_route = defaultdict(list)

    for service_route, owner in owners.items():
        parts = service_route.split(" ", 1)
        if len(parts) != 2:
            raise ValueError(f"invalid owner-map entry: {service_route!r}")
        method, path = parts[0].upper(), normalize_path(parts[1])
        owners_by_route[(method, path)].append(owner_key(owner))

    route_sources = defaultdict(set)
    for method, path, source in registrations:
        route_sources[(method, normalize_path(path))].add(source)

    problems = []
    for route, sources in sorted(route_sources.items(), key=lambda item: (item[0][1], item[0][0])):
        method, path = route
        entry = f"{method} {path}"
        matched_owners = owners_by_route.get(route, [])
        if len(matched_owners) > 1:
            problems.append(f"DUPLICATE OWNER MAP ENTRIES: {entry}; relevant owners: {matched_owners}")
        if not matched_owners:
            if entry in allowlist:
                continue
            problems.append(f"UNOWNED ROUTE: {entry}; no owner-map entry; source={sorted(sources)}")
        elif len(matched_owners) == 1:
            owner = matched_owners[0]
            if not owner:
                problems.append(f"UNOWNED ROUTE: {entry}; owner-map entry has no owner")

    # Detect two live registrations that resolve to the same route but no owner entry.
    for route, sources in sorted(route_sources.items(), key=lambda item: (item[0][1], item[0][0])):
        if len(sources) > 1:
            method, path = route
            problems.append(f"DUPLICATE ROUTE REGISTRATION: {method} {path}; sources={sorted(sources)}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixture-root", type=Path)
    parser.add_argument("--owner-map", type=Path)
    args = parser.parse_args()
    root = args.fixture_root.resolve() if args.fixture_root else ROOT
    owner_path = args.owner_map.resolve() if args.owner_map else OWNER_MAP

    try:
        owner_map = load_owner_map(owner_path)
        registrations = extract_typescript(root) + extract_rust(root)
        problems = validate(registrations, owner_map)
    except (OSError, json.JSONDecodeError, ValueError) as exc:
        print(f"ROUTE INVENTORY CHECK FAILED: {exc}", file=sys.stderr)
        return 1

    if problems:
        print("ROUTE INVENTORY CHECK FAILED", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1

    print(f"Route inventory check passed: {len(registrations)} registered routes checked")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
