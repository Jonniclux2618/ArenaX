import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "check_route_inventory.py"
spec = importlib.util.spec_from_file_location("check_route_inventory", SCRIPT)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)


class RouteInventoryFixtureTests(unittest.TestCase):
    def owner_map(self, entries=None, allowlist=None):
        tmp = tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False)
        json.dump({"owners": entries or {}, "cross_service_read_allowlist": allowlist or []}, tmp)
        tmp.close()
        return mod.load_owner_map(Path(tmp.name))

    def test_unowned_route_is_rejected_with_owner_map_context(self):
        owner_map = self.owner_map({"GET /owned": {"owner": "catalog"}})
        problems = mod.validate([("GET", "/orphan", "fixture/routes.ts")], owner_map)
        self.assertTrue(any("UNOWNED ROUTE: GET /orphan" in p for p in problems))
        self.assertTrue(any("no owner-map entry" in p for p in problems))

    def test_duplicate_owner_map_entry_is_rejected(self):
        owner_map = {"owners": {
            "GET /match": {"owner": "match"},
        }, "allowlist": []}
        # Reproduce a single route independently mounted by both live registries.
        problems = mod.validate([
            ("GET", "/match", "server/src/routes/match.routes.ts"),
            ("GET", "/match", "backend/src/main.rs"),
        ], owner_map)
        self.assertTrue(any("DUPLICATE ROUTE REGISTRATION" in p for p in problems))

    def test_cross_service_read_allowlist_is_explicit(self):
        owner_map = self.owner_map({}, ["GET /read-projection"])
        self.assertEqual(mod.validate([("GET", "/read-projection", "backend/src/http/users.rs")], owner_map), [])

    def test_malformed_owner_map_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "owners.json"
            path.write_text('{"owners": [], "cross_service_read_allowlist": []}', encoding="utf-8")
            with self.assertRaises(ValueError):
                mod.load_owner_map(path)

    def test_duplicate_owner_definitions_are_reported(self):
        # Owner-map entries are keyed exactly by "METHOD /path"; duplicate key names
        # are invalid JSON semantics and must be rejected rather than silently overwritten.
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "owners.json"
            path.write_text('{"owners":{"GET /match":{"owner":"match"},"GET /match":{"owner":"projection"}},"cross_service_read_allowlist":[]}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "duplicate owner-map key.*GET /match"):
                mod.load_owner_map(path)


if __name__ == "__main__":
    unittest.main()
