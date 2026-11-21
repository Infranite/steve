#!/bin/sh
# The boundary and release-script checks CI runs on both platforms: in the `scripts` job on Linux and
# in the first macOS Rust shard (.github/workflows/ci.yml). One list, so the platforms cannot drift.
set -eu
python3 scripts/check-public-boundary.py --strict
python3 scripts/test-public-boundary.py
python3 scripts/test-package-release.py
python3 packaging/homebrew/test-sync.py
python3 scripts/test-install-nextest.py
python3 scripts/test-affected.py
python3 scripts/test_structure_guard.py
python3 scripts/test-check-inventory-growth.py
python3 scripts/test-inventory-import-markers.py
python3 ops/test-schema-gate.py
python3 ops/test-local-release-journal.py
python3 ops/test-migration-probe.py
python3 ops/test-local-release-prune.py
python3 ops/test-prepare-bundle.py
python3 ops/test-steve-service.py
python3 ops/test-upgrade.py
python3 ops/test-cut-release.py
python3 -m py_compile ops/local-release.py ops/test-local-release.py ops/upgrade.py
bash ops/check-migration-lock.sh
python3 scripts/test-nextest-summary.py
python3 scripts/test-nextest-partitions.py
python3 scripts/test-skip-rust-jobs.py
python3 scripts/test-rerun-unacquired.py
