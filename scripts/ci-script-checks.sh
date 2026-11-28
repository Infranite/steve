#!/bin/sh
# The boundary and release-script checks CI and release workflows share across platforms:
# in the `scripts` job on Linux, the first macOS Rust shard (.github/workflows/ci.yml),
# and the release build gate (.github/workflows/release.yml). One list, so platforms cannot drift.
set -eu
python3 scripts/check-public-boundary.py
python3 scripts/test-public-boundary.py
python3 scripts/test-package-release.py
python3 packaging/homebrew/test-sync.py
python3 scripts/test-install-nextest.py
python3 scripts/test-affected.py
python3 ops/test-backup-external.py
python3 ops/test-cut-release.py
python3 ops/test-check-migration-lock.py
bash ops/check-migration-lock.sh "$@"
python3 scripts/test-nextest-summary.py
python3 scripts/test-nextest-partitions.py
python3 scripts/test-skip-rust-jobs.py
python3 scripts/test-rerun-unacquired.py
python3 ops/test-rehearse-upgrade.py
