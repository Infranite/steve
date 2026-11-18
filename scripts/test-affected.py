#!/usr/bin/env python3
"""A Cargo.lock change touches only the workspace crates that use a package version it added."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("affected", Path(__file__).with_name("affected.py"))
affected = importlib.util.module_from_spec(spec)
spec.loader.exec_module(affected)

REG = '"registry+https://github.com/rust-lang/crates.io-index"'


def lock(*packages):
    """Each package: (name, version, dependencies, registry?)."""
    out = ["version = 4\n"]
    for name, version, deps, registry in packages:
        out.append(f'[[package]]\nname = "{name}"\nversion = "{version}"\n')
        if registry:
            out.append(f"source = {REG}\n")
        if deps:
            out.append("dependencies = [\n" + "".join(f' "{d}",\n' for d in deps) + "]\n")
    return "\n".join(out)


MEMBERS = {"app", "core", "store"}
BASE = [("app", "0.1.0", ["core"], False), ("core", "0.1.0", ["store", "chrono"], False),
        ("store", "0.1.0", ["libc"], False), ("chrono", "0.4.0", ["link"], True),
        ("link", "0.2.0", [], True), ("libc", "0.2.0", [], True)]


class LockTouched(unittest.TestCase):
    def test_same_lock_touches_nothing(self):
        self.assertEqual(affected.lock_touched(lock(*BASE), lock(*BASE), MEMBERS), set())

    def test_a_new_dependency_touches_only_its_users(self):
        new = [p for p in BASE if p[0] != "store"] + [("store", "0.1.0", ["libc", "metrics"], False),
                                                       ("metrics", "0.24.0", [], True)]
        self.assertEqual(affected.lock_touched(lock(*BASE), lock(*new), MEMBERS), {"store"})

    def test_a_new_version_deep_in_the_graph_reaches_the_members_above_it(self):
        new = [p for p in BASE if p[0] not in ("link",)] + [("link", "0.3.0", [], True)]
        self.assertEqual(affected.lock_touched(lock(*BASE), lock(*new), MEMBERS), {"core"})

    def test_naming_a_dependency_with_its_version_is_not_a_change(self):
        # a second `link` elsewhere makes the lock name chrono's as `link 0.2.0`; chrono did not change
        new = [p for p in BASE if p[0] not in ("chrono", "store")] + [
            ("chrono", "0.4.0", ["link 0.2.0"], True), ("store", "0.1.0", ["libc", "link 0.3.0"], False),
            ("link", "0.3.0", [], True)]
        self.assertEqual(affected.lock_touched(lock(*BASE), lock(*new), MEMBERS), {"store"})

    def test_a_versioned_reference_follows_only_that_version(self):
        base = BASE + [("link", "0.3.0", [], True)]
        new = [p for p in base if p[0] != "link" or p[1] != "0.3.0"] + [("link", "0.3.0", ["libc"], True),
                                                                      ("libc", "0.3.0", [], True)]
        new = [p for p in new if p[0] not in ("chrono", "store")] + [
            ("chrono", "0.4.0", ["link 0.2.0"], True), ("store", "0.1.0", ["libc 0.2.0"], False)]
        self.assertEqual(affected.lock_touched(lock(*base), lock(*new), MEMBERS), set())

    def test_a_member_does_not_spread_through_the_lock(self):
        # the members above `store` are found from the manifests, with dev-dependencies, not here
        new = [p for p in BASE if p[0] != "libc"] + [("libc", "0.2.1", [], True)]
        self.assertEqual(affected.lock_touched(lock(*BASE), lock(*new), MEMBERS), {"store"})

    def test_no_old_lock_touches_every_member(self):
        self.assertEqual(affected.lock_touched("", lock(*BASE), MEMBERS), MEMBERS)


if __name__ == "__main__":
    unittest.main()
