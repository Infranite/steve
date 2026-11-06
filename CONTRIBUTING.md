# Contributing to Steve

Thanks for helping. Build and run Steve locally as described in the [README](README.md#quick-start), then use the checks and release rules below.

## Development checks

```sh
python3 scripts/test-public-boundary.py
python3 scripts/check-public-boundary.py --strict
python3 ops/test-cut-release.py
python3 ops/cut-release.py lint
cargo test --locked --manifest-path hub-rs/Cargo.toml --workspace
npm run check
npm run test:web
```

The full Rust test suite also requires `zsh` at `/bin/zsh` for the existing source-release fixtures (on Debian/Ubuntu, install the `zsh` package). The boundary scripts require Python 3.11+. macOS keychain integration tests are explicitly opt-in and create temporary signing resources; ordinary development must not use production credentials. The TypeScript `hub/` workspace is retained for compatibility; the default managed backend is Rust.

The release workflow runs the Rust tests with [cargo-nextest](https://nexte.st) instead, one process per test: `cargo nextest run --manifest-path hub-rs/Cargo.toml --workspace --profile ci --retries 2`, then `cargo test --doc`. A test that passes only on a retry does not block the release, but the job summary lists it (`scripts/nextest-summary.py`); fix it rather than relying on the retry.

## Changes and releases

A version is chosen when it is released, never ahead on a branch:

- **Release notes are fragments.** A change adds `docs/releases/unreleased/<item>.md` (its item, such as `ST-38.md`, or its branch name): what changes for the user, in a few sentences or bullets, with no version and no title. Do not write `docs/releases/X.Y.Z.md` on a branch; `python3 ops/cut-release.py lint` checks this.
- **Releasing:** after the change is on `main`, run `python3 ops/cut-release.py cut`. It takes `main` as it is on `origin`, gives it the next free version (one more than the highest `vX.Y.Z` tag), joins the fragments into `docs/releases/X.Y.Z.md`, and pushes the commit and the annotated tag together. If someone else releases first, it starts again with the next number. `--dry-run` shows the notes without releasing; `--minor` bumps the minor version.
- **Database migrations** take their number when they are merged: the next free one in `hub-rs/crates/steve-store/migrations`, with a line appended to `LOCK`.
