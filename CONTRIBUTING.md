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

The workspace suite includes the Node boundary guard (`hub-rs/crates/steve-hub/tests/node_boundary.rs`): production code outside `steve-node` may start processes or harnesses, list directories, or open, write, create or remove files and directories, only at the call sites its table allows, each with a reason; a new one fails until it goes through the Node or is listed. Imports count too, so a braced, renamed or glob `use` of a listed item cannot hide a call. It is a text scan, so it sees where harness handles are made and imported, not every place one is passed to.

The full Rust test suite also requires `zsh` at `/bin/zsh` for the existing source-release fixtures (on Debian/Ubuntu, install the `zsh` package). The boundary scripts require Python 3.11+. macOS keychain integration tests are explicitly opt-in and create temporary signing resources; ordinary development must not use production credentials. The TypeScript `hub/` workspace is retained for compatibility; the default managed backend is Rust.

CI (`.github/workflows/ci.yml`) runs on every pull request (Linux) and every push to `main` (Linux and macOS). It runs the Rust tests with [cargo-nextest](https://nexte.st) instead, one process per test: `cargo nextest run --manifest-path hub-rs/Cargo.toml --workspace --profile ci --retries 2`, then `cargo test --doc`. A test that passes only on a retry does not fail the run, but the job summary lists it (`scripts/nextest-summary.py`); fix it rather than relying on the retry. The release workflow, on the tag, only builds, packages and publishes.

- **No process documents.** Design notes, plans, proposals, specs, reviews, prototypes and runbooks are the maintainers' own thinking and are kept outside the repository. What a user or operator needs goes into the README, `docs/deployment.md` or the release notes; what a reader of the code needs goes into its comments. `python3 scripts/check-public-boundary.py --strict` (run by CI) rejects any other Markdown file, and anything under `docs/` other than `deployment.md`, `releases/` and `assets/`; test fixtures are exempt.

## Changes and releases

A version is chosen when it is released, never ahead on a branch:

- **Release notes are fragments.** A change adds `docs/releases/unreleased/<item>.md` (its item, such as `ST-38.md`, or its branch name): what changes for the user, in a few sentences or bullets, with no version and no title. Do not write `docs/releases/X.Y.Z.md` on a branch; `python3 ops/cut-release.py lint` checks this.
- **Releasing:** after the change is on `main`, run `python3 ops/cut-release.py cut`. It takes `main` as it is on `origin`, gives it the next free version (one more than the highest `vX.Y.Z` tag), joins the fragments into `docs/releases/X.Y.Z.md`, and pushes the commit and the annotated tag together. If someone else releases first, it starts again with the next number. `--dry-run` shows the notes without releasing; `--minor` bumps the minor version.
- **Only a tested `main` is tagged.** `cut` asks GitHub (with `gh`) for the CI run on the commit it is about to release, waits while it runs (`--ci-wait`, 45 minutes by default), and refuses when it failed or there is none. A failure that was only a flake does not need a fix or a new version: `gh run rerun <run id> --failed`, then `cut` again. `--skip-ci` releases without the check, on purpose. If the Release build on the tag fails on a flake, rerun it the same way on the same tag; nothing is published until every target is built.
- **Database migrations** take their number when they are merged: the next free one in `hub-rs/crates/steve-store/migrations`, with a line appended to `LOCK`.
